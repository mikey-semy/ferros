//! Файловые системные вызовы и таблица дескрипторов процесса (M6d2).
//!
//! # Дескрипторы и «текущий процесс»
//!
//! `open` возвращает **файловый дескриптор** (fd) — небольшое число, которым процесс потом
//! ссылается на открытый файл в `read`/`lseek`/`close`. У каждого процесса своя таблица
//! дескрипторов. «Текущий процесс» определяем по его адресному пространству: активный
//! корень таблиц страниц (CR3) уникален для процесса, поэтому таблицы храним в реестре,
//! ключ — физический адрес PML4 (через [`crate::arch::context::current_address_space`], не
//! трогая планировщик). Кадры не переиспользуются (аллокатор не освобождает, M3), так что
//! ключи не сталкиваются; при появлении реапинга запись надо будет удалять на `exit`
//! (HARDENING).
//!
//! # Узко и намеренно
//!
//! Открытый файл — это его **содержимое целиком в памяти** плюс позиция чтения: на `open`
//! читаем файл с диска ([`crate::fs::open`]), дальше `read` отдаёт байты из этого буфера.
//! Только чтение; дескрипторы 0/1/2 зарезервированы под стандартные потоки (их обслуживает
//! `write`), `open` выдаёт fd ≥ 3.

use super::{abi, uaccess};
use crate::drivers::{serial, vga};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::interrupts;
use x86_64::structures::paging::Page;
use x86_64::VirtAddr;

/// Верхняя граница числа дескрипторов на процесс.
const MAX_FDS: usize = 16;

// Whence для `lseek` (Linux).
const SEEK_SET: u64 = 0;
const SEEK_CUR: u64 = 1;
const SEEK_END: u64 = 2;

/// Открытый файл: его содержимое целиком и текущая позиция. `Clone` — для `fork` (M6f3):
/// ребёнок получает независимую копию. Запись (M6g3) идёт по модели **write-back**: `write`
/// меняет буфер `data` в памяти и взводит `dirty`, а `close` сбрасывает буфер на диск
/// (`fs::write_file` по имени `name`). Незакрытый файл свои изменения теряет — см. HARDENING.
#[derive(Clone)]
struct OpenFile {
    /// Имя файла (путь) — нужно, чтобы при `close` записать буфер обратно на диск.
    name: String,
    data: Vec<u8>,
    offset: usize,
    /// Открыт ли на запись (`O_WRONLY`/`O_RDWR`). `write` в файл только для чтения — `-EBADF`.
    writable: bool,
    /// Есть ли несброшенные изменения (нужно записать на диск при `close`).
    dirty: bool,
    /// Это каталог (M6g5): `data` хранит сериализованные записи `getdents64`, `offset` — байтовый
    /// курсор по ним. `read`/`write` на каталоге — ошибка; листинг идёт через `getdents64`.
    is_dir: bool,
}

/// Куда направлен файловый дескриптор (M7g1). Раньше fd 0/1/2 были захардкожены в `read`/`write`;
/// теперь они — обычные записи таблицы с подложкой `Console`/`Vga`/`Serial`, поэтому их можно
/// **перенаправить** (`dup2`, редиректы shell): открыть файл и `dup2` его на fd 1 — и `write(1)`
/// пойдёт в файл, а не на экран. `Clone` — для `fork`/`dup2` (копия подложки).
#[derive(Clone)]
enum Fd {
    /// stdin: блокирующее построчное чтение с консоли; `write` — `-EBADF`.
    Console,
    /// stdout: запись на экран (VGA); `read` — `-EBADF`.
    Vga,
    /// stderr: запись в serial; `read` — `-EBADF`.
    Serial,
    /// Обычный файл или каталог на диске.
    File(OpenFile),
    /// Конец чтения канала (M7g2).
    PipeRead(PipeRead),
    /// Конец записи канала (M7g2).
    PipeWrite(PipeWrite),
    /// UDP-сокет кольца 3 (M8d3): хэндл в общий сетевой стек.
    Socket(SocketEnd),
}

/// Сокет в таблице дескрипторов (M8d3): хэндл в общий сетевой стек ([`crate::net::socket`]) плюс
/// refcount — как у концов канала. `Clone` (fork/dup2) добавляет ссылку, `Drop` (close/exit/kill)
/// убирает; когда исчезает последняя — сокет удаляется из стека. Так fork/dup2/close/exit
/// учитываются сами, без отдельных хуков.
struct SocketEnd {
    handle: crate::net::socket::Handle,
    kind: crate::net::socket::SockKind,
    refs: Arc<()>,
}

impl Clone for SocketEnd {
    fn clone(&self) -> Self {
        SocketEnd {
            handle: self.handle,
            kind: self.kind,
            refs: self.refs.clone(),
        }
    }
}

impl Drop for SocketEnd {
    fn drop(&mut self) {
        // Этот конец — последний владелец (включая себя)? Тогда сокет больше никому не нужен.
        if Arc::strong_count(&self.refs) == 1 {
            crate::net::socket::close_socket(self.handle);
        }
    }
}

/// Разделяемый буфер канала (M7g2): байты в пути + число живых концов чтения/записи. Счётчики
/// ведут сами концы через `Clone`/`Drop`, поэтому fork/dup2/close/exit/kill учитываются
/// автоматически (любой способ исчезновения конца уменьшает счётчик). Буфер неограничен (запись
/// не блокируется — для shell-пайпов с небольшими данными достаточно; см. HARDENING).
struct PipeBuf {
    data: VecDeque<u8>,
    readers: usize,
    writers: usize,
}

/// Конец **чтения** канала. `Clone` (fork/dup2) увеличивает счётчик читателей, `Drop`
/// (close/exit/kill) уменьшает; когда читателей не осталось — будит заблокированных писателей
/// (им пора `-EPIPE`). Доступ к буферу — всегда под `without_interrupts` (как у консоли/планировщика).
struct PipeRead {
    buf: Arc<Mutex<PipeBuf>>,
}

impl Clone for PipeRead {
    fn clone(&self) -> Self {
        interrupts::without_interrupts(|| self.buf.lock().readers += 1);
        PipeRead {
            buf: self.buf.clone(),
        }
    }
}

impl Drop for PipeRead {
    fn drop(&mut self) {
        let gone = interrupts::without_interrupts(|| {
            let mut b = self.buf.lock();
            b.readers -= 1;
            b.readers == 0
        });
        if gone {
            crate::sched::thread::wake_pipe_waiters(); // писатели → `-EPIPE`
        }
    }
}

/// Конец **записи** канала. Симметрично: `Clone` ++writers, `Drop` --writers; когда писателей не
/// осталось — будит заблокированных читателей (им пора видеть EOF).
struct PipeWrite {
    buf: Arc<Mutex<PipeBuf>>,
}

impl Clone for PipeWrite {
    fn clone(&self) -> Self {
        interrupts::without_interrupts(|| self.buf.lock().writers += 1);
        PipeWrite {
            buf: self.buf.clone(),
        }
    }
}

impl Drop for PipeWrite {
    fn drop(&mut self) {
        let gone = interrupts::without_interrupts(|| {
            let mut b = self.buf.lock();
            b.writers -= 1;
            b.writers == 0
        });
        if gone {
            crate::sched::thread::wake_pipe_waiters(); // читатели → EOF
        }
    }
}

/// Состояние процесса в файловом слое: таблица дескрипторов и текущий рабочий каталог (M7c).
/// `Clone` — для `fork` (ребёнок получает независимую копию обоих). Живёт в [`PROCESSES`] под
/// ключом CR3, поэтому cwd сам наследуется при `fork` ([`fork_fds`]), переживает `execve`
/// ([`rekey_process`]) и убирается на `exit` ([`forget_process`]) — теми же хуками, что и fd.
#[derive(Clone)]
struct ProcState {
    /// Таблица дескрипторов (индекс = fd). По умолчанию заполнена стандартными потоками 0/1/2.
    fds: Vec<Option<Fd>>,
    /// Текущий рабочий каталог — нормализованный абсолютный путь (всегда с ведущим `/`).
    cwd: String,
    /// Конец кучи процесса — «program break» (M9a). Инвариант: отображены ровно страницы
    /// `[USER_HEAP_BASE, ⌈brk⌉)`. По умолчанию = `USER_HEAP_BASE` (куча пуста). Наследуется при
    /// `fork` (вместе со скопированными страницами кучи), СБРАСЫВАЕТСЯ при `execve` (новый образ —
    /// свежая куча, см. [`rekey_process`]).
    brk: u64,
}

