//! Пользовательская проверка листинга каталога (M6g5): открывает корневой каталог, читает его
//! записи через `getdents64` и проверяет, что среди них есть `HELLO.TXT` (его кладёт build.rs).
//! Выходит с 0 при успехе или отличимым ненулевым кодом на каждом шаге.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

const SYS_CLOSE: u64 = 3;
const SYS_OPEN: u64 = 2;
const SYS_EXIT: u64 = 60;
const SYS_GETDENTS64: u64 = 217;
const O_RDONLY: u64 = 0;

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
    let root = b"/\0";
    let target = b"HELLO.TXT";
    let mut buf = [0u8; 1024];

    // SAFETY: корректные сисколлы; root/buf — валидные буферы программы.
    let code = unsafe {
        let fd = syscall3(SYS_OPEN, root.as_ptr() as u64, O_RDONLY, 0);
        if fd < 0 {
            sys_exit(1);
        }

        let mut found = false;
        loop {
            let n = syscall3(
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
            // Разбираем записи linux_dirent64 в buf[..n]: d_reclen (u16 по +16), имя (с +19, до нуля).
            let n = n as usize;
            let mut pos = 0usize;
            while pos + 18 < n {
                let reclen = (buf[pos + 16] as usize) | ((buf[pos + 17] as usize) << 8);
                if reclen == 0 {
                    break; // защита от зацикливания на битой записи
                }
                let name_start = pos + 19;
                let mut name_len = 0usize;
                while name_start + name_len < n && buf[name_start + name_len] != 0 {
                    name_len += 1;
                }
                if name_len == target.len() {
                    let mut eq = true;
                    for i in 0..name_len {
                        if buf[name_start + i] != target[i] {
                            eq = false;
                        }
                    }
                    if eq {
                        found = true;
                    }
                }
                pos += reclen;
            }
        }

        let _ = syscall3(SYS_CLOSE, fd as u64, 0, 0);
        if found {
            0
        } else {
            3
        }
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
