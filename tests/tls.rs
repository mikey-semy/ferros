//! Интеграционный тест M9b: TLS через `arch_prctl(ARCH_SET_FS)` из кольца 3.
//!
//! Спавним `tlstest`: он ставит базу сегмента FS на свой блок TLS, читает его через `fs:[0]`,
//! сверяет `ARCH_GET_FS`, затем в цикле много раз читает `fs:[0]`. Выходит с 0 при успехе, иначе
//! с отличимым кодом (10..14).
//!
//! Почему это доказательство: пока `tlstest` крутит цикл чтений, **вытеснение по таймеру** уводит
//! CPU на «нулевой» поток ядра (его FS-база 0) и возвращает обратно. Если бы планировщик не
//! восстанавливал FS-базу процесса при переключении на него, `fs:[0]` после такого «круга» читал
//! бы не из блока TLS — и тест упал бы кодом 14. Поэтому преемпция здесь ВКЛЮЧЕНА и цикл длинный.

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
use ferros::syscall::elf::TLSTEST_ELF;
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
    unsafe { spawn_user(TLSTEST_ELF, phys_mem_offset, &mut frame_allocator) };
    frame::install(frame_allocator);

    // Преемпция ВКЛЮЧЕНА намеренно: цикл чтений в tlstest должен пережить переключения
    // на поток ядра и обратно (проверка восстановления FS-базы).
    thread::start_preemption();
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "tlstest never exited");
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

/// `tlstest` завершился с 0: `ARCH_SET_FS`/`ARCH_GET_FS` работают и FS-база переживает переключения.
#[test_case]
fn arch_prctl_sets_fs_base_and_survives_context_switches() {
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "tlstest failed (10 SET_FS; 11 fs:[0] read; 12 GET_FS; 13 GET_FS value; 14 base lost across a switch)"
    );
}