impl Default for ProcState {
    fn default() -> Self {
        ProcState {
            // Стандартные потоки: 0=stdin (консоль), 1=stdout (VGA), 2=stderr (serial).
            fds: alloc::vec![Some(Fd::Console), Some(Fd::Vga), Some(Fd::Serial)],
            cwd: String::from("/"),           // новый процесс стартует в корне
            brk: crate::arch::USER_HEAP_BASE, // куча пуста: разрыв на базе
        }
    }
}

/// Реестр состояний процессов: ключ — физический адрес PML4 (CR3) процесса.
static PROCESSES: Mutex<BTreeMap<u64, ProcState>> = Mutex::new(BTreeMap::new());

/// Забывает таблицу дескрипторов завершённого процесса (по физ. адресу его PML4). Зовёт
/// reaper (M6e3) при освобождении процесса. Это **корректность**: фрейм PML4 переиспользуется
/// (M6e1), и новый процесс не должен унаследовать чужие fd. У нормально завершённого процесса
/// дескрипторы уже закрыты ([`release_current_process_fds`] на `exit`); здесь освобождаются
/// дескрипторы убитых сигналом/сбоем процессов (их `Drop` отпустит и концы каналов).
pub fn forget_process(cr3_phys: u64) {
    PROCESSES.lock().remove(&cr3_phys);
}

/// Закрывает все дескрипторы ТЕКУЩЕГО процесса — зовётся из обработчика `exit` ДО завершения
/// (M7g1; концы каналов — M7g2). Делает это **синхронно** на ядровом стеке процесса (как `close`),
/// а не лениво в reaper'е, по двум причинам:
/// - грязные файлы (`> файл`, который программа не закрывает) должны быть **durable к моменту,
///   когда родитель вернётся из `wait`** — иначе следующая команда (`cat файл`) прочла бы
///   несброшенный файл;
/// - концы каналов должны освободиться сразу (`Drop` уменьшит счётчик и разбудит другой конец) —
///   иначе читатель на другом конце `cmd1 | cmd2` не увидел бы EOF, пока reaper не дойдёт до нас.
///
/// Забираем всю таблицу под замком, обрабатываем — без него (диск + `Drop` концов канала берут
/// другие замки). Сбой записи игнорируем: процесс уже завершается.
pub fn release_current_process_fds() {
    let key = current_key();
    let fds = {
        let mut procs = PROCESSES.lock();
        match procs.get_mut(&key) {
            Some(state) => core::mem::take(&mut state.fds),
            None => Vec::new(),
        }
    };
    for fd in fds {
        if let Some(Fd::File(f)) = &fd {
            if f.writable && f.dirty {
                let _ = crate::fs::write_file(&f.name, &f.data);
            }
        }
        // `fd` дропается здесь: для `PipeRead`/`PipeWrite` это уменьшит счётчик и разбудит
        // заблокированный другой конец.
    }
}

/// (Диагностика/тесты) Сколько процессов сейчас имеют таблицу дескрипторов.
pub fn process_count() -> usize {
    PROCESSES.lock().len()
}

/// Переносит состояние процесса (fd-таблица + cwd) с ключа `old_cr3` на `new_cr3` — для `execve`
/// (M6f2): дескрипторы и рабочий каталог переживают exec, но ключом служит CR3, а он при exec
/// меняется.
pub fn rekey_process(old_cr3: u64, new_cr3: u64) {
    let mut procs = PROCESSES.lock();
    if let Some(mut state) = procs.remove(&old_cr3) {
        // Дескрипторы и cwd переживают exec, а вот КУЧА — нет: у нового образа своё адресное
        // пространство (страницы старой кучи освобождены вместе со старым), поэтому разрыв
        // сбрасываем на базу (M9a). Иначе `brk(0)` нового образа вернул бы чужой старый разрыв.
        state.brk = crate::arch::USER_HEAP_BASE;
        procs.insert(new_cr3, state);
    }
}

/// Клонирует состояние процесса `parent_cr3` для ребёнка `child_cr3` — для `fork` (M6f3):
/// ребёнок наследует независимые копии открытых файлов (каждая со своим содержимым и позицией)
/// И текущий рабочий каталог (M7c). Если у родителя записи ещё нет (ни одного `open`/`chdir`),
/// у ребёнка её тоже не будет (создастся дефолтной — cwd `/` — при первом обращении).
pub fn fork_fds(parent_cr3: u64, child_cr3: u64) {
    let mut procs = PROCESSES.lock();
    if let Some(state) = procs.get(&parent_cr3).cloned() {
        procs.insert(child_cr3, state);
    }
}

/// Ключ текущего процесса — физ. адрес его корня таблиц страниц.
fn current_key() -> u64 {
    crate::arch::context::current_address_space()
        .start_address()
        .as_u64()
}

/// Выполняет `f` над состоянием текущего процесса (создаёт дефолтное — пустые fd, cwd `/` — при
/// первом обращении). Безопасно без `without_interrupts`: реестр не трогают обработчики
/// прерываний, а syscall идёт с IF=0 (не реентерабелен).
fn with_current_proc<R>(f: impl FnOnce(&mut ProcState) -> R) -> R {
    let key = current_key();
    let mut guard = PROCESSES.lock();
    f(guard.entry(key).or_default())
}

/// Выполняет `f` над таблицей дескрипторов текущего процесса (через [`with_current_proc`]).
fn with_current_fds<R>(f: impl FnOnce(&mut Vec<Option<Fd>>) -> R) -> R {
    with_current_proc(|p| f(&mut p.fds))
}

/// Текущий рабочий каталог процесса — нормализованный абсолютный путь (M7c). По умолчанию `/`.
fn current_cwd() -> String {
    with_current_proc(|p| p.cwd.clone())
}

/// Превращает пользовательский путь в нормализованный абсолютный (M7c): относительный (без
/// ведущего `/`) достраивается от текущего cwd, затем `.`/`..`/повторные `/` схлопываются.
/// FAT-слой `.`/`..` не понимает (сопоставляет имена как 8.3), поэтому разрешаем их здесь — до
/// обращения к ФС. Результат всегда начинается с `/`.
pub fn resolve_path(path: &str) -> String {
    if path.starts_with('/') {
        normalize(path)
    } else {
        let mut combined = current_cwd();
        if !combined.ends_with('/') {
            combined.push('/');
        }
        combined.push_str(path);
        normalize(&combined)
    }
}

/// Схлопывает путь: убирает пустые компоненты (`//`), `.` (текущий каталог) и `..` (вверх, с
/// зажимом на корне — выше `/` не уходим). Возвращает `/` для корня, иначе `/a/b/…` без
/// хвостового слэша.
fn normalize(path: &str) -> String {
    let mut comps: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {} // пусто (из `//` или краёв) и `.` — пропускаем
            ".." => {
                comps.pop(); // вверх; на корне (пусто) — no-op
            }
            p => comps.push(p),
        }
    }
    let mut out = String::from("/");
    out.push_str(&comps.join("/"));
    out
}

/// Кладёт подложку в **наименьший свободный** дескриптор (POSIX). 0/1/2 по умолчанию заняты
/// стандартными потоками, поэтому `open` обычно отдаёт fd ≥ 3 — но если процесс закрыл стандартный
/// поток, его номер переиспользуется. Возвращает fd или `-EMFILE`, если таблица полна.
fn alloc_fd(fds: &mut Vec<Option<Fd>>, entry: Fd) -> i64 {
    match (0..fds.len()).find(|&i| fds[i].is_none()) {
        Some(i) => {
            fds[i] = Some(entry);
            i as i64
        }
        None if fds.len() < MAX_FDS => {
            fds.push(Some(entry));
            (fds.len() - 1) as i64
        }
        None => -abi::EMFILE,
    }
}

