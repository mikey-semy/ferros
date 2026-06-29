//! Интеграционный тест M7g1: редиректы shell (`<`/`>`) через `dup2` + сброс файла на `exit`.
//!
//! «Набираем» shell'у `cat < /HELLO.TXT > /SUB/M7GOUT.TXT`: shell вынимает редиректы, в дочернем
//! процессе открывает `/HELLO.TXT` на чтение и `dup2`'ит на fd 0, открывает `/SUB/M7GOUT.TXT` на
//! запись и `dup2`'ит на fd 1, затем `execve("/bin/cat")`. `cat` без аргументов перекачивает
//! stdin → stdout, то есть копирует `/HELLO.TXT` в `/SUB/M7GOUT.TXT`. По `exit 0` shell завершается.
//!
//! `cat` НЕ закрывает stdout, поэтому перенаправленный файл сбрасывается на диск при завершении
//! процесса — СИНХРОННО в обработчике `exit` (M7g1), так что он durable к моменту, когда мы видим
//! завершение. Проверяем, что `/SUB/M7GOUT.TXT` побайтово совпал с `/HELLO.TXT`. Это доказывает: оба
//! редиректа, `dup2`, модель подложек fd (cat читает fd0=файл, пишет fd1=файл) и сброс на `exit`.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::Ordering;
use ferros::arch::x86_64::syscall::spawn_user;
use ferros::drivers::console;
use ferros::mm::frame::{self, BootInfoFrameAllocator};
use ferros::mm::{heap, paging};
use ferros::sched::thread::{self, STDIN_BLOCKS};
use ferros::syscall::elf::SHELL_ELF;
use ferros::syscall::EXIT_CALLS;
use x86_64::VirtAddr;

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init один раз.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");

    console::init();
    assert!(
        ferros::drivers::virtio_blk::init(phys_mem_offset, &mut frame_allocator),
        "virtio-blk not initialized"
    );

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(SHELL_ELF, phys_mem_offset, &mut frame_allocator) };
    frame::install(frame_allocator);

    thread::start_preemption();

    let mut spins = 0u64;
    while STDIN_BLOCKS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "shell never blocked on stdin");
        core::hint::spin_loop();
    }

    for c in "cat < /HELLO.TXT > /SUB/M7GOUT.TXT\nexit 0\n".chars() {
        console::feed_char(c);
    }

    // cat (ребёнок) + shell = 2 завершения.
    while EXIT_CALLS.load(Ordering::SeqCst) < 2 {
        spins += 1;
        assert!(spins < 5_000_000_000, "cat + shell never both exited");
        core::hint::spin_loop();
    }
    thread::stop_preemption();

    // Грязный перенаправленный файл уже сброшен на диск синхронно в `exit` (M7g1) — отдельный
    // reaper не нужен.
    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// `cat < /HELLO.TXT > /SUB/M7GOUT.TXT` скопировал файл: вывод побайтово равен входу.
#[test_case]
fn redirects_copy_a_file_through_cat() {
    let expected = ferros::fs::open("/HELLO.TXT").expect("HELLO.TXT missing on disk");
    let got = ferros::fs::open("/SUB/M7GOUT.TXT").expect("redirected output file was not created");
    assert_eq!(
        got, expected,
        "cat < HELLO.TXT > OUT did not copy the file (redirect/dup2/flush broken)"
    );
}
