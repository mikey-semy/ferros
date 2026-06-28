//! `syscall` — граница ядро/пользователь и диспетчер системных вызовов (M5).
//!
//! # Что это
//!
//! Когда программа в кольце 3 исполняет инструкцию `syscall`, процессор прыгает в ядро,
//! арх-трамплин ([`crate::arch::x86_64::syscall`]) сохраняет регистры и зовёт [`dispatch`]
//! отсюда. Это переносимая (арх-независимая) «телефонистка»: по номеру вызова направляет
//! в нужный обработчик и возвращает результат.
//!
//! # Linux-форма (D8)
//!
//! Номера и семантика — из таблицы **Linux x86-64** ([`abi`]); тот же слой потом понесёт
//! и source-level POSIX (relibc, M9), и ABI-совместимость. Аргументы приходят уже
//! разложенными по Linux-ABI (`rdi, rsi, rdx, r10, r8, r9`); результат — `i64`, где
//! отрицательное значение есть `-errno`.
//!
//! # Опасное — за швами
//!
//! Доступ к памяти пользователя идёт только через [`uaccess`] (там собран весь сырой
//! `unsafe`, D9); вывод — через драйверы. Здесь — чистая безопасная логика.

pub mod abi;
pub mod elf;
pub mod files;
pub mod uaccess;

use crate::drivers::{serial, vga};
use core::sync::atomic::{AtomicI64, AtomicU64, Ordering};

// --- Наблюдаемость для тестов M5b (последний обработанный write/exit) ---

/// Файловый дескриптор последнего `write` (тест сверяет, что это был stdout=1).
pub static LAST_WRITE_FD: AtomicU64 = AtomicU64::new(0);
/// Длина последнего `write`.
pub static LAST_WRITE_LEN: AtomicU64 = AtomicU64::new(0);
/// Сумма байт последнего `write` (дешёвая проверка, что из памяти пользователя прочиталось
/// именно то, что нужно).
pub static LAST_WRITE_SUM: AtomicU64 = AtomicU64::new(0);
/// `user_rsp` на момент последнего `write` (тест сверяет с вершиной user-стека — значит
/// код реально шёл в кольце 3 на своём стеке).
pub static LAST_WRITE_USER_RSP: AtomicU64 = AtomicU64::new(0);
/// Код последнего `exit`/`exit_group`.
pub static LAST_EXIT_CODE: AtomicI64 = AtomicI64::new(-1);
/// Сумма кодов всех `exit`/`exit_group` с загрузки. Когда завершаются несколько процессов и
/// порядок недетерминирован (например, родитель и ребёнок после `fork`), сумма позволяет
/// проверить НАБОР кодов независимо от порядка (тест M6f3).
pub static EXIT_CODE_SUM: AtomicI64 = AtomicI64::new(0);
/// Сколько раз вызывался `exit`/`exit_group` (тест проверяет, что завершение случилось).
pub static EXIT_CALLS: AtomicU64 = AtomicU64::new(0);
/// Сколько пользовательских процессов было завершено из-за сбоя в кольце 3 (page fault /
/// general protection fault). Тест M5c3b проверяет, что сбой убивает процесс, а не ядро.
pub static USER_FAULT_KILLS: AtomicU64 = AtomicU64::new(0);

/// Диспетчер системных вызовов: по номеру `nr` (Linux x86-64) направляет в обработчик.
/// `args` уже разложены по Linux-ABI: `[rdi, rsi, rdx, r10, r8, r9]`. `user_rsp` —
/// указатель стека пользователя на момент вызова. Возвращает результат в `rax`-конвенции
/// (≥0 — успех, отрицательное — `-errno`). Для `exit` НЕ возвращается (поток завершается).
pub fn dispatch(nr: u64, args: [u64; 6], user_rsp: u64) -> i64 {
    match nr {
        abi::SYS_WRITE => sys_write(args[0], args[1], args[2], user_rsp),
        abi::SYS_OPEN => files::sys_open(args[0]),
        abi::SYS_READ => files::sys_read(args[0], args[1], args[2]),
        abi::SYS_CLOSE => files::sys_close(args[0]),
        abi::SYS_LSEEK => files::sys_lseek(args[0], args[1] as i64, args[2]),
        abi::SYS_GETPID => crate::sched::thread::current_pid() as i64,
        abi::SYS_EXIT | abi::SYS_EXIT_GROUP => {
            let status = args[0] as i32;
            LAST_EXIT_CODE.store(status as i64, Ordering::SeqCst);
            EXIT_CODE_SUM.fetch_add(status as i64, Ordering::SeqCst);
            EXIT_CALLS.fetch_add(1, Ordering::SeqCst);
            // Завершаем текущий поток (с кодом возврата): планировщик пометит его мёртвым и
            // уйдёт на другой. Не возвращается — в `rax` ничего не кладётся, `sysret` не будет.
            crate::sched::thread::exit_current(status);
        }
        // Неизвестный номер — как в Linux: -ENOSYS.
        _ => -abi::ENOSYS,
    }
}

/// `write(fd, buf, count)`: пишет `count` байт из пользовательского буфера `buf` в `fd`.
/// Поддержаны `fd=1` (stdout → VGA) и `fd=2` (stderr → serial); прочее → `-EBADF`.
/// Возвращает число записанных байт или `-errno`.
fn sys_write(fd: u64, buf: u64, count: u64, user_rsp: u64) -> i64 {
    // Куда выводим — решаем по fd ДО чтения памяти пользователя.
    let sink: fn(&[u8]) = match fd {
        1 => vga::write_bytes,
        2 => serial::write_bytes,
        _ => return -abi::EBADF,
    };

    match uaccess::with_user_bytes(buf, count, |bytes| {
        sink(bytes);
        record_write(fd, bytes, user_rsp);
        bytes.len() as i64
    }) {
        Ok(written) => written,
        Err(errno) => -errno,
    }
}

/// Фиксирует параметры `write` для тестовой наблюдаемости (см. статики выше).
fn record_write(fd: u64, bytes: &[u8], user_rsp: u64) {
    LAST_WRITE_FD.store(fd, Ordering::SeqCst);
    LAST_WRITE_LEN.store(bytes.len() as u64, Ordering::SeqCst);
    LAST_WRITE_SUM.store(bytes.iter().map(|&b| b as u64).sum(), Ordering::SeqCst);
    LAST_WRITE_USER_RSP.store(user_rsp, Ordering::SeqCst);
}