/// `open(path, flags, mode)` — открывает файл ИЛИ каталог и возвращает дескриптор. Для файла
/// поддержаны режим доступа (`O_RDONLY`/`O_WRONLY`/`O_RDWR`), `O_CREAT` и `O_TRUNC`; `mode`
/// игнорируем. Каталог открывается только на чтение — его листинг идёт через `getdents64`
/// (M6g5). `path` — нуль-терминированная строка в памяти пользователя.
pub fn sys_open(path_ptr: u64, flags: u64) -> i64 {
    let path = match uaccess::read_user_cstr(path_ptr) {
        Ok(p) => p,
        Err(errno) => return -errno,
    };
    let path = match core::str::from_utf8(&path) {
        Ok(s) => s,
        Err(_) => return -abi::ENOENT,
    };
    if path.is_empty() {
        return -abi::ENOENT; // пустой путь — `-ENOENT`, как в Linux (а не «открыть cwd»)
    }
    // Относительный путь → абсолютный от cwd, `.`/`..` разрешены (M7c). Дальше по ФС идёт уже
    // абсолютный путь; его же запоминаем в дескрипторе (`close` пишет обратно по нему).
    let path = resolve_path(path);
    let path = path.as_str();

    let writable = (flags & abi::O_ACCMODE) != abi::O_RDONLY;
    let create = flags & abi::O_CREAT != 0;
    let truncate = flags & abi::O_TRUNC != 0;
    let append = flags & abi::O_APPEND != 0;

    // Узнаём, файл это или каталог (или чего нет). Один проход по ФС.
    let open_file = match crate::fs::lookup(path) {
        Ok(crate::fs::fat::Node::File(existing)) => {
            // Обычный файл. O_TRUNC обрезает на диске сразу (как в Linux — при открытии).
            let data = if truncate {
                if let Err(e) = crate::fs::write_file(path, &[]) {
                    return -fat_errno(e);
                }
                Vec::new()
            } else {
                existing
            };
            OpenFile {
                name: String::from(path),
                // O_APPEND: позиция в конце — записи дописываются (для `>>`).
                offset: if append { data.len() } else { 0 },
                data,
                writable,
                dirty: false,
                is_dir: false,
            }
        }
        Ok(crate::fs::fat::Node::Dir(items)) => {
            // Каталог: на запись открыть нельзя; данные — сериализованные записи getdents64.
            if writable {
                return -abi::EISDIR;
            }
            OpenFile {
                name: String::from(path),
                data: serialize_dirents(&items),
                offset: 0,
                writable: false,
                dirty: false,
                is_dir: true,
            }
        }
        Err(crate::fs::fat::FatError::NotFound) if create => {
            // O_CREAT и файла нет — создаём пустым сразу, чтобы он существовал и без записи.
            if let Err(e) = crate::fs::write_file(path, &[]) {
                return -fat_errno(e);
            }
            OpenFile {
                name: String::from(path),
                data: Vec::new(),
                offset: 0,
                writable,
                dirty: false,
                is_dir: false,
            }
        }
        Err(crate::fs::fat::FatError::NotFound) => return -abi::ENOENT,
        Err(e) => return -fat_errno(e),
    };

    with_current_fds(|fds| alloc_fd(fds, Fd::File(open_file)))
}

/// `chdir(path)` (M7c): меняет текущий рабочий каталог процесса. Путь резолвится от cwd и должен
/// указывать на существующий **каталог**. `-ENOTDIR`, если это файл; `-ENOENT`, если пути нет;
/// иначе соответствующий `-errno`.
pub fn sys_chdir(path_ptr: u64) -> i64 {
    let path = match uaccess::read_user_cstr(path_ptr) {
        Ok(p) => p,
        Err(errno) => return -errno,
    };
    let path = match core::str::from_utf8(&path) {
        Ok(s) => s,
        Err(_) => return -abi::ENOENT,
    };
    if path.is_empty() {
        return -abi::ENOENT; // пустой путь — `-ENOENT`, как в Linux
    }
    let resolved = resolve_path(path);
    // Каталог должен существовать: один `lookup` скажет, файл это или каталог.
    match crate::fs::lookup(&resolved) {
        Ok(crate::fs::fat::Node::Dir(_)) => {
            with_current_proc(|p| p.cwd = resolved);
            0
        }
        Ok(crate::fs::fat::Node::File(_)) => -abi::ENOTDIR,
        Err(crate::fs::fat::FatError::NotFound) => -abi::ENOENT,
        Err(e) => -fat_errno(e),
    }
}

/// `getcwd(buf, size)` (M7c): копирует текущий рабочий каталог (с завершающим нулём) в буфер
/// пользователя. Возвращает число записанных байт включая нуль (как сырой Linux-`getcwd`);
/// `-ERANGE`, если не влезает в `size`; `-EFAULT` на недоступном буфере.
pub fn sys_getcwd(buf: u64, size: u64) -> i64 {
    let cwd = current_cwd();
    let needed = cwd.len() + 1; // +1 под завершающий нуль
    if (size as usize) < needed {
        return -abi::ERANGE;
    }
    let mut out = cwd.into_bytes();
    out.push(0); // нуль-терминатор
    match uaccess::copy_to_user(buf, &out) {
        Ok(()) => needed as i64,
        Err(errno) => -errno,
    }
}

/// `getdents64(fd, buf, count)` — копирует записи каталога (М6g5) в буфер пользователя. Записи
/// уже сериализованы в `data` при `open`; копируем из `offset` столько ЦЕЛЫХ записей, сколько
/// влезает в `count`, и сдвигаем курсор. 0 — записи кончились. `-ENOTDIR`, если fd не каталог;
/// `-EINVAL`, если буфер меньше одной записи.
pub fn sys_getdents64(fd: u64, buf: u64, count: u64) -> i64 {
    let fd = fd as usize;
    let count = count as usize;
    with_current_fds(|fds| {
        let file = match fds.get_mut(fd).and_then(|slot| slot.as_mut()) {
            Some(Fd::File(f)) if f.is_dir => f,
            Some(Fd::File(_)) => return -abi::ENOTDIR, // обычный файл — не каталог
            Some(_) => return -abi::ENOTDIR,           // консоль/устройство — не каталог
            None => return -abi::EBADF,
        };
        let data = &file.data;
        // Набираем целые записи (длина каждой — в d_reclen, поле u16 по смещению +16) от курсора.
        let mut span = 0usize;
        while file.offset + span < data.len() {
            let rec = file.offset + span;
            let reclen = u16::from_le_bytes([data[rec + 16], data[rec + 17]]) as usize;
            if span + reclen > count {
                break;
            }
            span += reclen;
        }
        if span == 0 {
            // Либо записи кончились (вернём 0), либо буфер меньше одной записи (-EINVAL).
            return if file.offset >= data.len() {
                0
            } else {
                -abi::EINVAL
            };
        }
        match uaccess::copy_to_user(buf, &data[file.offset..file.offset + span]) {
            Ok(()) => {
                file.offset += span;
                span as i64
            }
            Err(errno) => -errno,
        }
    })
}

/// Тип записи в `d_type` структуры `linux_dirent64`.
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;

/// Сериализует записи каталога в поток структур `linux_dirent64` (Linux x86-64), как их ждёт
/// `getdents64`: `d_ino u64`, `d_off i64`, `d_reclen u16`, `d_type u8`, затем нуль-терминированное
/// имя; длина каждой записи выровнена вверх по 8 байт.
fn serialize_dirents(items: &[crate::fs::fat::DirItem]) -> Vec<u8> {
    /// Заголовок до имени: d_ino(8)+d_off(8)+d_reclen(2)+d_type(1) = 19 байт.
    const HEADER: usize = 19;
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let name = item.name.as_bytes();
        let reclen = (HEADER + name.len() + 1).div_ceil(8) * 8; // +1 под нуль, выравнивание 8
        let start = out.len();
        out.resize(start + reclen, 0);
        let rec = &mut out[start..start + reclen];
        rec[0..8].copy_from_slice(&((i as u64) + 1).to_le_bytes()); // d_ino (псевдо, ненулевой)
        rec[8..16].copy_from_slice(&((start + reclen) as i64).to_le_bytes()); // d_off — курсор за этой записью
        rec[16..18].copy_from_slice(&(reclen as u16).to_le_bytes()); // d_reclen
        rec[18] = if item.is_dir { DT_DIR } else { DT_REG }; // d_type
        rec[HEADER..HEADER + name.len()].copy_from_slice(name); // имя (нуль уже стоит — буфер обнулён)
    }
    out
}

