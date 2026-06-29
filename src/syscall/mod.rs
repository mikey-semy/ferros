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
        abi::SYS_WRITE => files::sys_write(args[0], args[1], args[2], user_rsp),
        abi::SYS_OPEN => files::sys_open(args[0], args[1]),
        abi::SYS_READ => files::sys_read(args[0], args[1], args[2]),
        abi::SYS_CLOSE => files::sys_close(args[0]),
        abi::SYS_PIPE => files::sys_pipe(args[0]),
        abi::SYS_DUP2 => files::sys_dup2(args[0], args[1]),
        abi::SYS_GETDENTS64 => files::sys_getdents64(args[0], args[1], args[2]),
        abi::SYS_LSEEK => files::sys_lseek(args[0], args[1] as i64, args[2]),
        abi::SYS_CHDIR => files::sys_chdir(args[0]),
        abi::SYS_GETCWD => files::sys_getcwd(args[0], args[1]),
        abi::SYS_MKDIR => files::sys_mkdir(args[0], args[1]),
        abi::SYS_RMDIR => files::sys_rmdir(args[0]),
        abi::SYS_UNLINK => files::sys_unlink(args[0]),
        abi::SYS_GETPID => crate::sched::thread::current_pid() as i64,
        abi::SYS_WAIT4 => sys_wait4(args[0] as i64, args[1]),
        abi::SYS_KILL => sys_kill(args[0] as i64, args[1]),
        abi::SYS_EXIT | abi::SYS_EXIT_GROUP => {
            // Закрываем дескрипторы СИНХРОННО, пока процесс ещё жив (M7g1/M7g2): сбрасываем грязные
            // файлы (durable к возврату родителя из `wait`) и освобождаем концы каналов (другой
            // конец сразу видит EOF). Иначе это сделал бы reaper — асинхронно и слишком поздно.
            files::release_current_process_fds();
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

/// `wait4(pid, status, options, rusage)` (M6f4): ждёт завершения ребёнка. Поддержаны
/// `pid == -1` (любой ребёнок) и `pid > 0` (конкретный); `options`/`rusage` игнорируем.
/// Блокирует вызывающего, пока подходящий ребёнок не завершится; возвращает его PID и пишет
/// закодированный статус в `*status` (если не NULL). `-ECHILD`, если подходящих детей нет.
fn sys_wait4(pid: i64, status_ptr: u64) -> i64 {
    match crate::sched::thread::wait_current(pid) {
        Some((child_pid, code, signal)) => {
            if status_ptr != 0 {
                // Linux-кодировка статуса. Убит сигналом: младшие 7 бит = номер сигнала
                // (WIFSIGNALED, WTERMSIG = status & 0x7f). Обычный `exit`: младшие 7 бит = 0,
                // код выхода — в битах 8..15 (WIFEXITED, WEXITSTATUS = (status >> 8) & 0xff).
                let encoded = match signal {
                    Some(sig) => sig as u32,
                    None => ((code & 0xff) << 8) as u32,
                };
                if let Err(errno) = uaccess::copy_to_user(status_ptr, &encoded.to_ne_bytes()) {
                    return -errno;
                }
            }
            child_pid as i64
        }
        None => -abi::ECHILD,
    }
}

/// `kill(pid, sig)` (M6f5): посылает сигнал `sig` процессу `pid`. Поддержан `pid > 0`
/// (конкретный процесс); группы процессов и широковещание (`pid <= 0`) — нет. `sig == 0` —
/// проверка существования (Linux). Сигналы с действием по умолчанию «завершить» завершают цель
/// сразу (своих обработчиков пока нет). Возвращает 0 или `-errno`.
fn sys_kill(pid: i64, sig: u64) -> i64 {
    if sig > abi::SIG_MAX {
        return -abi::EINVAL;
    }
    if pid <= 0 {
        // Группы процессов (pid <= 0) не поддержаны — для нас «нет такого процесса».
        return -abi::ESRCH;
    }
    match crate::sched::thread::signal(pid as u32, sig as u8) {
        crate::sched::thread::SignalOutcome::Delivered => 0,
        crate::sched::thread::SignalOutcome::NoSuchProcess => -abi::ESRCH,
    }
}

/// Фиксирует параметры `write` для тестовой наблюдаемости (см. статики выше). Зовётся файловым
/// слоем при записи на устройство (stdout/stderr) — вся диспетчеризация `write` теперь там (M7g1).
pub(crate) fn record_write(fd: u64, bytes: &[u8], user_rsp: u64) {
    LAST_WRITE_FD.store(fd, Ordering::SeqCst);
    LAST_WRITE_LEN.store(bytes.len() as u64, Ordering::SeqCst);
    LAST_WRITE_SUM.store(bytes.iter().map(|&b| b as u64).sum(), Ordering::SeqCst);
    LAST_WRITE_USER_RSP.store(user_rsp, Ordering::SeqCst);
}
