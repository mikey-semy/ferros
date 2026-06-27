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
use alloc::vec::Vec;
use spin::Mutex;

/// Верхняя граница числа дескрипторов на процесс.
const MAX_FDS: usize = 16;
/// Первый выдаваемый `open` дескриптор (0/1/2 — stdin/stdout/stderr).
const FIRST_FD: usize = 3;

// Whence для `lseek` (Linux).
const SEEK_SET: u64 = 0;
const SEEK_CUR: u64 = 1;
const SEEK_END: u64 = 2;

/// Открытый файл: его содержимое целиком и текущая позиция чтения.
struct OpenFile {
    data: Vec<u8>,
    offset: usize,
}

/// Реестр таблиц дескрипторов по процессам: ключ — физический адрес PML4 (CR3) процесса.
static PROCESSES: Mutex<BTreeMap<u64, Vec<Option<OpenFile>>>> = Mutex::new(BTreeMap::new());

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

/// Ключ текущего процесса — физ. адрес его корня таблиц страниц.
fn current_key() -> u64 {
    crate::arch::context::current_address_space()
        .start_address()
        .as_u64()
}

/// Выполняет `f` над таблицей дескрипторов текущего процесса (создаёт пустую при первом
/// обращении). Безопасно без `without_interrupts`: реестр не трогают обработчики прерываний,
/// а syscall идёт с IF=0 (не реентерабелен).
fn with_current_fds<R>(f: impl FnOnce(&mut Vec<Option<OpenFile>>) -> R) -> R {
    let key = current_key();
    let mut guard = PROCESSES.lock();
    f(guard.entry(key).or_default())
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

/// `open(path, flags, mode)` — открывает файл и возвращает дескриптор. Флаги/режим пока
/// игнорируем (только чтение). `path` — нуль-терминированная строка в памяти пользователя.
pub fn sys_open(path_ptr: u64) -> i64 {
    let path = match uaccess::read_user_cstr(path_ptr) {
        Ok(p) => p,
        Err(errno) => return -errno,
    };
    let path = match core::str::from_utf8(&path) {
        Ok(s) => s,
        Err(_) => return -abi::ENOENT,
    };
    let data = match crate::fs::open(path) {
        Ok(d) => d,
        Err(_) => return -abi::ENOENT,
    };
    with_current_fds(|fds| alloc_fd(fds, OpenFile { data, offset: 0 }))
}

/// `read(fd, buf, count)` — копирует до `count` байт файла с текущей позиции в буфер
/// пользователя, сдвигает позицию, возвращает число прочитанных байт (0 — конец файла).
pub fn sys_read(fd: u64, buf: u64, count: u64) -> i64 {
    let fd = fd as usize;
    let count = count as usize;
    with_current_fds(|fds| {
        let file = match fds.get_mut(fd).and_then(|slot| slot.as_mut()) {
            Some(f) => f,
            None => return -abi::EBADF,
        };
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

/// `close(fd)` — освобождает дескриптор.
pub fn sys_close(fd: u64) -> i64 {
    let fd = fd as usize;
    with_current_fds(|fds| match fds.get_mut(fd) {
        Some(slot) if slot.is_some() => {
            *slot = None;
            0
        }
        _ => -abi::EBADF,
    })
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