/// `write(fd, buf, count)` (M7g1): диспетчеризуется по подложке дескриптора — `Vga`/`Serial` пишут
/// на устройство, `File` (на запись, не каталог) — в буфер файла (write-back). `-EBADF`, если
/// дескриптор закрыт / только на чтение / каталог. `user_rsp` — для тестовой наблюдаемости.
pub fn sys_write(fd: u64, buf: u64, count: u64, user_rsp: u64) -> i64 {
    let fd = fd as usize;
    // Тип подложки — под коротким замком; устройство пишем уже без него.
    enum Kind {
        Vga,
        Serial,
        Pipe,
        File,
        NotWritable,
        Bad,
    }
    let kind = with_current_fds(|fds| match fds.get(fd).and_then(|s| s.as_ref()) {
        Some(Fd::Vga) => Kind::Vga,
        Some(Fd::Serial) => Kind::Serial,
        Some(Fd::PipeWrite(_)) => Kind::Pipe,
        Some(Fd::File(f)) if f.writable && !f.is_dir => Kind::File,
        Some(_) => Kind::NotWritable, // консоль/PipeRead (read-only), файл без записи, каталог
        None => Kind::Bad,
    });
    match kind {
        Kind::Bad | Kind::NotWritable => -abi::EBADF,
        Kind::Vga => write_device(fd as u64, buf, count, user_rsp, vga::write_bytes),
        Kind::Serial => write_device(fd as u64, buf, count, user_rsp, serial::write_bytes),
        Kind::Pipe => write_pipe(fd, buf, count),
        Kind::File => with_current_fds(|fds| {
            let Some(Fd::File(file)) = fds.get_mut(fd).and_then(|s| s.as_mut()) else {
                return -abi::EBADF; // не должно меняться при IF=0, но перепроверяем
            };
            // Копируем из памяти пользователя в буфер файла (uaccess сам проверит диапазон).
            match uaccess::with_user_bytes(buf, count, |bytes| {
                let end = file.offset + bytes.len();
                if end > file.data.len() {
                    file.data.resize(end, 0);
                }
                file.data[file.offset..end].copy_from_slice(bytes);
                file.offset = end;
                file.dirty = true;
                bytes.len() as i64
            }) {
                Ok(written) => written,
                Err(errno) => -errno,
            }
        }),
    }
}

/// Пишет байты пользователя на устройство (VGA/serial) и фиксирует наблюдаемость для тестов
/// (M5b: `LAST_WRITE_*`). `-EFAULT`, если буфер пользователя недоступен.
fn write_device(fd: u64, buf: u64, count: u64, user_rsp: u64, sink: fn(&[u8])) -> i64 {
    match uaccess::with_user_bytes(buf, count, |bytes| {
        sink(bytes);
        crate::syscall::record_write(fd, bytes, user_rsp);
        bytes.len() as i64
    }) {
        Ok(written) => written,
        Err(errno) => -errno,
    }
}

/// `read(fd, buf, count)` (M7g1): диспетчеризуется по подложке. `Console` (stdin) блокируется до
/// строки (M7a); `File` читает с текущей позиции, сдвигая её. `Vga`/`Serial` — `-EBADF` (только
/// запись). Возвращает число прочитанных байт (0 — конец файла).
pub fn sys_read(fd: u64, buf: u64, count: u64) -> i64 {
    let fd = fd as usize;
    // Тип подложки определяем под коротким замком: блокирующее чтение консоли держать замок
    // `PROCESSES` нельзя (оно уступает CPU).
    enum Kind {
        Console,
        Pipe,
        File,
        NotReadable,
        Bad,
    }
    let kind = with_current_fds(|fds| match fds.get(fd).and_then(|s| s.as_ref()) {
        Some(Fd::Console) => Kind::Console,
        Some(Fd::PipeRead(_)) => Kind::Pipe,
        Some(Fd::File(_)) => Kind::File,
        Some(_) => Kind::NotReadable, // Vga/Serial/PipeWrite — только запись
        None => Kind::Bad,
    });
    match kind {
        Kind::Console => read_stdin(buf, count),
        Kind::Pipe => read_pipe(fd, buf, count),
        Kind::Bad | Kind::NotReadable => -abi::EBADF,
        Kind::File => {
            let count = count as usize;
            with_current_fds(|fds| {
                let Some(Fd::File(file)) = fds.get_mut(fd).and_then(|s| s.as_mut()) else {
                    return -abi::EBADF; // при IF=0 не меняется, но перепроверяем
                };
                if file.is_dir {
                    // Каталог нельзя читать как файл — листинг идёт через getdents64.
                    return -abi::EISDIR;
                }
                // Позиция могла уйти ЗА конец файла (`lseek` разрешает) — тогда читаем 0 (EOF),
                // а не уходим в переполнение `len - offset`. `start` зажат в пределах файла.
                let len = file.data.len();
                let start = file.offset.min(len);
                let n = (len - start).min(count);
                match uaccess::copy_to_user(buf, &file.data[start..start + n]) {
                    Ok(()) => {
                        file.offset += n;
                        n as i64
                    }
                    Err(errno) => -errno,
                }
            })
        }
    }
}

/// `read(0, buf, count)` со stdin (M7a): отдаёт пользователю ввод с консоли построчно. Если
/// готового ввода нет — **блокирует** процесс, пока линейная дисциплина не завершит строку
/// (Enter), затем перечитывает. Возвращает число скопированных байт (≥ 1 при успехе).
///
/// Развязка «потерянной побудки»: проверку «ввод пуст?» и установку `Blocked` делаем в одной
/// области с выключенными прерываниями (на одном ядре никто не вклинится между ними), поэтому
/// строка, пришедшая после проверки, не «проскочит» мимо засыпающего читателя. Копирование в
/// пользователя — уже вне этой области (syscall и так идёт с IF=0; держать его дольше незачем).
fn read_stdin(buf: u64, count: u64) -> i64 {
    if count == 0 {
        return 0;
    }
    let count = count as usize;
    loop {
        // Атомарно: забрать готовое ИЛИ, если пусто, заблокироваться и уступить CPU.
        let bytes = interrupts::without_interrupts(|| {
            let bytes = crate::drivers::console::take_ready(count);
            if bytes.is_empty() {
                crate::sched::thread::block_current_on_stdin();
                None // разбудили — внешний цикл перечитает
            } else {
                Some(bytes)
            }
        });
        if let Some(bytes) = bytes {
            return match uaccess::copy_to_user(buf, &bytes) {
                Ok(()) => bytes.len() as i64,
                // Буфер пользователя плох: не теряем уже снятую с очереди строку — возвращаем её
                // в начало буфера, чтобы следующий `read` её получил (как `EFAULT` в Linux).
                Err(errno) => {
                    crate::drivers::console::unread(bytes);
                    -errno
                }
            };
        }
    }
}

/// `mkdir(path, mode)` (M7f): создаёт каталог по пути (резолвится от cwd). `mode` игнорируем.
/// `-EEXIST`, если имя занято; `-ENOENT`, если родителя нет; иначе соответствующий `-errno`.
pub fn sys_mkdir(path_ptr: u64, _mode: u64) -> i64 {
    let path = match uaccess::read_user_cstr(path_ptr) {
        Ok(p) => p,
        Err(errno) => return -errno,
    };
    let path = match core::str::from_utf8(&path) {
        Ok(s) => s,
        Err(_) => return -abi::ENOENT,
    };
    if path.is_empty() {
        return -abi::ENOENT;
    }
    let resolved = resolve_path(path);
    match crate::fs::mkdir(&resolved) {
        Ok(()) => 0,
        Err(e) => -fat_errno(e),
    }
}

/// `unlink(path)` (M7g3): удаляет файл (резолвится от cwd). `-EISDIR` на каталоге (для него
/// `rmdir`); `-ENOENT`, если пути нет.
pub fn sys_unlink(path_ptr: u64) -> i64 {
    match path_arg(path_ptr) {
        Ok(resolved) => match crate::fs::unlink(&resolved) {
            Ok(()) => 0,
            Err(e) => -fat_errno(e),
        },
        Err(errno) => -errno,
    }
}

/// `rmdir(path)` (M7g3): удаляет ПУСТОЙ каталог. `-ENOTDIR` на файле; `-ENOTEMPTY`, если в каталоге
/// есть записи; `-ENOENT`, если пути нет.
pub fn sys_rmdir(path_ptr: u64) -> i64 {
    match path_arg(path_ptr) {
        Ok(resolved) => match crate::fs::rmdir(&resolved) {
            Ok(()) => 0,
            Err(e) => -fat_errno(e),
        },
        Err(errno) => -errno,
    }
}

