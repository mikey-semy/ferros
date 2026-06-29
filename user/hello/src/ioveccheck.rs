//! Проверка `writev`/`readv`/`fcntl` (M9f). Встраивается в ядро; тест `tests/iovec.rs` запускает её
//! и ждёт код выхода 0. Любое несовпадение — свой код (10..17).
//!
//! Самодостаточно через канал (без диска): `writev` нескольких буферов в конец записи, чтение
//! обратно из конца чтения и сверка; затем `readv` в несколько буферов; затем `fcntl` (`F_GETFL`
//! и `F_DUPFD` — продублированный конец чтения реально работает).

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

const SYS_READ: u64 = 0;
const SYS_READV: u64 = 19;
const SYS_WRITEV: u64 = 20;
const SYS_PIPE: u64 = 22;
const SYS_FCNTL: u64 = 72;
const SYS_EXIT: u64 = 60;

const F_DUPFD: u64 = 0;
const F_GETFL: u64 = 3;

global_asm!(".global _start", "_start:", "    call main");

fn sc1(nr: u64, a: u64) -> i64 {
    let ret: i64;
    // SAFETY: один аргумент в rdi; rcx/r11 затираются syscall'ом.
    unsafe {
        asm!("syscall", inlateout("rax") nr => ret, in("rdi") a,
             lateout("rcx") _, lateout("r11") _);
    }
    ret
}

fn sc3(nr: u64, a: u64, b: u64, c: u64) -> i64 {
    let ret: i64;
    // SAFETY: три аргумента в rdi/rsi/rdx; rcx/r11 затираются syscall'ом.
    unsafe {
        asm!("syscall", inlateout("rax") nr => ret, in("rdi") a, in("rsi") b, in("rdx") c,
             lateout("rcx") _, lateout("r11") _);
    }
    ret
}

/// # Safety
/// Не возвращается.
unsafe fn sys_exit(code: u64) -> ! {
    asm!("syscall", in("rax") SYS_EXIT, in("rdi") code, options(noreturn));
}

/// `writev(fd, &[(base,len); 2])` из двух срезов.
fn writev2(fd: i32, a: &[u8], b: &[u8]) -> i64 {
    let iov: [u64; 4] = [
        a.as_ptr() as u64,
        a.len() as u64,
        b.as_ptr() as u64,
        b.len() as u64,
    ];
    sc3(SYS_WRITEV, fd as u64, iov.as_ptr() as u64, 2)
}

#[no_mangle]
extern "C" fn main() -> ! {
    let mut fds = [0i32; 2];

    // SAFETY: указатели — на локальные буферы достаточного размера.
    unsafe {
        if sc1(SYS_PIPE, fds.as_mut_ptr() as u64) != 0 {
            sys_exit(10);
        }
        let (r, w) = (fds[0], fds[1]);

        // 1) writev двух буферов → 14 байт; читаем обратно и сверяем «Hello, writev!».
        if writev2(w, b"Hello, ", b"writev!") != 14 {
            sys_exit(11);
        }
        let mut buf = [0u8; 14];
        if sc3(SYS_READ, r as u64, buf.as_mut_ptr() as u64, 14) != 14 || buf != *b"Hello, writev!" {
            sys_exit(12);
        }

        // 2) readv в два буфера: пишем «abcde», читаем в [2][3] → «ab»+«cde».
        if writev2(w, b"ab", b"cde") != 5 {
            sys_exit(13);
        }
        let mut p2 = [0u8; 2];
        let mut p3 = [0u8; 3];
        let iov: [u64; 4] = [
            p2.as_mut_ptr() as u64,
            2,
            p3.as_mut_ptr() as u64,
            3,
        ];
        if sc3(SYS_READV, r as u64, iov.as_ptr() as u64, 2) != 5
            || p2 != *b"ab"
            || p3 != *b"cde"
        {
            sys_exit(14);
        }

        // 3) fcntl(F_GETFL) на конце записи — режим доступа неотрицателен.
        if sc3(SYS_FCNTL, w as u64, F_GETFL, 0) < 0 {
            sys_exit(15);
        }

        // 4) fcntl(F_DUPFD): дубль конца чтения ≥ 8 и он реально читает из канала.
        let r2 = sc3(SYS_FCNTL, r as u64, F_DUPFD, 8);
        if r2 < 8 {
            sys_exit(16);
        }
        if writev2(w, b"Z", b"") != 1 {
            sys_exit(17);
        }
        let mut one = [0u8; 1];
        if sc3(SYS_READ, r2 as u64, one.as_mut_ptr() as u64, 1) != 1 || one[0] != b'Z' {
            sys_exit(17);
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
