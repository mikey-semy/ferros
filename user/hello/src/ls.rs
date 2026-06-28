//! `ls` (M7f): печатает имена записей каталога (по одной в строке). Внешняя утилита `/bin`.
//! Без аргумента — текущий каталог (`.`), иначе — каталог-аргумент. Выход 0 при успехе.

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

const SYS_WRITE: u64 = 1;
const SYS_OPEN: u64 = 2;
const SYS_CLOSE: u64 = 3;
const SYS_EXIT: u64 = 60;
const SYS_GETDENTS64: u64 = 217;
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

#[no_mangle]
extern "C" fn argv_main(sp: *const u64) -> ! {
    let argc = unsafe { *sp } as usize;
    let argv = unsafe { sp.add(1) as *const *const u8 };

    // SAFETY: валидный argv от ядра.
    let dir = unsafe {
        if argc > 1 {
            *argv.add(1)
        } else {
            b".\0".as_ptr() // текущий каталог (резолвится ядром от cwd)
        }
    };

    // SAFETY: корректные сисколлы; dir/buf — валидные буферы.
    unsafe {
        let fd = sc3(SYS_OPEN, dir as u64, O_RDONLY, 0);
        if fd < 0 {
            write_str(b"ls: cannot open directory\n");
            sys_exit(1);
        }
        let mut buf = [0u8; 1024];
        loop {
            let n = sc3(
                SYS_GETDENTS64,
                fd as u64,
                buf.as_mut_ptr() as u64,
                buf.len() as u64,
            );
            if n < 0 {
                sys_exit(2);
            }
            if n == 0 {
                break; // записи кончились
            }
            // Идём по записям linux_dirent64: d_reclen (u16 @ +16), имя (@ +19, до нуля).
            let n = n as usize;
            let mut pos = 0usize;
            while pos + 19 <= n {
                let reclen = (buf[pos + 16] as usize) | ((buf[pos + 17] as usize) << 8);
                if reclen == 0 {
                    break; // защита от зацикливания на битой записи
                }
                let name_start = pos + 19;
                let mut len = 0usize;
                while name_start + len < n && buf[name_start + len] != 0 {
                    len += 1;
                }
                write_bytes(buf.as_ptr().add(name_start), len);
                write_str(b"\n");
                pos += reclen;
            }
        }
        sc3(SYS_CLOSE, fd as u64, 0, 0);
        sys_exit(0)
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