/// Читает путь-аргумент из памяти пользователя, проверяет на пустоту и резолвит от cwd. Возвращает
/// нормализованный абсолютный путь или положительный `errno`. Общая часть `unlink`/`rmdir`.
fn path_arg(path_ptr: u64) -> Result<String, i64> {
    let path = uaccess::read_user_cstr(path_ptr)?;
    let path = core::str::from_utf8(&path).map_err(|_| abi::ENOENT)?;
    if path.is_empty() {
        return Err(abi::ENOENT);
    }
    Ok(resolve_path(path))
}

/// `brk(addr)` (M9a): задаёт конец кучи процесса («program break»). Возвращает НОВЫЙ разрыв при
/// успехе или ТЕКУЩИЙ при неудаче — это сырая Linux-семантика: libc так и запрашивает разрыв через
/// `brk(0)` (0 ниже базы → «неудача» → вернётся текущий разрыв).
///
/// Куча — приватная область процесса `[USER_HEAP_BASE, USER_HEAP_MAX)`. Мы внутри системного
/// вызова с активным CR3 процесса и `IF=0`, поэтому рост отображает новые страницы прямо в
/// активную таблицу, а сжатие снимает их и возвращает фреймы. Инвариант: отображены РОВНО страницы
/// `[USER_HEAP_BASE, ⌈brk⌉)`, поэтому новые/снимаемые страницы — это `[⌈cur⌉, ⌈addr⌉)`.
pub fn sys_brk(addr: u64) -> i64 {
    use crate::arch::{USER_HEAP_BASE, USER_HEAP_MAX};
    // Округление адреса вверх до границы страницы (4 КиБ).
    let page_up = |x: u64| (x + 0xFFF) & !0xFFF;

    with_current_proc(|p| {
        let cur = p.brk;
        // brk(0) и любой адрес вне [base, max] — запрос/неудача: разрыв не двигаем, отдаём текущий.
        if !(USER_HEAP_BASE..=USER_HEAP_MAX).contains(&addr) {
            return cur as i64;
        }
        if addr > cur {
            // Рост: отобразить страницы [⌈cur⌉, ⌈addr⌉) (ниже, [base, ⌈cur⌉), уже отображены).
            let from = page_up(cur);
            let to = page_up(addr);
            let mut va = from;
            while va < to {
                if !crate::mm::paging::map_active_user_page(Page::containing_address(
                    VirtAddr::new(va),
                )) {
                    // Нет фреймов: откатываем отображённое В ЭТОМ вызове, оставляем старый разрыв.
                    let mut back = from;
                    while back < va {
                        crate::mm::paging::unmap_active_user_page(Page::containing_address(
                            VirtAddr::new(back),
                        ));
                        back += 4096;
                    }
                    return cur as i64;
                }
                va += 4096;
            }
        } else if addr < cur {
            // Сжатие: снять страницы [⌈addr⌉, ⌈cur⌉) и вернуть их фреймы аллокатору.
            let mut va = page_up(addr);
            let to = page_up(cur);
            while va < to {
                crate::mm::paging::unmap_active_user_page(Page::containing_address(VirtAddr::new(
                    va,
                )));
                va += 4096;
            }
        }
        p.brk = addr;
        addr as i64
    })
}

/// Размер линуксового `struct stat` (x86-64): 144 байта. Мы заполняем подмножество полей по их
/// точным смещениям ABI; остальные (uid/gid/времена/rdev/…) оставляем нулями.
const STAT_SIZE: usize = 144;

/// Собирает линуксовый `struct stat` (x86-64) в 144-байтный буфер по точным смещениям ABI:
/// `st_ino`@8, `st_nlink`@16, `st_mode`@24, `st_size`@48, `st_blksize`@56, `st_blocks`@64.
/// `st_blocks` — в единицах по 512 байт (как в Linux). Прочие поля — нули (буфер занулён).
fn build_stat(mode: u32, ino: u64, nlink: u64, size: u64) -> [u8; STAT_SIZE] {
    let mut b = [0u8; STAT_SIZE];
    b[8..16].copy_from_slice(&ino.to_le_bytes()); // st_ino
    b[16..24].copy_from_slice(&nlink.to_le_bytes()); // st_nlink
    b[24..28].copy_from_slice(&mode.to_le_bytes()); // st_mode
    b[48..56].copy_from_slice(&size.to_le_bytes()); // st_size
    b[56..64].copy_from_slice(&512u64.to_le_bytes()); // st_blksize
    b[64..72].copy_from_slice(&size.div_ceil(512).to_le_bytes()); // st_blocks (по 512 Б)
    b
}

/// `st_mode` (тип + права) для файла/каталога. Права условны (реальных прав у нас нет).
fn file_mode(is_dir: bool) -> u32 {
    if is_dir {
        abi::S_IFDIR | 0o755
    } else {
        abi::S_IFREG | 0o644
    }
}

/// `stat(path, statbuf)` (M9c): пишет метаданные пути в `struct stat` пользователя. Путь
/// резолвится от cwd. `st_ino` — первый кластер (псевдо-инод), `st_nlink` — 2 для каталога, иначе 1.
pub fn sys_stat(path_ptr: u64, statbuf: u64) -> i64 {
    let resolved = match path_arg(path_ptr) {
        Ok(p) => p,
        Err(errno) => return -errno,
    };
    match crate::fs::stat(&resolved) {
        Ok(meta) => {
            let nlink = if meta.is_dir { 2 } else { 1 };
            let st = build_stat(
                file_mode(meta.is_dir),
                meta.first_cluster as u64,
                nlink,
                meta.size as u64,
            );
            match uaccess::copy_to_user(statbuf, &st) {
                Ok(()) => 0,
                Err(errno) => -errno,
            }
        }
        Err(e) => -fat_errno(e),
    }
}

/// `fstat(fd, statbuf)` (M9c): метаданные по дескриптору. Различает подложку fd: файл/каталог →
/// обычный/каталог (размер — длина буфера в памяти); консоль/serial → символьное устройство
/// (для `isatty`); концы канала → FIFO. `-EBADF`, если дескриптор закрыт/неизвестен.
pub fn sys_fstat(fd: u64, statbuf: u64) -> i64 {
    // Собираем буфер под замком, копируем в пользователя — после (uaccess не держит замок).
    let st = with_current_fds(|fds| {
        let backing = fds.get(fd as usize).and_then(|slot| slot.as_ref());
        match backing {
            None => None,
            Some(Fd::Console) | Some(Fd::Vga) | Some(Fd::Serial) => {
                Some(build_stat(abi::S_IFCHR | 0o620, 0, 1, 0))
            }
            Some(Fd::PipeRead(_)) | Some(Fd::PipeWrite(_)) => {
                Some(build_stat(abi::S_IFIFO | 0o600, 0, 1, 0))
            }
            Some(Fd::Socket(_)) => Some(build_stat(abi::S_IFSOCK | 0o600, 0, 1, 0)),
            Some(Fd::File(f)) => {
                let nlink = if f.is_dir { 2 } else { 1 };
                Some(build_stat(
                    file_mode(f.is_dir),
                    0,
                    nlink,
                    f.data.len() as u64,
                ))
            }
        }
    });
    match st {
        None => -abi::EBADF,
        Some(buf) => match uaccess::copy_to_user(statbuf, &buf) {
            Ok(()) => 0,
            Err(errno) => -errno,
        },
    }
}

/// Преобразует ошибку FAT в errno для возврата пользователю.
fn fat_errno(e: crate::fs::fat::FatError) -> i64 {
    use crate::fs::fat::FatError;
    match e {
        FatError::NotFound => abi::ENOENT,
        FatError::IsADirectory => abi::EISDIR,
        FatError::NotADirectory => abi::ENOTDIR,
        FatError::AlreadyExists => abi::EEXIST,
        FatError::NotEmpty => abi::ENOTEMPTY,
        FatError::NoSpace | FatError::DirFull => abi::ENOSPC,
        _ => abi::EIO,
    }
}

