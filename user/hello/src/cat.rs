//! `cat` (M7f): печатает содержимое файлов-аргументов в stdout. Внешняя утилита `/bin`.
//! Требует хотя бы один файл (чтение stdin без аргументов — позже). Выход 0 при успехе, 1 —
//! если какой-то файл не открылся.

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

const SYS_READ: u64 = 0;
const SYS_WRITE: u64 = 1;
const SYS_OPEN: u64 = 2;
const SYS_CLOSE: u64 = 3;
const SYS_EXIT: u64 = 60;
const O_RDONLY: u64 = 0;

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

fn write_bytes(p: *const u8, len: usize) {
    // SAFETY: write в stdout.
    unsafe {
        sc3(SYS_WRITE, 1, p as u64, len as u64);
    }
}

/// Пишет срез в stdout — длина из самого среза (без ручных счётчиков длины).
fn write_str(s: &[u8]) {
    write_bytes(s.as_ptr(), s.len());
}

/// Печатает один файл по пути. Возвращает `true` при успехе.
///
/// # Safety
/// `path` — читаемая нуль-терминированная строка.
unsafe fn cat_file(path: *const u8) -> bool {
    let fd = sc3(SYS_OPEN, path as u64, O_RDONLY, 0);
    if fd < 0 {
        write_str(b"cat: cannot open file\n");
        return false;
    }
    let mut buf = [0u8; 512];
    loop {
        let n = sc3(SYS_READ, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64);
        if n <= 0 {
            break; // 0 — конец файла, <0 — ошибка (прекращаем)
        }
        write_bytes(buf.as_ptr(), n as usize);
    }
    sc3(SYS_CLOSE, fd as u64, 0, 0);
    true
}

#[no_mangle]
extern "C" fn argv_main(sp: *const u64) -> ! {
    let argc = unsafe { *sp } as usize;
    let argv = unsafe { sp.add(1) as *const *const u8 };

    // SAFETY: валидный argv от ядра.
    unsafe {
        if argc < 2 {
            write_str(b"usage: cat FILE...\n");
            sys_exit(1);
        }
        let mut code = 0u64;
        let mut i = 1;
        while i < argc {
            if !cat_file(*argv.add(i)) {
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
