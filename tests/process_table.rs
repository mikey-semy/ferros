//! Интеграционный тест M6f1: у процессов есть идентификаторы (PID).
//!
//! Проверяем, что (1) «нулевой» поток ядра — PID 0, и (2) первый пользовательский процесс
//! получает PID 1, который ему отдаёт `getpid`. Программа `getpidtest` спрашивает свой PID и
//! завершается с ним как кодом возврата — ядро сверяет код выхода.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use ferros::arch::x86_64::syscall::spawn_user;
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::{heap, paging};
use ferros::sched::thread;
use ferros::syscall::elf::GETPIDTEST_ELF;
use ferros::syscall::{EXIT_CALLS, LAST_EXIT_CODE};
use x86_64::VirtAddr;

entry_point!(main);

/// PID контекста, в котором стартует тест (должен быть «нулевой» поток ядра — 0).
static ZERO_PID: AtomicU32 = AtomicU32::new(u32::MAX);
/// Код выхода getpidtest (= его PID).
static REPORTED_PID: AtomicI64 = AtomicI64::new(-1);

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init один раз.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");

    thread::init();
    // Тест исполняется на «нулевом» потоке — его PID должен быть 0.
    ZERO_PID.store(thread::current_pid(), Ordering::SeqCst);

    // Первый пользовательский процесс → PID 1; он выйдет с этим значением.
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(GETPIDTEST_ELF, phys_mem_offset, &mut frame_allocator) };
    thread::start_preemption();

    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) == 0 {
        spins += 1;
        assert!(spins < 5_000_000_000, "getpidtest never exited");
        core::hint::spin_loop();
    }
    thread::stop_preemption();
    REPORTED_PID.store(LAST_EXIT_CODE.load(Ordering::SeqCst), Ordering::SeqCst);

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// «Нулевой» поток ядра имеет PID 0.
#[test_case]
fn zero_thread_is_pid_zero() {
    assert_eq!(
        ZERO_PID.load(Ordering::SeqCst),
        0,
        "the kernel zero thread should be PID 0"
    );
}

/// Первый пользовательский процесс получил PID 1, и `getpid` его вернул (код выхода = PID).
#[test_case]
fn first_user_process_is_pid_one() {
    assert_eq!(
        REPORTED_PID.load(Ordering::SeqCst),
        1,
        "first user process should see getpid()==1"
    );
}
