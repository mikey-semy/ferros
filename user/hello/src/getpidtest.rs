//! Пользовательская программа-проверка `getpid` (M6f1): спрашивает свой PID и завершается с
//! ним как кодом возврата — чтобы ядро (тест) могло сверить присвоенный процессу PID.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

const SYS_GETPID: u64 = 39;
const SYS_EXIT: u64 = 60;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // pid = getpid() — без аргументов; результат в rax.
    // SAFETY: `syscall` с корректным номером; rcx/r11 затираются инструкцией.
    let pid: u64;
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_GETPID,
            lateout("rax") pid,
            lateout("rcx") _,
            lateout("r11") _,
        );
    }

    // exit(pid) — код возврата равен нашему PID.
    // SAFETY: `syscall` exit; управление не вернётся.
    unsafe {
        asm!("syscall", in("rax") SYS_EXIT, in("rdi") pid, options(noreturn));
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
