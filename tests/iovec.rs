//! Интеграционный тест M9f: `writev`/`readv`/`fcntl` из кольца 3.
//!
//! Спавним `ioveccheck`: создаёт канал, `writev` нескольких буферов в конец записи и читает обратно
//! из конца чтения, `readv` в несколько буферов, проверяет `fcntl(F_GETFL)` и `fcntl(F_DUPFD)`
//! (продублированный конец чтения реально работает). Выходит с 0 при успехе, иначе с кодом 10..17.
//!
//! Самодостаточно через канал — диск не нужен.

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
use ferros::syscall::elf::IOVECCHECK_ELF;
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
    unsafe { spawn_user(IOVECCHECK_ELF, phys_mem_offset, &mut frame_allocator) };
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "ioveccheck never exited");
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

/// `ioveccheck` завершился с 0: `writev`/`readv`/`fcntl` отработали через канал.
#[test_case]
fn writev_readv_and_fcntl() {
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "ioveccheck failed (10 pipe; 11/12 writev+read; 13/14 readv; 15 F_GETFL; 16/17 F_DUPFD)"
    );
}
