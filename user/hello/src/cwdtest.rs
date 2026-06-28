//! Пользовательская проверка cwd (M7c): прогоняет `getcwd`/`chdir`/относительный `open`/`..`.
//!
//! Сценарий: `getcwd` == `/` → `chdir("SUB")` → `getcwd` == `/SUB` → относительный
//! `open("INSIDE.TXT")` (резолвится под `/SUB`) и чтение → `chdir("..")` → `getcwd` == `/`.
//! Выход 0 при успехе, иначе отличимый код 1–7 на конкретном шаге. На диске должны быть каталог
//! `SUB` и файл `SUB/INSIDE.TXT` (их кладёт build.rs).

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

const SYS_READ: u64 = 0;
const SYS_OPEN: u64 = 2;
const SYS_GETCWD: u64 = 79;
const SYS_CHDIR: u64 = 80;
const SYS_EXIT: u64 = 60;
const O_RDONLY: u64 = 0;

/// Ожидаемое содержимое `SUB/INSIDE.TXT` (держать синхронно с build.rs).
const INSIDE: &[u8] = b"inside SUB\n";

/// Системный вызов с тремя аргументами; возвращает `rax`. Для вызовов с меньшим числом
/// аргументов лишние просто игнорируются ядром.
///
/// # Safety
/// Номер и аргументы должны соответствовать вызываемому сисколлу.
unsafe fn syscall3(nr: u64, a: u64, b: u64, c: u64) -> i64 {
    let ret: i64;
    asm!(
        "syscall",
        inlateout("rax") nr => ret,
        in("rdi") a,
        in("rsi") b,
        in("rdx") c,
        lateout("rcx") _,
        lateout("r11") _,
    );
    ret
}

/// `exit(code)` — не возвращается.
///
/// # Safety
/// Управление в программу уже не вернётся.
unsafe fn sys_exit(code: u64) -> ! {
    asm!("syscall", in("rax") SYS_EXIT, in("rdi") code, options(noreturn));
}

/// `getcwd` записал ровно `expected` (+ нуль): возврат = длина с нулём, а байты совпадают.
fn cwd_eq(buf: &[u8], n: i64, expected: &[u8]) -> bool {
    n >= 1 && n as usize == expected.len() + 1 && &buf[..expected.len()] == expected
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let mut buf = [0u8; 128];

    // SAFETY: корректные сисколлы; buf/строки — валидные буферы программы.
    let code = unsafe {
        // 1) Стартуем в корне.
        let n = syscall3(SYS_GETCWD, buf.as_mut_ptr() as u64, buf.len() as u64, 0);
        if !cwd_eq(&buf, n, b"/") {
            sys_exit(1);
        }

        // 2) Спускаемся в подкаталог.
        if syscall3(SYS_CHDIR, b"SUB\0".as_ptr() as u64, 0, 0) < 0 {
            sys_exit(2);
        }
        let n = syscall3(SYS_GETCWD, buf.as_mut_ptr() as u64, buf.len() as u64, 0);
        if !cwd_eq(&buf, n, b"/SUB") {
            sys_exit(3);
        }

        // 3) Относительный open резолвится под /SUB.
        let fd = syscall3(SYS_OPEN, b"INSIDE.TXT\0".as_ptr() as u64, O_RDONLY, 0);
        if fd < 0 {
            sys_exit(4);
        }
        let mut fbuf = [0u8; 64];
        let r = syscall3(SYS_READ, fd as u64, fbuf.as_mut_ptr() as u64, fbuf.len() as u64);
        if r < 0 || &fbuf[..r as usize] != INSIDE {
            sys_exit(5);
        }

        // 4) `..` возвращает в корень.
        if syscall3(SYS_CHDIR, b"..\0".as_ptr() as u64, 0, 0) < 0 {
            sys_exit(6);
        }
        let n = syscall3(SYS_GETCWD, buf.as_mut_ptr() as u64, buf.len() as u64, 0);
        if !cwd_eq(&buf, n, b"/") {
            sys_exit(7);
        }
        0
    };

    // SAFETY: завершаемся итоговым кодом.
    unsafe { sys_exit(code) }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
