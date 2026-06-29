//! Интеграционный тест M9d: время (`clock_gettime`/`gettimeofday`/`time`) из кольца 3.
//!
//! Спавним `timetest`: читает `CLOCK_MONOTONIC`, ждёт его возрастания, проверяет диапазоны полей
//! (нс < 1e9, мкс < 1e6) и согласованность `time()` (возврат == записанное). Выходит с 0 при
//! успехе, иначе с отличимым кодом (10..16).
//!
//! Почему это доказательство: «время идёт вперёд» означает, что таймер PIT тикает в фоне и ядро
//! правильно конвертирует тики в `struct timespec`. Преемпция ВКЛЮЧЕНА (как обычно), но даже без
//! неё аппаратный таймер тикает независимо от того, что исполняется в кольце 3.

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
use ferros::syscall::elf::TIMETEST_ELF;
use ferros::syscall::{EXIT_CALLS, LAST_EXIT_CODE};
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

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(TIMETEST_ELF, phys_mem_offset, &mut frame_allocator) };
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "timetest never exited");
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

/// `timetest` завершился с 0: время читается, идёт вперёд, поля в диапазоне, `time()` согласован.
#[test_case]
fn clock_advances_and_fields_are_valid() {
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "timetest failed (10/11 clock_gettime; 12 time did not advance; 13/14 gettimeofday; 15/16 time)"
    );
}
