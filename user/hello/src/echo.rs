//! `echo` (M7f): печатает свои аргументы через пробел и перевод строки. Внешняя утилита `/bin`,
//! запускается shell'ом через fork/execve.

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

const SYS_WRITE: u64 = 1;
const SYS_EXIT: u64 = 60;

// Точка входа на ассемблере: ловит начальный rsp (указывает на argc) до пролога.
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

/// Длина нуль-терминированной строки.
///
/// # Safety
/// `p` указывает на читаемую нуль-терминированную строку.
unsafe fn strlen(p: *const u8) -> usize {
    let mut n = 0;
    while *p.add(n) != 0 {
        n += 1;
    }
    n
}

fn write_bytes(p: *const u8, len: usize) {
    // SAFETY: write в stdout с валидным буфером.
    unsafe {
        sc3(SYS_WRITE, 1, p as u64, len as u64);
    }
}

/// Пишет срез в stdout — длина берётся из самого среза (никаких ручных счётчиков длины).
fn write_str(s: &[u8]) {
    write_bytes(s.as_ptr(), s.len());
}

#[no_mangle]
extern "C" fn argv_main(sp: *const u64) -> ! {
    let argc = unsafe { *sp } as usize;
    let argv = unsafe { sp.add(1) as *const *const u8 };

    // SAFETY: ядро построило валидный argv; читаем argc указателей.
    unsafe {
        let mut i = 1;
        while i < argc {
            if i > 1 {
                write_str(b" ");
            }
            let arg = *argv.add(i);
            write_bytes(arg, strlen(arg));
            i += 1;
        }
        write_str(b"\n");
        sys_exit(0)
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
