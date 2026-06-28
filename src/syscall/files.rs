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
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::interrupts;

/// Верхняя граница числа дескрипторов на процесс.
const MAX_FDS: usize = 16;
/// Первый выдаваемый `open` дескриптор (0/1/2 — stdin/stdout/stderr).
const FIRST_FD: usize = 3;

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

/// Состояние процесса в файловом слое: таблица дескрипторов и текущий рабочий каталог (M7c).
/// `Clone` — для `fork` (ребёнок получает независимую копию обоих). Живёт в [`PROCESSES`] под
/// ключом CR3, поэтому cwd сам наследуется при `fork` ([`fork_fds`]), переживает `execve`
/// ([`rekey_process`]) и убирается на `exit` ([`forget_process`]) — теми же хуками, что и fd.
#[derive(Clone)]
struct ProcState {
    /// Таблица открытых файлов (индекс = fd).
    fds: Vec<Option<OpenFile>>,
    /// Текущий рабочий каталог — нормализованный абсолютный путь (всегда с ведущим `/`).
    cwd: String,
}

impl Default for ProcState {
    fn default() -> Self {
        ProcState {
            fds: Vec::new(),
            cwd: String::from("/"), // новый процесс стартует в корне
        }
    }
}

/// Реестр состояний процессов: ключ — физический адрес PML4 (CR3) процесса.
static PROCESSES: Mutex<BTreeMap<u64, ProcState>> = Mutex::new(BTreeMap::new());

/// Забывает таблицу дескрипторов завершённого процесса (по физ. адресу его PML4). Зовёт
/// reaper (M6e3) при освобождении процесса. Теперь это не просто уборка, а **корректность**:
/// фрейм PML4 переиспользуется (M6e1), и новый процесс не должен унаследовать чужие fd.
pub fn forget_process(cr3_phys: u64) {
    PROCESSES.lock().remove(&cr3_phys);
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
    if let Some(state) = procs.remove(&old_cr3) {
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
fn with_current_fds<R>(f: impl FnOnce(&mut Vec<Option<OpenFile>>) -> R) -> R {
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

/// Кладёт открытый файл в первый свободный дескриптор ≥ [`FIRST_FD`]. Возвращает fd или
/// `-EMFILE`, если таблица полна.
fn alloc_fd(fds: &mut Vec<Option<OpenFile>>, file: OpenFile) -> i64 {
    while fds.len() < FIRST_FD {
        fds.push(None);
    }
    match (FIRST_FD..fds.len()).find(|&i| fds[i].is_none()) {
        Some(i) => {
            fds[i] = Some(file);
            i as i64
        }
        None if fds.len() < MAX_FDS => {
            fds.push(Some(file));
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
                data,
                offset: 0,
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

    with_current_fds(|fds| alloc_fd(fds, open_file))
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
            Some(f) => f,
            None => return -abi::EBADF,
        };
        if !file.is_dir {
            return -abi::ENOTDIR;
        }
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

/// `write(fd, buf, count)` в обычный файл (fd ≥ 3, M6g3): копирует `count` байт из памяти
/// пользователя в буфер файла с текущей позиции (расширяя его при необходимости), сдвигает
/// позицию и помечает файл «грязным» (сбросится на диск при `close`). `-EBADF`, если дескриптор
/// неверен или открыт только на чтение.
pub fn sys_write(fd: u64, buf: u64, count: u64) -> i64 {
    let fd = fd as usize;
    with_current_fds(|fds| {
        let file = match fds.get_mut(fd).and_then(|slot| slot.as_mut()) {
            Some(f) => f,
            None => return -abi::EBADF,
        };
        if !file.writable {
            return -abi::EBADF;
        }
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
    })
}

/// `read(fd, buf, count)` — копирует до `count` байт в буфер пользователя. `fd == 0` (stdin,
/// M7a) читает с консоли (блокируется до строки); `fd ≥ 3` — из открытого файла с текущей
/// позиции (сдвигая её). Возвращает число прочитанных байт (0 — конец файла).
pub fn sys_read(fd: u64, buf: u64, count: u64) -> i64 {
    if fd == 0 {
        return read_stdin(buf, count);
    }
    let fd = fd as usize;
    let count = count as usize;
    with_current_fds(|fds| {
        let file = match fds.get_mut(fd).and_then(|slot| slot.as_mut()) {
            Some(f) => f,
            None => return -abi::EBADF,
        };
        if file.is_dir {
            // Каталог нельзя читать как файл — листинг идёт через getdents64.
            return -abi::EISDIR;
        }
        // Позиция могла уйти ЗА конец файла (`lseek` это разрешает) — тогда читаем 0 (EOF),
        // а не уходим в переполнение `len - offset`. `start` зажат в пределах файла.
        let len = file.data.len();
        let start = file.offset.min(len);
        let n = (len - start).min(count);
        // Копируем срез файла в память пользователя ДО сдвига позиции (срез заимствует
        // file.data; заём кончается с вызовом, дальше можно менять offset).
        match uaccess::copy_to_user(buf, &file.data[start..start + n]) {
            Ok(()) => {
                file.offset += n;
                n as i64
            }
            Err(errno) => -errno,
        }
    })
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

/// Преобразует ошибку FAT в errno для возврата пользователю.
fn fat_errno(e: crate::fs::fat::FatError) -> i64 {
    use crate::fs::fat::FatError;
    match e {
        FatError::NotFound => abi::ENOENT,
        FatError::IsADirectory => abi::EISDIR,
        FatError::NotADirectory => abi::ENOTDIR,
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
    // Изымаем открытый файл из таблицы дескрипторов (слот освобождается).
    let file = with_current_fds(|fds| fds.get_mut(fd).and_then(|slot| slot.take()));
    let file = match file {
        Some(f) => f,
        None => return -abi::EBADF,
    };
    if file.writable && file.dirty {
        if let Err(e) = crate::fs::write_file(&file.name, &file.data) {
            return -fat_errno(e);
        }
    }
    0
}

/// `lseek(fd, offset, whence)` — двигает позицию чтения; возвращает новую позицию.
pub fn sys_lseek(fd: u64, offset: i64, whence: u64) -> i64 {
    let fd = fd as usize;
    with_current_fds(|fds| {
        let file = match fds.get_mut(fd).and_then(|slot| slot.as_mut()) {
            Some(f) => f,
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
