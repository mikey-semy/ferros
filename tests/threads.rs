//! Интеграционный тест кооперативных потоков ядра (M4d): два потока по очереди
//! инкрементят общий счётчик через явный `yield_now`. Проходит только если
//! переключение контекста (сохранение/восстановление стека и регистров) корректно.
//!
//! Нужна инициализированная куча (потоки выделяют стеки), поэтому тест поднимает
//! память, как `heap_allocation`/`async_tasks`.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicUsize, Ordering};
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::{heap, paging};
use ferros::sched::thread;
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

/// Общий счётчик, который инкрементят оба рабочих потока.
static COUNTER: AtomicUsize = AtomicUsize::new(0);
/// Сколько раз каждый рабочий поток инкрементит счётчик.
const PER_THREAD: usize = 5;

/// Рабочий поток: инкрементит счётчик `PER_THREAD` раз, уступая после каждого шага,
/// затем уступает вечно (его просто перестанут планировать, когда `main` выйдет).
extern "C" fn worker() -> ! {
    for _ in 0..PER_THREAD {
        COUNTER.fetch_add(1, Ordering::SeqCst);
        thread::yield_now();
    }
    loop {
        thread::yield_now();
    }
}

/// Два потока чередуются через `yield_now`. Если переключение контекста работает,
/// счётчик дорастает ровно до `2 * PER_THREAD`.
#[test_case]
fn cooperative_threads_interleave() {
    thread::init();
    thread::spawn(worker);
    thread::spawn(worker);

    let target = 2 * PER_THREAD;
    let mut spins = 0usize;
    while COUNTER.load(Ordering::SeqCst) < target {
        thread::yield_now();
        spins += 1;
        assert!(spins < 100_000, "threads did not make progress");
    }
    assert_eq!(COUNTER.load(Ordering::SeqCst), target);
}