/// `close(fd)` — освобождает дескриптор, предварительно сбросив изменения на диск (M6g3):
/// если файл открыт на запись и «грязный», его буфер пишется обратно через `fs::write_file`.
/// Файл изымаем из таблицы ДО записи — не держим замок `PROCESSES` на время дискового ввода-
/// вывода. `-EBADF` на неверном дескрипторе; `-EIO`/`-ENOSPC`, если сброс на диск не удался.
pub fn sys_close(fd: u64) -> i64 {
    let fd = fd as usize;
    // Изымаем подложку из таблицы дескрипторов (слот освобождается).
    let entry = with_current_fds(|fds| fds.get_mut(fd).and_then(|slot| slot.take()));
    match entry {
        None => -abi::EBADF,
        Some(Fd::File(file)) => {
            if file.writable && file.dirty {
                if let Err(e) = crate::fs::write_file(&file.name, &file.data) {
                    return -fat_errno(e);
                }
            }
            0
        }
        Some(_) => 0, // консоль/устройство — закрывать на диске нечего
    }
}

/// `dup2(oldfd, newfd)` (M7g1): направляет `newfd` на ту же подложку, что и `oldfd` (закрывая
/// прежний `newfd`). Основа редиректов: `open(файл)` → `dup2(fd, 1)` — и `write(1)` идёт в файл.
/// `-EBADF`, если `oldfd` закрыт или `newfd` вне диапазона. Возвращает `newfd`.
///
/// Подложка **копируется** (не разделяется): для `File` это отдельная копия буфера/позиции — не
/// полноценное «общее описание файла» Unix, но для редиректов (open→dup2→close) достаточно; см.
/// HARDENING. Прежний `newfd`, если был грязным файлом, при этом теряет несброшенные данные.
pub fn sys_dup2(oldfd: u64, newfd: u64) -> i64 {
    let oldfd = oldfd as usize;
    let newfd = newfd as usize;
    if newfd >= MAX_FDS {
        return -abi::EBADF;
    }
    with_current_proc(|p| {
        let fds = &mut p.fds;
        // oldfd должен быть открыт.
        if fds.get(oldfd).and_then(|s| s.as_ref()).is_none() {
            return -abi::EBADF;
        }
        if oldfd == newfd {
            return newfd as i64;
        }
        let dup = fds[oldfd].clone();
        while fds.len() <= newfd {
            fds.push(None);
        }
        fds[newfd] = dup;
        newfd as i64
    })
}

/// Читает `index`-й `struct iovec { base: u64 @0; len: u64 @8 }` (16 байт) из массива в памяти
/// пользователя по адресу `iov`. Адрес элемента считаем через `checked_add`: пользователь задаёт
/// `iov` произвольным, и `iov + index*16` у самой вершины адресного пространства переполнился бы —
/// в dev-профиле (overflow-checks включены) это уронило бы ЯДРО. На переполнении — `-EFAULT`.
fn read_iovec(iov: u64, index: u64) -> Result<(u64, u64), i64> {
    let slot = iov.checked_add(index * 16).ok_or(abi::EFAULT)?;
    uaccess::with_user_bytes(slot, 16, |b| {
        let base = u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
        let len = u64::from_le_bytes([b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]);
        (base, len)
    })
}

/// `writev(fd, iov, iovcnt)` (M9f): записывает буферы массива `iovec` по порядку, как один `write`.
/// libc буферизует вывод и сбрасывает его одним `writev`. Возвращает суммарно записанные байты;
/// на ошибке первого буфера — `-errno`, на ошибке/короткой записи позже — уже записанное (POSIX).
pub fn sys_writev(fd: u64, iov: u64, iovcnt: u64, user_rsp: u64) -> i64 {
    if iovcnt > abi::IOV_MAX {
        return -abi::EINVAL;
    }
    let mut total: i64 = 0;
    for i in 0..iovcnt {
        let (base, len) = match read_iovec(iov, i) {
            Ok(v) => v,
            Err(errno) => return if total == 0 { -errno } else { total },
        };
        if len == 0 {
            continue;
        }
        let written = sys_write(fd, base, len, user_rsp);
        if written < 0 {
            return if total == 0 { written } else { total };
        }
        total += written;
        if (written as u64) < len {
            break; // короткая запись — дальше не продолжаем
        }
    }
    total
}

/// `readv(fd, iov, iovcnt)` (M9f): читает в буферы массива `iovec` по порядку, как один `read`.
/// Возвращает суммарно прочитанные байты; короткое чтение/EOF (меньше длины буфера) останавливает
/// заполнение дальнейших буферов.
pub fn sys_readv(fd: u64, iov: u64, iovcnt: u64) -> i64 {
    if iovcnt > abi::IOV_MAX {
        return -abi::EINVAL;
    }
    let mut total: i64 = 0;
    for i in 0..iovcnt {
        let (base, len) = match read_iovec(iov, i) {
            Ok(v) => v,
            Err(errno) => return if total == 0 { -errno } else { total },
        };
        if len == 0 {
            continue;
        }
        let got = sys_read(fd, base, len);
        if got < 0 {
            return if total == 0 { got } else { total };
        }
        total += got;
        if (got as u64) < len {
            break; // короткое чтение / EOF
        }
    }
    total
}

/// `fcntl(fd, cmd, arg)` (M9f): управление дескриптором. Поддержаны `F_DUPFD` (дублировать в
/// наименьший свободный ≥ `arg`), `F_GETFL` (режим доступа по подложке) и no-op `F_GETFD`/`F_SETFD`/
/// `F_SETFL` (флаги close-on-exec / `O_NONBLOCK` мы не отслеживаем). Прочие команды — `-EINVAL`.
/// `F_DUPFD` копирует подложку (как `dup2`, не разделяя позицию файла — см. HARDENING).
pub fn sys_fcntl(fd: u64, cmd: u64, arg: u64) -> i64 {
    match cmd {
        abi::F_DUPFD => {
            let start = arg as usize;
            if start >= MAX_FDS {
                return -abi::EINVAL;
            }
            with_current_fds(|fds| {
                let backing = match fds.get(fd as usize).and_then(|s| s.as_ref()) {
                    Some(b) => b.clone(),
                    None => return -abi::EBADF,
                };
                // Наименьший свободный индекс ≥ start (за пределами вектора — тоже «свободен»).
                let mut idx = start;
                while idx < MAX_FDS && fds.get(idx).map(Option::is_some).unwrap_or(false) {
                    idx += 1;
                }
                if idx >= MAX_FDS {
                    return -abi::EMFILE;
                }
                while fds.len() <= idx {
                    fds.push(None);
                }
                fds[idx] = Some(backing);
                idx as i64
            })
        }
        abi::F_GETFD | abi::F_SETFD | abi::F_SETFL => with_current_fds(|fds| {
            if fds.get(fd as usize).and_then(|s| s.as_ref()).is_some() {
                0
            } else {
                -abi::EBADF
            }
        }),
        abi::F_GETFL => {
            with_current_fds(|fds| match fds.get(fd as usize).and_then(|s| s.as_ref()) {
                None => -abi::EBADF,
                Some(Fd::Console) | Some(Fd::PipeRead(_)) => abi::O_RDONLY as i64,
                Some(Fd::Vga) | Some(Fd::Serial) | Some(Fd::PipeWrite(_)) => abi::O_WRONLY as i64,
                Some(Fd::Socket(_)) => abi::O_RDWR as i64,
                Some(Fd::File(f)) => {
                    if f.writable {
                        abi::O_RDWR as i64
                    } else {
                        abi::O_RDONLY as i64
                    }
                }
            })
        }
        _ => -abi::EINVAL,
    }
}

/// `pipe(fds)` (M7g2): создаёт канал и выдаёт два дескриптора — `fds[0]` конец чтения, `fds[1]`
/// конец записи (как в Linux). Концы делят один буфер; данные из `write(fds[1])` читаются из
/// `read(fds[0])`. Возвращает 0 или `-errno` (`-EMFILE`, если таблица полна; `-EFAULT` на плохом
/// `fds`).
pub fn sys_pipe(fds_ptr: u64) -> i64 {
    let buf = Arc::new(Mutex::new(PipeBuf {
        data: VecDeque::new(),
        readers: 1, // конец чтения, который кладём в таблицу
        writers: 1, // конец записи
    }));
    let read_end = Fd::PipeRead(PipeRead { buf: buf.clone() });
    let write_end = Fd::PipeWrite(PipeWrite { buf });

    // Выделяем оба дескриптора. При неудаче выделенный откатываем (его `Drop` вернёт счётчик).
    let (rfd, wfd) = with_current_proc(|p| {
        let r = alloc_fd(&mut p.fds, read_end);
        if r < 0 {
            return (r, r); // read_end не сохранён alloc_fd'ом → его Drop уже уменьшил readers
        }
        let w = alloc_fd(&mut p.fds, write_end);
        if w < 0 {
            p.fds[r as usize] = None; // дропаем сохранённый конец чтения
            return (w, w);
        }
        (r, w)
    });
    if rfd < 0 {
        return rfd;
    }
    if wfd < 0 {
        return wfd;
    }

    // Пишем пару i32 [rfd, wfd] в массив пользователя.
    let mut out = [0u8; 8];
    out[0..4].copy_from_slice(&(rfd as i32).to_le_bytes());
    out[4..8].copy_from_slice(&(wfd as i32).to_le_bytes());
    match uaccess::copy_to_user(fds_ptr, &out) {
        Ok(()) => 0,
        Err(errno) => -errno, // дескрипторы остаются открытыми — мелкая утечка при EFAULT
    }
}

