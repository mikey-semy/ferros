//! Пользовательский запускатель (M7b): `execve("ARGVECHO", ["ARGVECHO","ping","pong"], [])`.
//! При успехе образ заменяется на `ARGVECHO` (с диска), который проверит полученные argv и
//! завершится с 0; сюда управление не вернётся. Если `execve` не удался — выходим с 42.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;
use core::ptr;

const SYS_EXECVE: u64 = 59;
const SYS_EXIT: u64 = 60;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let path = b"ARGVECHO\0";
    // Аргументы: argv[0] — имя программы (как в Unix), затем два «полезных» аргумента.
    let a0 = b"ARGVECHO\0";
    let a1 = b"ping\0";
    let a2 = b"pong\0";
    let argv = [a0.as_ptr(), a1.as_ptr(), a2.as_ptr(), ptr::null()];
    let envp = [ptr::null::<u8>()]; // пустое окружение (только NULL-терминатор)

    // execve(path, argv, envp): при успехе не возвращается.
    // SAFETY: path/argv/envp — валидные NULL-терминированные структуры в нашей памяти.
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_EXECVE,
            in("rdi") path.as_ptr(),
            in("rsi") argv.as_ptr(),
            in("rdx") envp.as_ptr(),
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
