//! `rmdir` (M7g3): удаляет ПУСТЫЕ каталоги-аргументы через `rmdir(2)`. Внешняя утилита `/bin`.
//! Выход 0 при успехе, 1 — если хоть один не удалился (нет / это файл / каталог не пуст).

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

const SYS_WRITE: u64 = 1;
const SYS_EXIT: u64 = 60;
const SYS_RMDIR: u64 = 84;

global_asm!(
    ".global _start",
    "_start:",
    "    mov rdi, rsp",
    "    call argv_main",
);

/// # Safety
/// Номер/аргументы должны соответствовать сисколлу.
unsafe fn sc3(nr: u64, a: u64, b: u64, c: u64) -> i64 {
    let ret: i64;
    asm!(
        "syscall",
        inlateout("rax") nr => ret,
        in("rdi") a, in("rsi") b, in("rdx") c,
        lateout("rcx") _, lateout("r11") _,
    );
    ret
}

/// # Safety
/// Не возвращается.
unsafe fn sys_exit(code: u64) -> ! {
    asm!("syscall", in("rax") SYS_EXIT, in("rdi") code, options(noreturn));
}

fn write_str(s: &[u8]) {
    // SAFETY: write в stdout.
    unsafe {
        sc3(SYS_WRITE, 1, s.as_ptr() as u64, s.len() as u64);
    }
}

#[no_mangle]
extern "C" fn argv_main(sp: *const u64) -> ! {
    let argc = unsafe { *sp } as usize;
    let argv = unsafe { sp.add(1) as *const *const u8 };

    // SAFETY: валидный argv от ядра.
    unsafe {
        if argc < 2 {
            write_str(b"usage: rmdir DIR...\n");
            sys_exit(1);
        }
        let mut code = 0u64;
        let mut i = 1;
        while i < argc {
            if sc3(SYS_RMDIR, *argv.add(i) as u64, 0, 0) < 0 {
                write_str(b"rmdir: cannot remove directory\n");
                code = 1;
            }
            i += 1;
        }
        sys_exit(code)
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
