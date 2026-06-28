//! Интеграционный тест M7f: `mkdir` из `/bin` создаёт каталог через сисколл `mkdir(2)`.
//!
//! «Набираем» shell'у `mkdir /M7FBIN`: он находит `/bin/mkdir`, запускает (`fork`+`execve`), а тот
//! зовёт `mkdir("/M7FBIN")` — новый сисколл идёт в FAT-слой и пишет каталог на диск. Затем `exit 0`.
//!
//! Проверяем результат по СУЩЕСТВОВАНИЮ каталога (`fs::lookup`), а не по коду выхода `mkdir`: образ
//! диска переживает прогоны (запись virtio-blk идёт в файл образа), поэтому на повторном прогоне
//! `mkdir` вернул бы `-EEXIST`, но каталог всё равно на месте. `EXIT_CALLS == 2` подтверждает, что
//! и `mkdir`, и shell завершились нормально (через `exit`, а не сбоем).

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
use ferros::fs::fat::Node;
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

    for c in "mkdir /M7FBIN\nexit 0\n".chars() {
        console::feed_char(c);
    }

    while EXIT_CALLS.load(Ordering::SeqCst) < 2 {
        spins += 1;
        assert!(spins < 5_000_000_000, "mkdir + shell never both exited");
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

/// `/bin/mkdir` создал `/M7FBIN`: каталог существует на диске, оба процесса вышли нормально.
#[test_case]
fn mkdir_utility_creates_a_directory() {
    assert_eq!(
        EXIT_CALLS.load(Ordering::SeqCst),
        2,
        "expected mkdir + shell to each exit"
    );
    assert!(
        matches!(ferros::fs::lookup("/M7FBIN"), Ok(Node::Dir(_))),
        "/M7FBIN was not created on disk by the mkdir utility"
    );
}
