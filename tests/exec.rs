//! Интеграционный тест M6f2: `execve` заменяет образ процесса программой с диска.
//!
//! Спавним `exectest`: он `execve("HELLO")` — это должно заменить его программой `hello`,
//! загруженной С ДИСКА (FAT). Тогда `hello` печатает свою строку и завершается с кодом 0.
//! Если бы `execve` не сработал, `exectest` вышел бы с кодом 42.
//!
//! Почему это доказательство: код выхода 0 (а не 42) и последний `write` = строка `hello`
//! означают, что процесс действительно стал `hello` — то есть exec прочитал ELF с диска,
//! построил новое адресное пространство и ушёл в его точку входа.

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
use ferros::mm::frame::{self, BootInfoFrameAllocator};
use ferros::mm::{heap, paging};
use ferros::sched::thread;
use ferros::syscall::elf::EXECTEST_ELF;
use ferros::syscall::{EXIT_CALLS, LAST_EXIT_CODE, LAST_WRITE_LEN, LAST_WRITE_SUM};
use x86_64::VirtAddr;

entry_point!(main);

/// Строка, которую печатает `hello` (см. `user/hello/src/main.rs`) — держать синхронно.
const HELLO_MSG: &[u8] = b"hello from a real ELF program in ring 3\n";

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init один раз.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");

    // execve читает программу с диска → нужен virtio-blk.
    assert!(
        ferros::drivers::virtio_blk::init(phys_mem_offset, &mut frame_allocator),
        "virtio-blk not initialized"
    );

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(EXECTEST_ELF, phys_mem_offset, &mut frame_allocator) };

    // execve строит новое адресное пространство и освобождает старое → нужен глобальный
    // аллокатор.
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) == 0 {
        spins += 1;
        assert!(spins < 5_000_000_000, "exectest never exited");
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

/// После `execve` процесс стал `hello`: вышел с кодом 0 (не 42) и напечатал строку `hello`.
#[test_case]
fn execve_replaces_image_with_program_from_disk() {
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "process did not become hello (exit code != 0 means execve failed)"
    );
    assert_eq!(
        LAST_WRITE_LEN.load(Ordering::SeqCst) as usize,
        HELLO_MSG.len(),
        "last write length != hello's message"
    );
    let expected: u64 = HELLO_MSG.iter().map(|&b| b as u64).sum();
    assert_eq!(
        LAST_WRITE_SUM.load(Ordering::SeqCst),
        expected,
        "last write content != hello's message"
    );
}
