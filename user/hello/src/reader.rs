//! Пользовательская программа-«читатель» (M6d2): открывает файл на диске, читает его и
//! печатает в stdout — демонстрация файловых системных вызовов из кольца 3.
//!
//! `open("HELLO.TXT")` → `read(fd, …)` → `write(1, …)` → `close(fd)` → `exit(0)`. Файл лежит
//! на диске (FAT32); ядро по `open` читает его с virtio-blk, по `read` отдаёт байты в наш
//! буфер. Никакого рантайма: `#![no_std]` + своя точка входа.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

const SYS_READ: u64 = 0;
const SYS_WRITE: u64 = 1;
const SYS_OPEN: u64 = 2;
const SYS_CLOSE: u64 = 3;
const SYS_EXIT: u64 = 60;

/// Системный вызов с тремя аргументами (Linux x86-64 ABI). Возвращает `rax` как `i64`
/// (≥0 — результат, отрицательное — `-errno`).
///
/// # Safety
/// Аргументы должны соответствовать вызываемому номеру; ядро может читать/писать переданные
/// буферы.
unsafe fn syscall3(nr: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let ret: i64;
    asm!(
        "syscall",
        in("rax") nr,
        in("rdi") a1,
        in("rsi") a2,
        in("rdx") a3,
        lateout("rax") ret,
        lateout("rcx") _,
        lateout("r11") _,
    );
    ret
}

/// `exit(code)` — не возвращается.
fn exit(code: u64) -> ! {
    // SAFETY: номер exit; управление в программу уже не вернётся.
    unsafe {
        asm!("syscall", in("rax") SYS_EXIT, in("rdi") code, options(noreturn));
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let path = b"HELLO.TXT\0";
    let mut buf = [0u8; 64];

    // open(path, flags=0 (O_RDONLY), mode=0)
    // SAFETY: путь — корректная нуль-терминированная строка в нашей памяти.
    let fd = unsafe { syscall3(SYS_OPEN, path.as_ptr() as u64, 0, 0) };
    if fd < 0 {
        exit(1);
    }

    // read(fd, buf, buf.len())
    // SAFETY: buf — наш буфер на стеке, ядро запишет в него прочитанное.
    let n = unsafe { syscall3(SYS_READ, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64) };
    if n < 0 {
        exit(2);
    }

    // write(1, buf, n) — печатаем прочитанное в stdout.
    // SAFETY: читаем первые n байт нашего буфера.
    unsafe {
        syscall3(SYS_WRITE, 1, buf.as_ptr() as u64, n as u64);
    }

    // close(fd)
    // SAFETY: закрываем валидный дескриптор.
    unsafe {
        syscall3(SYS_CLOSE, fd as u64, 0, 0);
    }

    exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
