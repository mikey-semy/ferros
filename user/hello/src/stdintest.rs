//! Пользовательская проверка stdin (M7a): читает строку с клавиатуры через `read(0)` и
//! печатает её обратно в stdout, затем завершается с 0.
//!
//! `read(0, buf, …)` блокируется в ядре, пока линейная дисциплина не завершит строку (Enter);
//! затем отдаёт её байты. Это первая программа, получающая ввод из кольца 3 — фундамент под
//! интерактивный shell (M7). Выход: 0 при успехе, 1 — если `read` вернул ошибку/0 байт.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

const SYS_READ: u64 = 0;
const SYS_WRITE: u64 = 1;
const SYS_EXIT: u64 = 60;

/// Системный вызов с тремя аргументами (Linux x86-64 ABI); возвращает `rax` как `i64`.
///
/// # Safety
/// Номер и аргументы должны соответствовать вызываемому сисколлу.
unsafe fn syscall3(nr: u64, a: u64, b: u64, c: u64) -> i64 {
    let ret: i64;
    asm!(
        "syscall",
        inlateout("rax") nr => ret,
        in("rdi") a,
        in("rsi") b,
        in("rdx") c,
        lateout("rcx") _,
        lateout("r11") _,
    );
    ret
}

/// `exit(code)` — не возвращается.
///
/// # Safety
/// Управление в программу уже не вернётся.
unsafe fn sys_exit(code: u64) -> ! {
    asm!("syscall", in("rax") SYS_EXIT, in("rdi") code, options(noreturn));
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let mut buf = [0u8; 128];

    // SAFETY: buf — наш буфер на стеке; ядро запишет в него прочитанную строку.
    let code = unsafe {
        // read(0, buf, buf.len()) — блокируется до завершённой строки.
        let n = syscall3(SYS_READ, 0, buf.as_mut_ptr() as u64, buf.len() as u64);
        if n <= 0 {
            sys_exit(1);
        }
        // write(1, buf, n) — печатаем прочитанное обратно (видно на экране + наблюдаемо в тесте).
        syscall3(SYS_WRITE, 1, buf.as_ptr() as u64, n as u64);
        0
    };

    // SAFETY: завершаемся итоговым кодом.
    unsafe { sys_exit(code) }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
