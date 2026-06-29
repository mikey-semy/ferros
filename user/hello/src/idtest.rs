//! Проверка информационных сисколлов (M9e): `getuid`/`geteuid`/`getgid`/`getegid`/`getppid` и
//! `uname`. Встраивается в ядро; тест `tests/ids.rs` запускает её и ждёт код выхода 0. Любое
//! несовпадение — свой код (10..17).

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

const SYS_UNAME: u64 = 63;
const SYS_GETUID: u64 = 102;
const SYS_GETGID: u64 = 104;
const SYS_GETEUID: u64 = 107;
const SYS_GETEGID: u64 = 108;
const SYS_GETPPID: u64 = 110;
const SYS_EXIT: u64 = 60;

/// Раскладка `struct utsname`: 6 полей по 65 байт.
const UTS_FIELD: usize = 65;
const UTSNAME_SIZE: usize = UTS_FIELD * 6;
const OFF_SYSNAME: usize = 0;
const OFF_MACHINE: usize = 4 * UTS_FIELD; // 5-е поле (sysname,nodename,release,version,machine)

global_asm!(".global _start", "_start:", "    call main");

/// Сисколл без аргументов (идентификаторы).
fn sc0(nr: u64) -> i64 {
    let ret: i64;
    // SAFETY: без аргументов; rcx/r11 — затираемые syscall'ом.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") nr => ret,
            lateout("rcx") _, lateout("r11") _,
        );
    }
    ret
}

/// Сисколл с одним аргументом (`uname`).
fn sc1(nr: u64, a: u64) -> i64 {
    let ret: i64;
    // SAFETY: один аргумент в rdi; rcx/r11 — затираемые syscall'ом.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") nr => ret,
            in("rdi") a,
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

/// Начинается ли поле utsname по смещению `off` с C-строки `s`.
fn field_is(buf: &[u8; UTSNAME_SIZE], off: usize, s: &[u8]) -> bool {
    buf[off..off + s.len()] == *s && buf[off + s.len()] == 0
}

#[no_mangle]
extern "C" fn main() -> ! {
    // SAFETY: системные вызовы; буфер uname достаточного размера.
    unsafe {
        // Однопользовательская система — всё от root (0).
        if sc0(SYS_GETUID) != 0 {
            sys_exit(10);
        }
        if sc0(SYS_GETEUID) != 0 {
            sys_exit(11);
        }
        if sc0(SYS_GETGID) != 0 {
            sys_exit(12);
        }
        if sc0(SYS_GETEGID) != 0 {
            sys_exit(13);
        }
        // Этот процесс спавнит «нулевой» поток ядра (PID 0), поэтому родитель — 0.
        if sc0(SYS_GETPPID) != 0 {
            sys_exit(14);
        }

        // uname: sysname == "ferros", machine == "x86_64".
        let mut uts = [0u8; UTSNAME_SIZE];
        if sc1(SYS_UNAME, uts.as_mut_ptr() as u64) != 0 {
            sys_exit(15);
        }
        if !field_is(&uts, OFF_SYSNAME, b"ferros") {
            sys_exit(16);
        }
        if !field_is(&uts, OFF_MACHINE, b"x86_64") {
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
