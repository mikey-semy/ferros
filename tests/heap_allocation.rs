//! Интеграционный тест кучи (M3c): проверяем, что динамическая память реально
//! работает — `Box`, большой `Vec`, и переиспользование освобождённой памяти.
//!
//! Тест поднимает ядро как настоящий бинарь (`entry_point!` → `BootInfo`),
//! инициализирует пейджинг, аллокатор фреймов и кучу, затем запускает `#[test_case]`
//! через тот же тест-раннер ([`ferros::test_runner`]). Без инициализированной кучи
//! эти аллокации паниковали бы — поэтому heap-тесты только здесь, а не в lib-тестах.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::{heap, paging};
use x86_64::VirtAddr;

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init вызывается один раз.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// Простейшие аллокации: значения в `Box` читаются обратно корректно.
#[test_case]
fn simple_allocation() {
    let a = Box::new(41);
    let b = Box::new(13);
    assert_eq!(*a, 41);
    assert_eq!(*b, 13);
}

/// Большой `Vec`: тысяча элементов кладётся и суммируется (проверка роста буфера).
#[test_case]
fn large_vec() {
    let n = 1000;
    let mut vec = Vec::new();
    for i in 0..n {
        vec.push(i);
    }
    assert_eq!(vec.iter().sum::<u64>(), (n - 1) * n / 2);
}

/// Переиспользование памяти: создаём и роняем больше боксов, чем влезает в кучу
/// одновременно. Проходит только если освобождённая память возвращается в оборот.
#[test_case]
fn many_boxes() {
    for i in 0..heap::HEAP_SIZE {
        let x = Box::new(i);
        assert_eq!(*x, i);
    }
}
