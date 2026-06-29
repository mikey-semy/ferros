//! Интеграционный тест M9g: первая программа на **C** работает на ferros.
//!
//! Спавним `HELLO_C_ELF` — собранный clang'ом свободностоящий C-бинарь, который пишет
//! «Hello from C on ferros!\n» через `write(1)` и выходит с 0. Проверяем по наблюдаемости записи
//! (`LAST_WRITE_*`), что запись пришла на fd 1 нужной длины и с нужной суммой байт, и что процесс
//! вышел с 0.
//!
//! Почему это важно: доказывает, что обычный C-ABI ELF (не наши ручные Rust-asm программы) грузится
//! и исполняется в кольце 3 и корректно делает Linux-сисколлы — фундамент под порт libc.

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
use ferros::drivers::console;
use ferros::mm::frame::{self, BootInfoFrameAllocator};
use ferros::mm::{heap, paging};
use ferros::sched::thread;
use ferros::syscall::elf::HELLO_C_ELF;
use ferros::syscall::{EXIT_CALLS, LAST_EXIT_CODE, LAST_WRITE_FD, LAST_WRITE_LEN, LAST_WRITE_SUM};
use x86_64::VirtAddr;

/// Та же строка, что печатает `user/c/hello.c` (разные крейты — общую константу не пошарить).
const EXPECTED: &[u8] = b"Hello from C on ferros!\n";

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init один раз.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");
    console::init(); // C-программа пишет на fd 1 (VGA)

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(HELLO_C_ELF, phys_mem_offset, &mut frame_allocator) };
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "C program never exited");
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

/// C-программа вышла с 0 и записала ожидаемую строку на fd 1.
#[test_case]
fn c_program_runs_and_writes() {
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "C program exited non-zero"
    );
    assert_eq!(
        LAST_WRITE_FD.load(Ordering::SeqCst),
        1,
        "write was not to fd 1"
    );
    assert_eq!(
        LAST_WRITE_LEN.load(Ordering::SeqCst),
        EXPECTED.len() as u64,
        "C program wrote the wrong number of bytes"
    );
    let expected_sum: u64 = EXPECTED.iter().map(|&b| b as u64).sum();
    assert_eq!(
        LAST_WRITE_SUM.load(Ordering::SeqCst),
        expected_sum,
        "C program wrote the wrong bytes"
    );
}
