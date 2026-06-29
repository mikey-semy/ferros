//! Интеграционный тест M9j: форматированный вывод минимальной libc (`snprintf`/`printf`).
//!
//! Спавним `PRINTFTEST_ELF` — C-программу, которая форматирует строку со всеми поддержанными
//! спецификаторами (`%d %u %x %X %s %c %ld %%`), пишет её на fd 1 и сама сверяет результат с
//! эталоном через `strcmp`, возвращая 0 при совпадении. Проверяем код выхода 0 (формат верный) и
//! записанную строку (длина + сумма байт).

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
use ferros::syscall::elf::PRINTFTEST_ELF;
use ferros::syscall::{EXIT_CALLS, LAST_EXIT_CODE, LAST_WRITE_FD, LAST_WRITE_LEN, LAST_WRITE_SUM};
use x86_64::VirtAddr;

/// Та же строка, что форматирует `user/c/printftest.c` (разные крейты — общую константу не пошарить).
const EXPECTED: &[u8] = b"d=-42 u=42 x=dead X=BEEF s=ok c=Q ld=-9000000000 %\n";

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init один раз.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");
    console::init(); // printftest пишет на fd 1 (VGA)

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(PRINTFTEST_ELF, phys_mem_offset, &mut frame_allocator) };
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "printftest never exited");
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

/// `printftest` вышла с 0 (формат совпал с эталоном) и записала ожидаемую строку на fd 1.
#[test_case]
fn printf_formats_all_specifiers() {
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "printftest's snprintf output did not match the expected string"
    );
    assert_eq!(
        LAST_WRITE_FD.load(Ordering::SeqCst),
        1,
        "write was not to fd 1"
    );
    assert_eq!(
        LAST_WRITE_LEN.load(Ordering::SeqCst),
        EXPECTED.len() as u64,
        "printf wrote the wrong number of bytes"
    );
    let expected_sum: u64 = EXPECTED.iter().map(|&b| b as u64).sum();
    assert_eq!(
        LAST_WRITE_SUM.load(Ordering::SeqCst),
        expected_sum,
        "printf wrote the wrong bytes"
    );
}
