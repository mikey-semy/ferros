//! Интеграционный тест M5c3b: сбой пользователя завершает **процесс, а не ядро**.
//!
//! `main` заводит планировщик и спавнит программу-«фолтер» (`user/hello` бинарь `faulter`),
//! которая в кольце 3 читает неотображённый адрес → page fault. Ядро должно убить этот
//! процесс и продолжить работать. Тест крутится на «нулевом» потоке, пока ядро не отметит
//! завершение по сбою, и проверяет, что мы живы и вернулись в ядро.
//!
//! Почему это доказательство: если бы сбой кольца 3 ронял ядро (паника/тройной сброс), мы
//! бы сюда не вернулись (таймаут). А ненулевой `USER_FAULT_KILLS` + продолжение «нулевого»
//! потока показывают: процесс убит, ядро живо — изоляция сбоев работает.

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
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::{heap, paging};
use ferros::sched::thread;
use ferros::syscall::elf::FAULTER_ELF;
use ferros::syscall::USER_FAULT_KILLS;
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

    thread::init();
    // SAFETY: phys_mem_offset корректен, куча поднята; адреса процесса свободны у ядра.
    unsafe { spawn_user(FAULTER_ELF, phys_mem_offset, &mut frame_allocator) };
    thread::start_preemption();

    // Крутимся, пока ядро не убьёт процесс из-за сбоя. Если бы сбой ронял ядро, сюда бы не
    // вернулись (таймаут); страховка-лимит ловит зависание.
    let mut spins = 0u64;
    while USER_FAULT_KILLS.load(Ordering::SeqCst) == 0 {
        spins += 1;
        assert!(spins < 5_000_000_000, "faulting process was never killed");
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

/// Сбой в кольце 3 завершил процесс, а ядро продолжило работу.
#[test_case]
fn ring3_fault_kills_process_not_kernel() {
    // Раз мы дошли до проверки (на «нулевом» потоке) — ядро пережило сбой пользователя.
    assert!(
        USER_FAULT_KILLS.load(Ordering::SeqCst) >= 1,
        "no user process was killed by a ring-3 fault"
    );
}
