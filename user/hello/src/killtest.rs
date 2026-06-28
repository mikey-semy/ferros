//! Пользовательская проверка `kill` + `wait4`-по-сигналу (M6f5): форкается, ребёнок крутится
//! вечно, родитель посылает ему `SIGTERM`, дожидается через `wait4` и проверяет, что ребёнок
//! завершён ИМЕННО сигналом `SIGTERM` (WIFSIGNALED, WTERMSIG == SIGTERM). Выходит с 0 (успех)
//! или 1 (что-то не так) — тест читает этот код.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

const SYS_FORK: u64 = 57;
const SYS_EXIT: u64 = 60;
const SYS_WAIT4: u64 = 61;
const SYS_KILL: u64 = 62;
const SIGTERM: u64 = 15;

/// `exit(code)` — не возвращается.
///
/// # Safety
/// Корректный номер вызова; управление не вернётся.
unsafe fn sys_exit(code: u64) -> ! {
    asm!("syscall", in("rax") SYS_EXIT, in("rdi") code, options(noreturn));
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let child: u64;
    // fork(): ребёнку вернётся 0, родителю — PID ребёнка.
    // SAFETY: fork без аргументов; помечаем затёртые инструкцией регистры.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") SYS_FORK => child,
            lateout("rcx") _,
            lateout("r11") _,
        );
    }

    if child == 0 {
        // Ребёнок: крутимся вечно, пока родитель не убьёт сигналом.
        loop {
            core::hint::spin_loop();
        }
    }

    // Родитель: посылаем ребёнку SIGTERM. kill(child, SIGTERM).
    let kill_ret: u64;
    // SAFETY: корректный вызов; помечаем затёртые регистры.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") SYS_KILL => kill_ret,
            in("rdi") child,
            in("rsi") SIGTERM,
            lateout("rcx") _,
            lateout("r11") _,
        );
    }

    // Ждём ребёнка: wait4(child, &status, 0, 0).
    let mut status: i32 = -1;
    let waited: u64;
    // SAFETY: status_ptr указывает на нашу валидную переменную; помечаем затёртые регистры.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") SYS_WAIT4 => waited,
            in("rdi") child,
            in("rsi") &mut status as *mut i32,
            in("rdx") 0u64,
            in("r10") 0u64,
            lateout("rcx") _,
            lateout("r11") _,
        );
    }

    // WIFSIGNALED: младшие 7 бит статуса = номер сигнала-убийцы (не 0 и не 0x7f).
    let term_sig = (status & 0x7f) as u64;
    let ok = kill_ret == 0 && waited == child && term_sig == SIGTERM;
    // SAFETY: выходим; 0 — успех (ребёнок убит SIGTERM и собран), 1 — иначе.
    unsafe { sys_exit(if ok { 0 } else { 1 }) }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