/// `read` с конца чтения канала (M7g2): отдаёт данные из буфера; если пусто и писатели ещё есть —
/// **блокирует** до записи/закрытия; если пусто и писателей нет — EOF (0). Замок буфера не держим
/// через блокировку (как у stdin), поэтому writer всегда может его взять.
fn read_pipe(fd: usize, buf: u64, count: u64) -> i64 {
    // Клонируем Arc буфера (не сам конец — счётчик читателей не трогаем): он живёт независимо от
    // таблицы fd на время чтения.
    let pipe = with_current_fds(|fds| match fds.get(fd).and_then(|s| s.as_ref()) {
        Some(Fd::PipeRead(p)) => Some(p.buf.clone()),
        _ => None,
    });
    let pipe = match pipe {
        Some(p) => p,
        None => return -abi::EBADF,
    };
    if count == 0 {
        return 0;
    }
    let count = count as usize;
    enum Out {
        Data(Vec<u8>),
        Eof,
        Retry,
    }
    loop {
        let out = interrupts::without_interrupts(|| {
            let mut b = pipe.lock();
            if !b.data.is_empty() {
                let n = count.min(b.data.len());
                Out::Data(b.data.drain(..n).collect())
            } else if b.writers == 0 {
                Out::Eof
            } else {
                drop(b); // отпускаем замок буфера ДО блокировки (writer должен мочь его взять)
                crate::sched::thread::block_current_on_pipe();
                Out::Retry
            }
        });
        match out {
            Out::Data(d) => {
                return match uaccess::copy_to_user(buf, &d) {
                    Ok(()) => d.len() as i64,
                    Err(errno) => -errno,
                }
            }
            Out::Eof => return 0,
            Out::Retry => {}
        }
    }
}

/// `write` в конец записи канала (M7g2): дописывает байты в буфер и будит читателей. Буфер
/// неограничен — запись не блокируется. `-EPIPE`, если читателей не осталось (вместо SIGPIPE).
fn write_pipe(fd: usize, buf: u64, count: u64) -> i64 {
    let pipe = with_current_fds(|fds| match fds.get(fd).and_then(|s| s.as_ref()) {
        Some(Fd::PipeWrite(p)) => Some(p.buf.clone()),
        _ => None,
    });
    let pipe = match pipe {
        Some(p) => p,
        None => return -abi::EBADF,
    };
    if count == 0 {
        return 0; // нулевая запись — 0, без `-EPIPE` (как в Linux)
    }
    let written = uaccess::with_user_bytes(buf, count, |bytes| {
        interrupts::without_interrupts(|| {
            let mut b = pipe.lock();
            if b.readers == 0 {
                return -abi::EPIPE; // некому читать
            }
            b.data.extend(bytes.iter().copied());
            bytes.len() as i64
        })
    });
    match written {
        Ok(n) => {
            if n > 0 {
                crate::sched::thread::wake_pipe_waiters(); // разбудить читателей
            }
            n
        }
        Err(errno) => -errno,
    }
}

/// `lseek(fd, offset, whence)` — двигает позицию чтения файла; возвращает новую позицию.
/// `-ESPIPE` на консоли/устройстве (не позиционируются).
pub fn sys_lseek(fd: u64, offset: i64, whence: u64) -> i64 {
    let fd = fd as usize;
    with_current_fds(|fds| {
        let file = match fds.get_mut(fd).and_then(|slot| slot.as_mut()) {
            Some(Fd::File(f)) => f,
            Some(_) => return -abi::ESPIPE, // консоль/устройство не позиционируется
            None => return -abi::EBADF,
        };
        let base = match whence {
            SEEK_SET => 0i64,
            SEEK_CUR => file.offset as i64,
            SEEK_END => file.data.len() as i64,
            _ => return -abi::EINVAL,
        };
        match base.checked_add(offset) {
            // Позиция не может быть отрицательной; за концом файла — допустимо (read вернёт 0).
            Some(pos) if pos >= 0 => {
                file.offset = pos as usize;
                pos
            }
            _ => -abi::EINVAL,
        }
    })
}

// --- Сокеты (M8d3) ---

/// Максимум полезной нагрузки одной UDP-датаграммы (UDP поверх IPv4/Ethernet без фрагментации) —
/// предел на `sendto` (длиннее → `EMSGSIZE`).
const MAX_DGRAM: usize = 1472;
/// Максимум байт за один вызов сокет-ввода-вывода (буфер копирования в/из пользователя для
/// `recvfrom` и TCP-`sendto`). Совпадает с payload-буфером сокета в `net::socket`, чтобы `recvfrom`
/// не резал датаграмму/поток искусственно мельче, чем влезает у пользователя; остаток (для потока)
/// вызывающий дочитает/дошлёт следующим вызовом.
const SOCK_IO_MAX: usize = 4096;

/// Достаёт хэндл сокета и его тип (UDP/TCP) из дескриптора `fd`. `EBADF` — нет такого fd;
/// `ENOTSOCK` — fd не сокет. Тип нужен, чтобы send/recv шли к нужным операциям.
fn socket_handle(
    fd: u64,
) -> Result<(crate::net::socket::Handle, crate::net::socket::SockKind), i64> {
    with_current_fds(|fds| match fds.get(fd as usize).and_then(|s| s.as_ref()) {
        Some(Fd::Socket(s)) => Ok((s.handle, s.kind)),
        Some(_) => Err(abi::ENOTSOCK),
        None => Err(abi::EBADF),
    })
}

/// Разбирает пользовательский `struct sockaddr_in` (`AF_INET`): возвращает IPv4-октеты и порт.
/// `EINVAL` — буфер короче 16 байт; `EAFNOSUPPORT` — не `AF_INET`; `EFAULT` — адрес недоступен.
fn parse_sockaddr_in(addr_ptr: u64, addrlen: u64) -> Result<([u8; 4], u16), i64> {
    if (addrlen as usize) < abi::SOCKADDR_IN_LEN {
        return Err(abi::EINVAL);
    }
    uaccess::with_user_bytes(addr_ptr, abi::SOCKADDR_IN_LEN as u64, |b| {
        // sin_family — в порядке хоста; sin_port и sin_addr — в сетевом (big-endian).
        if u16::from_ne_bytes([b[0], b[1]]) as u64 != abi::AF_INET {
            return Err(abi::EAFNOSUPPORT);
        }
        Ok(([b[4], b[5], b[6], b[7]], u16::from_be_bytes([b[2], b[3]])))
    })?
}

/// Собирает `struct sockaddr_in` адреса отправителя (для `recvfrom`).
fn encode_sockaddr_in(ip: [u8; 4], port: u16) -> [u8; abi::SOCKADDR_IN_LEN] {
    let mut sa = [0u8; abi::SOCKADDR_IN_LEN];
    sa[0..2].copy_from_slice(&(abi::AF_INET as u16).to_ne_bytes()); // sin_family (порядок хоста)
    sa[2..4].copy_from_slice(&port.to_be_bytes()); // sin_port (сетевой)
    sa[4..8].copy_from_slice(&ip); // sin_addr
    sa // sin_zero[8] = 0
}

