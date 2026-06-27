//! Пользовательская программа-проверка `execve` (M6f2): заменяет себя программой `HELLO` с
//! диска. При успехе управление сюда не возвращается (процесс становится `hello`); если
//! `execve` не удался — завершаемся с кодом 42, чтобы тест это увидел.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

const SYS_EXECVE: u64 = 59;
const SYS_EXIT: u64 = 60;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let path = b"HELLO\0";

    // execve(path, NULL, NULL) — argv/envp пока не нужны.
    // SAFETY: путь — корректная нуль-терминированная строка; при успехе не возвращается.
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_EXECVE,
            in("rdi") path.as_ptr(),
            in("rsi") 0u64,
            in("rdx") 0u64,
            lateout("rax") _,
            lateout("rcx") _,
            lateout("r11") _,
        );
    }

    // Сюда попадаем только если execve не удался.
    // SAFETY: exit; управление не вернётся.
    unsafe {
        asm!("syscall", in("rax") SYS_EXIT, in("rdi") 42u64, options(noreturn));
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
