//! Интеграционный тест M6f5: `kill` завершает процесс, а `wait4` сообщает гибель по сигналу.
//!
//! Спавним `killtest` (PID 1): он форкается, ребёнок (PID 2) крутится вечно, родитель посылает
//! ему `SIGTERM`, дожидается через `wait4` и проверяет, что ребёнок завершён ИМЕННО сигналом
//! `SIGTERM` (WIFSIGNALED, WTERMSIG == 15). Родитель выходит с 0 при успехе, иначе с 1.
//!
//! Почему это доказательство: убитый ребёнок НЕ зовёт `exit`, поэтому единственный `exit` —
//! родительский, и его код 0 означает, что `kill` действительно завершил ребёнка, а `wait4`
//! вернул статус «убит SIGTERM». Так проверяются и сам `kill`, и кодировка WIFSIGNALED.

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
use ferros::syscall::elf::KILLTEST_ELF;
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
    unsafe { spawn_user(KILLTEST_ELF, phys_mem_offset, &mut frame_allocator) };

    // fork (внутри killtest) строит адресное пространство ребёнка → нужен глобальный аллокатор.
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    // Убитый ребёнок не зовёт exit; единственный exit — родительский (после wait4).
    while EXIT_CALLS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "killtest parent never exited");
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

/// Родитель завершился с 0: `kill(child, SIGTERM)` сработал, и `wait4` сообщил гибель ребёнка по
/// сигналу `SIGTERM`.
#[test_case]
fn kill_terminates_child_and_wait_reports_signal() {
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "parent: kill(SIGTERM) + wait4 did not report the child as killed by SIGTERM"
    );
}
