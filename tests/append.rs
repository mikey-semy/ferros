//! Интеграционный тест M7g1: дозапись `>>` (`O_APPEND`) поверх durable-вывода.
//!
//! «Набираем» две команды: `echo a > /SUB/M7GAPP.TXT` (создаёт файл со строкой `a`), затем
//! `echo b >> /SUB/M7GAPP.TXT` (дописывает `b`). Вторая команда видит результат первой ТОЛЬКО потому,
//! что вывод первой сброшен на диск синхронно при её `exit` (M7g1) — `echo b` открывает уже
//! durable файл с `O_APPEND` (позиция в конце) и дописывает. Итог — `/SUB/M7GAPP.TXT` == `"a\nb\n"`.
//!
//! Это покрывает: `>>`/`O_APPEND`, durability вывода между последовательными командами и сам
//! редирект. `EXIT_CALLS == 3` — оба `echo` и shell завершились.

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

    for c in "echo a > /SUB/M7GAPP.TXT\necho b >> /SUB/M7GAPP.TXT\nexit 0\n".chars() {
        console::feed_char(c);
    }

    // echo a + echo b + shell = 3 завершения.
    while EXIT_CALLS.load(Ordering::SeqCst) < 3 {
        spins += 1;
        assert!(spins < 5_000_000_000, "both echos + shell never all exited");
        core::hint::spin_loop();
    }
    thread::stop_preemption();

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// `echo a > f; echo b >> f` дало `"a\nb\n"`: append дописал поверх durable-вывода первой команды.
#[test_case]
fn append_extends_a_durable_file() {
    let got = ferros::fs::open("/SUB/M7GAPP.TXT").expect("/SUB/M7GAPP.TXT was not created");
    assert_eq!(
        got, b"a\nb\n",
        "append (>>) did not extend the file: redirect durability or O_APPEND broken"
    );
}
