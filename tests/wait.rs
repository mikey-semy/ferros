//! Интеграционный тест M6f4: `wait4` собирает завершившегося ребёнка.
//!
//! Спавним `waittest` (он будет PID 1): он форкается, ребёнок (PID 2) выходит с кодом 7, а
//! родитель через `wait4` дожидается ребёнка и проверяет, что получил его PID и код 7 — если
//! да, выходит с 0, иначе с 1. Ждём завершения обоих и проверяем ПОСЛЕДНИЙ код выхода.
//!
//! Почему это доказательство `wait`: родитель завершается ПОСЛЕ ребёнка (он его ждёт), поэтому
//! последний код выхода — родительский. 0 означает, что `wait4` вернул PID ребёнка и собрал его
//! статус (код 7) — то есть родитель реально заблокировался, был разбужен завершением ребёнка и
//! получил верные данные. Это же прогоняет блокирующий syscall на собственном ядровом стеке
//! процесса (M6f4): без него кадр заблокированного `wait` затёрся бы чужим вызовом.

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
use ferros::syscall::elf::WAITTEST_ELF;
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
    unsafe { spawn_user(WAITTEST_ELF, phys_mem_offset, &mut frame_allocator) };

    // fork (внутри waittest) строит адресное пространство ребёнка → нужен глобальный аллокатор.
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    // Ждём завершения ОБОИХ процессов (ребёнок + родитель).
    while EXIT_CALLS.load(Ordering::SeqCst) < 2 {
        spins += 1;
        assert!(spins < 5_000_000_000, "parent + child never both exited");
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

/// Родитель (выходит последним — он ждёт ребёнка) завершился с 0: `wait4` вернул PID ребёнка и
/// собрал его код выхода 7.
#[test_case]
fn wait_collects_child_pid_and_status() {
    assert_eq!(
        EXIT_CALLS.load(Ordering::SeqCst),
        2,
        "expected exactly two processes to exit (parent + child)"
    );
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "parent's wait4 did not return the child's pid + status 7"
    );
}
