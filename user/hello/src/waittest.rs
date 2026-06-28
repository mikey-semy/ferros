//! Пользовательская проверка `fork` + `wait4` (M6f4): форкается; ребёнок выходит с кодом 7;
//! родитель через `wait4` дожидается ребёнка, проверяет, что получил его PID и код выхода 7, и
//! завершается с 0 (успех) или 1 (что-то не так). Так тест по последнему коду выхода (родитель
//! завершается последним — он ждёт) видит, что `wait` корректно собрал PID и статус ребёнка.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

const SYS_FORK: u64 = 57;
const SYS_EXIT: u64 = 60;
const SYS_WAIT4: u64 = 61;

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
        // SAFETY: ребёнок — выходим с отличимым кодом 7.
        unsafe { sys_exit(7) }
    }

    // Родитель: ждём ребёнка. wait4(-1, &status, 0, 0) → PID завершившегося; status кодирует код.
    let mut status: i32 = -1;
    let waited: u64;
    // SAFETY: status_ptr указывает на нашу валидную переменную; помечаем затёртые регистры.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") SYS_WAIT4 => waited,
            in("rdi") (-1i64) as u64,            // pid = -1 → любой ребёнок
            in("rsi") &mut status as *mut i32,   // куда записать статус
            in("rdx") 0u64,                      // options
            in("r10") 0u64,                      // rusage (4-й аргумент Linux — в r10)
            lateout("rcx") _,
            lateout("r11") _,
        );
    }

    // Linux-кодировка: код выхода — в битах 8..15.
    let child_code = (status >> 8) & 0xff;
    let ok = waited == child && child_code == 7;
    // SAFETY: выходим; 0 — успех (wait вернул верные PID и код), 1 — иначе.
    unsafe { sys_exit(if ok { 0 } else { 1 }) }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
