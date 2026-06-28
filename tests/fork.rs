//! Интеграционный тест M6f3: `fork` создаёт ребёнка — копию процесса.
//!
//! Спавним `forktest` (он будет PID 1). Он форкается: ребёнок (`fork` вернул 0) выходит с
//! кодом 7, родитель (`fork` вернул PID ребёнка = 2) выходит с этим PID. Ждём, пока завершатся
//! ОБА процесса, и проверяем сумму кодов = 7 + 2 = 9.
//!
//! Почему это доказательство `fork`: завершились ДВА процесса из одной запущенной программы
//! (значит, появился ребёнок и он реально исполнялся), и сумма кодов 9 = 7 (ребёнок получил из
//! `fork` ноль → ветка `exit(7)`) + 2 (родитель получил PID ребёнка → `exit(2)`). Порядок
//! завершения недетерминирован, поэтому сверяем сумму, а не последний код.

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
use ferros::syscall::elf::FORKTEST_ELF;
use ferros::syscall::{EXIT_CALLS, EXIT_CODE_SUM};
use x86_64::VirtAddr;

entry_point!(main);

/// Код выхода ребёнка (см. `user/hello/src/forktest.rs`) — держать синхронно.
const CHILD_EXIT: i64 = 7;
/// PID ребёнка: `forktest` стартует PID 1 (первый пользовательский процесс), его ребёнок — 2.
const EXPECTED_CHILD_PID: i64 = 2;

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
    unsafe { spawn_user(FORKTEST_ELF, phys_mem_offset, &mut frame_allocator) };

    // fork строит адресное пространство ребёнка → нужен глобальный аллокатор.
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    // Ждём завершения ОБОИХ процессов (родитель + ребёнок).
    while EXIT_CALLS.load(Ordering::SeqCst) < 2 {
        spins += 1;
        assert!(spins < 5_000_000_000, "both processes never exited");
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

/// `fork` разделил исполнение: ровно два процесса завершились, сумма их кодов = 7 + PID ребёнка.
#[test_case]
fn fork_splits_into_parent_and_child() {
    assert_eq!(
        EXIT_CALLS.load(Ordering::SeqCst),
        2,
        "expected exactly two processes to exit (parent + child)"
    );
    assert_eq!(
        EXIT_CODE_SUM.load(Ordering::SeqCst),
        CHILD_EXIT + EXPECTED_CHILD_PID,
        "exit codes != child's 7 (fork→0 branch) + parent's child-pid (fork→pid branch)"
    );
}
