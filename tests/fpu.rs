//! Интеграционный тест M9i: состояние FPU/SSE сохраняется при переключении контекста.
//!
//! Спавним `FPUTEST_ELF` — C-программу, которая `fork`'ается; родитель и ребёнок кладут в `xmm0`
//! РАЗНЫЕ значения и в длинном цикле проверяют, что их `xmm0` не изменился. Под вытеснением
//! планировщик постоянно переключает их, и каждый при исполнении ставит `xmm0` = своё. Если бы
//! `switch_task` НЕ делал `fxsave`/`fxrstor`, значение протекло бы между процессами и проверка
//! упала бы. Оба процесса выходят с 0 только если каждый видел ровно своё — проверяем
//! `EXIT_CODE_SUM == 0` после двух завершений.
//!
//! Преемпция включена намеренно (нужны переключения); 5e6-цикл в программе охватывает много тиков.

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
use ferros::syscall::elf::FPUTEST_ELF;
use ferros::syscall::{EXIT_CALLS, EXIT_CODE_SUM};
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
    unsafe { spawn_user(FPUTEST_ELF, phys_mem_offset, &mut frame_allocator) };
    // Глобальный аллокатор — ДО старта: `fork` копирует адресное пространство через него.
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    // Родитель + ребёнок = 2 завершения.
    while EXIT_CALLS.load(Ordering::SeqCst) < 2 {
        spins += 1;
        assert!(
            spins < 5_000_000_000,
            "fputest parent+child never both exited"
        );
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

/// Оба процесса вышли с 0: каждый видел только своё значение `xmm0` — FPU не протёк при переключении.
#[test_case]
fn fpu_state_survives_context_switch() {
    assert_eq!(
        EXIT_CODE_SUM.load(Ordering::SeqCst),
        0,
        "an fputest process saw a foreign xmm0 (FPU/SSE state leaked across a context switch)"
    );
}
