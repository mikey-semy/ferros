//! Проверка `stat`/`fstat` (M9c). Встраивается в ядро; тест `tests/stat.rs` запускает её и ждёт
//! код выхода 0. Любое несовпадение — свой код (10..21).
//!
//! Что доказываем: `stat` по пути отдаёт тип (обычный/каталог) и размер; `fstat` по дескриптору
//! различает подложку (stdout — символьное устройство → так работает `isatty`; открытый файл —
//! обычный, с тем же размером, что и `stat`).

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

const SYS_OPEN: u64 = 2;
const SYS_STAT: u64 = 4;
const SYS_FSTAT: u64 = 5;
const SYS_EXIT: u64 = 60;

const O_RDONLY: u64 = 0;

// Биты типа файла в st_mode (Linux).
const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;
const S_IFCHR: u32 = 0o020000;

/// Размер `struct stat` (x86-64) и смещения нужных полей.
const STAT_SIZE: usize = 144;
const OFF_MODE: usize = 24; // st_mode (u32)
const OFF_SIZE: usize = 48; // st_size (u64)

/// Содержимое `/HELLO.TXT` из build.rs — `b"ferros M6c: hello from FAT32!\n"`, 30 байт. (Разные
/// крейты, общую константу не пошарить — как и в tests/fat_read.rs; менять синхронно с build.rs.)
const HELLO_SIZE: u64 = 30;

global_asm!(".global _start", "_start:", "    call main");

/// Сисколл с двумя аргументами (open/stat/fstat).
fn sc2(nr: u64, a: u64, b: u64) -> i64 {
    let ret: i64;
    // SAFETY: аргументы в rdi/rsi; rcx/r11 — затираемые syscall'ом.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") nr => ret,
            in("rdi") a, in("rsi") b,
            lateout("rcx") _, lateout("r11") _,
        );
    }
    ret
}

/// # Safety
/// Не возвращается.
unsafe fn sys_exit(code: u64) -> ! {
    asm!("syscall", in("rax") SYS_EXIT, in("rdi") code, options(noreturn));
}

/// `st_mode` (u32) из заполненного буфера `struct stat`.
fn stat_mode(buf: &[u8; STAT_SIZE]) -> u32 {
    u32::from_le_bytes([
        buf[OFF_MODE],
        buf[OFF_MODE + 1],
        buf[OFF_MODE + 2],
        buf[OFF_MODE + 3],
    ])
}

/// `st_size` (u64) из заполненного буфера `struct stat`.
fn stat_size(buf: &[u8; STAT_SIZE]) -> u64 {
    let mut s = [0u8; 8];
    s.copy_from_slice(&buf[OFF_SIZE..OFF_SIZE + 8]);
    u64::from_le_bytes(s)
}

#[no_mangle]
extern "C" fn main() -> ! {
    let mut buf = [0u8; STAT_SIZE];
    let buf_ptr = buf.as_mut_ptr() as u64;

    // SAFETY: пути — статические C-строки с нулём; буфер достаточного размера.
    unsafe {
        // 1) stat обычного файла: тип REG, размер совпадает с содержимым фикстуры.
        if sc2(SYS_STAT, b"/HELLO.TXT\0".as_ptr() as u64, buf_ptr) != 0 {
            sys_exit(10);
        }
        if stat_mode(&buf) & S_IFMT != S_IFREG {
            sys_exit(11);
        }
        if stat_size(&buf) != HELLO_SIZE {
            sys_exit(12);
        }

        // 2) stat каталога: тип DIR.
        if sc2(SYS_STAT, b"/SUB\0".as_ptr() as u64, buf_ptr) != 0 {
            sys_exit(13);
        }
        if stat_mode(&buf) & S_IFMT != S_IFDIR {
            sys_exit(14);
        }

        // 3) fstat(stdout): символьное устройство (на этом стоит isatty).
        if sc2(SYS_FSTAT, 1, buf_ptr) != 0 {
            sys_exit(15);
        }
        if stat_mode(&buf) & S_IFMT != S_IFCHR {
            sys_exit(16);
        }

        // 4) fstat открытого файла: тип REG и тот же размер, что отдал stat.
        let fd = sc2(SYS_OPEN, b"/HELLO.TXT\0".as_ptr() as u64, O_RDONLY);
        if fd < 0 {
            sys_exit(17);
        }
        if sc2(SYS_FSTAT, fd as u64, buf_ptr) != 0 {
            sys_exit(18);
        }
        if stat_mode(&buf) & S_IFMT != S_IFREG {
            sys_exit(19);
        }
        if stat_size(&buf) != HELLO_SIZE {
            sys_exit(20);
        }

        sys_exit(0)
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
