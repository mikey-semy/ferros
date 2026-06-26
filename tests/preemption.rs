//! Интеграционный тест **вытесняющей** многозадачности (M4e): два фоновых потока
//! крутят бесконечный цикл и НИ РАЗУ не уступают добровольно. Если вытеснение по
//! таймеру работает, таймер всё равно переключает между ними (и обратно в тест), и оба
//! счётчика растут. Если бы вытеснения не было, первый же занятый поток захватил бы CPU
//! навсегда — второй счётчик остался бы нулём и тест провалился бы по таймауту-страховке.
//!
//! Нужна инициализированная куча (потоки выделяют стеки), поэтому тест поднимает
//! память, как `threads`/`heap_allocation`.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU64, Ordering};
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

/// Счётчики «полезной работы» каждого занятого потока.
static A: AtomicU64 = AtomicU64::new(0);
static B: AtomicU64 = AtomicU64::new(0);

/// Занятый поток A: бесконечно инкрементит счётчик и НИКОГДА не зовёт `yield_now`.
/// Уступить его может только таймер.
extern "C" fn busy_a() -> ! {
    loop {
        A.fetch_add(1, Ordering::Relaxed);
        core::hint::spin_loop();
    }
}

/// Занятый поток B: то же, что A, со своим счётчиком.
extern "C" fn busy_b() -> ! {
    loop {
        B.fetch_add(1, Ordering::Relaxed);
        core::hint::spin_loop();
    }
}

/// Вытеснение по таймеру переключает два потока, которые сами не уступают. Тест-поток
/// (тоже не уступающий) ждёт, пока оба счётчика не сдвинутся с нуля — это невозможно
/// без вытеснения: оба занятых потока крутят `loop {}`, а сам тест должен быть вытеснен
/// в их пользу и обратно.
#[test_case]
fn timer_preempts_busy_threads() {
    thread::init();
    thread::spawn(busy_a);
    thread::spawn(busy_b);
    thread::start_preemption();

    // Страховка от зависания: если вытеснения нет, оба счётчика останутся нулём, и мы
    // выйдем по лимиту с понятной диагностикой вместо вечного цикла.
    let mut spins = 0u64;
    loop {
        let a = A.load(Ordering::Relaxed);
        let b = B.load(Ordering::Relaxed);
        if a > 0 && b > 0 {
            break;
        }
        spins += 1;
        assert!(
            spins < 5_000_000_000,
            "no timer preemption: a={a} b={b} (busy threads never got scheduled)"
        );
        core::hint::spin_loop();
    }

    // Дальше потоки не нужны — выключаем вытеснение, чтобы спокойно завершить QEMU из
    // тест-раннера, не перескакивая больше в занятые потоки.
    thread::stop_preemption();

    assert!(A.load(Ordering::Relaxed) > 0);
    assert!(B.load(Ordering::Relaxed) > 0);
}
