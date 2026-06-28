//! Пользовательская проверка `fork` (M6f3): форкается; ребёнок (`fork` вернул 0) выходит с
//! кодом 7, родитель (`fork` вернул PID ребёнка) выходит с этим PID. Тест M6f3 проверяет, что
//! завершились ДВА процесса и сумма кодов = 7 + child_pid — это доказывает разделение возврата
//! `fork` (ребёнок получил 0, родитель — ненулевой PID) и что у ребёнка своё исполнение.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

const SYS_FORK: u64 = 57;
const SYS_EXIT: u64 = 60;

/// `exit(code)` — не возвращается.
///
/// # Safety
/// Корректный номер вызова; управление не вернётся.
unsafe fn sys_exit(code: u64) -> ! {
    asm!("syscall", in("rax") SYS_EXIT, in("rdi") code, options(noreturn));
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let pid: u64;
    // fork(): ребёнку вернётся 0, родителю — PID ребёнка. `syscall` затирает rcx и r11.
    // SAFETY: fork без аргументов; помечаем затёртые инструкцией регистры.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") SYS_FORK => pid,
            lateout("rcx") _,
            lateout("r11") _,
        );
    }

    if pid == 0 {
        // SAFETY: ребёнок — выходим с отличимым кодом 7.
        unsafe { sys_exit(7) }
    } else {
        // SAFETY: родитель — выходим с PID ребёнка.
        unsafe { sys_exit(pid) }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
