//! Интеграционный тест M9k: строковые/мемори/конвертирующие функции минимальной libc.
//!
//! Спавним `LIBCHECK_ELF` — C-программу, которая прогоняет `memmove`/`memcmp`/`strncmp`/`strcpy`/
//! `strncpy`/`strchr`/`atoi` на known-входах и сама их сверяет, возвращая 0 только если ВСЕ группы
//! сошлись (иначе свой код 10..17). Проверяем код выхода 0.

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
use ferros::sched::thread;
use ferros::syscall::elf::LIBCHECK_ELF;
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
    console::init(); // libcheck печатает строку через puts

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(LIBCHECK_ELF, phys_mem_offset, &mut frame_allocator) };
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "libcheck never exited");
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

/// `libcheck` вышла с 0: все строковые/мемори/atoi функции дали ожидаемые результаты.
#[test_case]
fn libc_string_mem_conv_functions() {
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "a libc function failed (10/11 memmove; 12 memcmp; 13 strncmp; 14 strcpy; 15 strncpy; 16 strchr; 17 atoi)"
    );
}