/// `socket(domain, type, protocol)` (M8d3/M8e): создаёт UDP-сокет (`SOCK_DGRAM`) или TCP-сокет
/// (`SOCK_STREAM`) семейства `AF_INET` в общем стеке и кладёт его в дескриптор. `protocol`
/// игнорируем (0 = протокол по умолчанию для типа). Возвращает fd или `-errno` (`EAFNOSUPPORT` —
/// не `AF_INET`; `EPROTONOSUPPORT` — неподдержанный тип; `ENETDOWN` — сеть не поднялась; `EMFILE` —
/// таблица дескрипторов полна).
pub fn sys_socket(domain: u64, sock_type: u64, _protocol: u64) -> i64 {
    use crate::net::socket::SockKind;
    if domain != abi::AF_INET {
        return -abi::EAFNOSUPPORT;
    }
    let (handle, kind) = match sock_type {
        abi::SOCK_DGRAM => match crate::net::socket::udp_socket() {
            Ok(h) => (h, SockKind::Udp),
            Err(e) => return -e,
        },
        abi::SOCK_STREAM => match crate::net::socket::tcp_socket() {
            Ok(h) => (h, SockKind::Tcp),
            Err(e) => return -e,
        },
        _ => return -abi::EPROTONOSUPPORT,
    };
    let end = SocketEnd {
        handle,
        kind,
        refs: Arc::new(()),
    };
    // Если таблица полна, alloc_fd вернёт -EMFILE и НЕ сохранит entry — тогда `Fd::Socket` дропнется
    // прямо в alloc_fd, и его `Drop` уберёт сокет из стека (без утечки). Иначе — номер дескриптора.
    with_current_fds(|fds| alloc_fd(fds, Fd::Socket(end)))
}

/// `connect(fd, addr, addrlen)` (M8e): устанавливает TCP-соединение с адресом из `sockaddr_in`
/// (блокирующе — рукопожатие). Только для TCP-сокета (`EOPNOTSUPP` на UDP). 0 или `-errno`
/// (`ECONNREFUSED`/`ETIMEDOUT` — соединение не установилось).
pub fn sys_connect(fd: u64, addr_ptr: u64, addrlen: u64) -> i64 {
    let (handle, kind) = match socket_handle(fd) {
        Ok(v) => v,
        Err(e) => return -e,
    };
    if kind != crate::net::socket::SockKind::Tcp {
        return -abi::EOPNOTSUPP; // connect поддержан только для потоковых (TCP) сокетов
    }
    let (ip, port) = match parse_sockaddr_in(addr_ptr, addrlen) {
        Ok(v) => v,
        Err(e) => return -e,
    };
    match crate::net::socket::tcp_connect(handle, ip, port) {
        Ok(()) => 0,
        Err(e) => -e,
    }
}

/// `bind(fd, addr, addrlen)` (M8d3): привязывает UDP-сокет к локальному порту из `sockaddr_in`
/// (адрес привязки игнорируем — принимаем на всех локальных). 0 или `-errno`.
pub fn sys_bind(fd: u64, addr_ptr: u64, addrlen: u64) -> i64 {
    // Дескриптор проверяем ПЕРВЫМ (EBADF/ENOTSOCK старше ошибок адреса — как в Linux).
    let (handle, kind) = match socket_handle(fd) {
        Ok(v) => v,
        Err(e) => return -e,
    };
    // Только UDP: TCP-серверов (listen/accept) у нас нет, а клиенту bind не нужен (connect сам
    // берёт эфемерный порт).
    if kind != crate::net::socket::SockKind::Udp {
        return -abi::EOPNOTSUPP;
    }
    let (_ip, port) = match parse_sockaddr_in(addr_ptr, addrlen) {
        Ok(v) => v,
        Err(e) => return -e,
    };
    match crate::net::socket::udp_bind(handle, port) {
        Ok(()) => 0,
        Err(e) => -e,
    }
}

/// `sendto(fd, buf, len, flags, dest_addr, addrlen)` (M8d3/M8e): UDP — шлёт датаграмму на
/// `dest_addr`; TCP — `send` в соединение (адрес игнорируется). `flags` игнорируем. За один вызов
/// берём не больше [`MAX_DGRAM`] байт (вызывающий дошлёт остаток — как POSIX). Возвращает число
/// отправленных байт или `-errno` (`EMSGSIZE` — UDP-датаграмма длиннее лимита).
pub fn sys_sendto(fd: u64, buf: u64, len: u64, dest_addr: u64, addrlen: u64) -> i64 {
    use crate::net::socket::SockKind;
    // Дескриптор проверяем ПЕРВЫМ (EBADF/ENOTSOCK старше ошибок длины/адреса — как в Linux).
    let (handle, kind) = match socket_handle(fd) {
        Ok(v) => v,
        Err(e) => return -e,
    };
    match kind {
        SockKind::Udp => {
            if len as usize > MAX_DGRAM {
                return -abi::EMSGSIZE; // датаграмма должна уйти целиком
            }
            let (ip, port) = match parse_sockaddr_in(dest_addr, addrlen) {
                Ok(v) => v,
                Err(e) => return -e,
            };
            let data = match uaccess::with_user_bytes(buf, len, |b| b.to_vec()) {
                Ok(v) => v,
                Err(e) => return -e,
            };
            match crate::net::socket::udp_sendto(handle, &data, ip, port) {
                Ok(n) => n as i64,
                Err(e) => -e,
            }
        }
        SockKind::Tcp => {
            // Поток: адрес назначения не нужен (соединён). Один вызов берёт кусок ≤ SOCK_IO_MAX.
            let chunk = (len as usize).min(SOCK_IO_MAX) as u64;
            let data = match uaccess::with_user_bytes(buf, chunk, |b| b.to_vec()) {
                Ok(v) => v,
                Err(e) => return -e,
            };
            match crate::net::socket::tcp_send(handle, &data) {
                Ok(n) => n as i64,
                Err(e) => -e,
            }
        }
    }
}

/// `recvfrom(fd, buf, len, flags, src_addr, addrlen)` (M8d3): принимает датаграмму в `buf`
/// (блокирующе опросом — бюджет в опросах, см. `net::socket`). `flags` игнорируем. Если `src_addr` и
/// `addrlen` не NULL, пишет туда `sockaddr_in` отправителя (не больше входного `*addrlen` байт) и
/// кладёт фактическую длину `16` в `*addrlen`. Возвращает число прочитанных байт или `-errno`
/// (`EAGAIN` — за отведённые опросы ничего не пришло).
pub fn sys_recvfrom(fd: u64, buf: u64, len: u64, src_addr: u64, addrlen_ptr: u64) -> i64 {
    use crate::net::socket::SockKind;
    let (handle, kind) = match socket_handle(fd) {
        Ok(v) => v,
        Err(e) => return -e,
    };
    let cap = (len as usize).min(SOCK_IO_MAX);
    let mut kbuf = alloc::vec![0u8; cap];
    // UDP даёт адрес отправителя; TCP — поток, отправитель это подключённый пир (не пишем src).
    let (n, ip, port) = match kind {
        SockKind::Udp => match crate::net::socket::udp_recvfrom(handle, &mut kbuf) {
            Ok(v) => v,
            Err(e) => return -e,
        },
        SockKind::Tcp => match crate::net::socket::tcp_recv(handle, &mut kbuf) {
            Ok(n) => (n, [0, 0, 0, 0], 0),
            Err(e) => return -e,
        },
    };
    if let Err(e) = uaccess::copy_to_user(buf, &kbuf[..n]) {
        return -e;
    }
    // Адрес отправителя — UDP и только если запросили (src_addr и addrlen оба не NULL, POSIX).
    // Пишем не больше, чем вызывающий объявил во ВХОДНОМ `*addrlen` (иначе затёрли бы его память за
    // буфером адреса), затем кладём туда фактическую длину (16).
    if kind == SockKind::Udp && src_addr != 0 && addrlen_ptr != 0 {
        let want = match uaccess::with_user_bytes(addrlen_ptr, 4, |b| {
            u32::from_ne_bytes([b[0], b[1], b[2], b[3]]) as usize
        }) {
            Ok(v) => v,
            Err(e) => return -e,
        };
        let sa = encode_sockaddr_in(ip, port);
        let w = want.min(sa.len());
        if w > 0 {
            if let Err(e) = uaccess::copy_to_user(src_addr, &sa[..w]) {
                return -e;
            }
        }
        let l = (abi::SOCKADDR_IN_LEN as u32).to_ne_bytes();
        if let Err(e) = uaccess::copy_to_user(addrlen_ptr, &l) {
            return -e;
        }
    }
    n as i64
}
