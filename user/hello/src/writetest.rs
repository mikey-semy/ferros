//! Пользовательская проверка записи файлов через сисколлы (M6g3): создаёт файл
//! (`open(O_CREAT|O_WRONLY|O_TRUNC)`), пишет в него строку, закрывает (сброс на диск), затем
//! открывает заново на чтение и сверяет прочитанное с записанным. Выходит с 0 при успехе или с
//! отличимым ненулевым кодом на каждом шаге — тест читает этот код.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

const SYS_READ: u64 = 0;
const SYS_WRITE: u64 = 1;
const SYS_OPEN: u64 = 2;
const SYS_CLOSE: u64 = 3;
const SYS_EXIT: u64 = 60;

const O_RDONLY: u64 = 0o0;
const O_WRONLY: u64 = 0o1;
const O_CREAT: u64 = 0o100;
const O_TRUNC: u64 = 0o1000;

/// Системный вызов с тремя аргументами; возвращает значение из `rax`.
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
/// Управление не вернётся.
unsafe fn sys_exit(code: u64) -> ! {
    asm!("syscall", in("rax") SYS_EXIT, in("rdi") code, options(noreturn));
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let path = b"WRITTEN.TXT\0";
    let msg = b"hello from the write syscall (M6g3)\n";

    // SAFETY: корректные номера/аргументы сисколлов; path/msg/buf — валидные буферы программы.
    let code = unsafe {
        // open(path, O_CREAT|O_WRONLY|O_TRUNC, 0)
        let fd = syscall3(SYS_OPEN, path.as_ptr() as u64, O_CREAT | O_WRONLY | O_TRUNC, 0);
        if fd < 0 {
            sys_exit(1);
        }
        // write(fd, msg, len)
        let w = syscall3(SYS_WRITE, fd as u64, msg.as_ptr() as u64, msg.len() as u64);
        if w != msg.len() as i64 {
            sys_exit(2);
        }
        // close(fd) — сбрасывает буфер на диск
        if syscall3(SYS_CLOSE, fd as u64, 0, 0) < 0 {
            sys_exit(3);
        }

        // Открываем заново на чтение и сверяем.
        let fd2 = syscall3(SYS_OPEN, path.as_ptr() as u64, O_RDONLY, 0);
        if fd2 < 0 {
            sys_exit(4);
        }
        let mut buf = [0u8; 64];
        let r = syscall3(SYS_READ, fd2 as u64, buf.as_mut_ptr() as u64, buf.len() as u64);
        let _ = syscall3(SYS_CLOSE, fd2 as u64, 0, 0);
        if r != msg.len() as i64 {
            sys_exit(5);
        }
        let mut ok = true;
        for i in 0..msg.len() {
            if buf[i] != msg[i] {
                ok = false;
            }
        }
        if ok {
            0
        } else {
            6
        }
    };

    // SAFETY: завершаемся с итоговым кодом.
    unsafe { sys_exit(code) }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
