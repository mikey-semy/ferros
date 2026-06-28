//! Интеграционный тест M7a: `read(0)` доставляет ввод с консоли в кольцо 3.
//!
//! Спавним `stdintest` (PID 1): он делает `read(0, buf, …)` и блокируется (ввода ещё нет).
//! Тест ДОЖИДАЕТСЯ блокировки (через счётчик [`STDIN_BLOCKS`]) — так детерминированно проверяется
//! весь путь «процесс заснул на пустом stdin → побудка», а не только буфер, — затем подаёт строку
//! `"hi\n"` в линейную дисциплину ([`console::feed_char`], как делала бы клавиатура). Завершение
//! строки будит читателя; он копирует её себе, печатает обратно через `write(1, …)` и выходит с 0.
//!
//! Почему это доказательство stdin: `read(0)` вернул ровно те байты, что «набраны» (их фиксируют
//! `LAST_WRITE_LEN`/`LAST_WRITE_SUM` обработчика `write`), а сам читатель реально заблокировался и
//! был разбужен завершением строки. Подачу ввода имитируем программно (без живой клавиатуры),
//! поэтому тест headless-детерминирован.

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
use ferros::sched::thread::{self, STDIN_BLOCKS};
use ferros::syscall::elf::STDINTEST_ELF;
use ferros::syscall::{EXIT_CALLS, LAST_EXIT_CODE, LAST_WRITE_LEN, LAST_WRITE_SUM};
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

    console::init();
    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(STDINTEST_ELF, phys_mem_offset, &mut frame_allocator) };
    frame::install(frame_allocator);

    thread::start_preemption();

    // 1) Ждём, пока читатель заблокируется на пустом stdin.
    let mut spins = 0u64;
    while STDIN_BLOCKS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "reader never blocked on empty stdin");
        core::hint::spin_loop();
    }

    // 2) «Набираем» строку: символы копятся, Enter завершает её и будит читателя.
    for c in "hi\n".chars() {
        console::feed_char(c);
    }

    // 3) Ждём завершения читателя.
    while EXIT_CALLS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "reader never exited after input");
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

/// `read(0)` вернул набранную строку: читатель вышел с 0, а `write` обратно отдал ровно 3 байта
/// `"hi\n"` (сумма 'h'+'i'+'\n' = 104+105+10 = 219).
#[test_case]
fn stdin_delivers_typed_line() {
    assert_eq!(
        EXIT_CALLS.load(Ordering::SeqCst),
        1,
        "expected exactly one process (the reader) to exit"
    );
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "reader exited non-zero: read(0) failed or returned 0 bytes"
    );
    assert_eq!(
        LAST_WRITE_LEN.load(Ordering::SeqCst),
        3,
        "reader echoed a line of the wrong length"
    );
    assert_eq!(
        LAST_WRITE_SUM.load(Ordering::SeqCst),
        219,
        "reader echoed bytes other than \"hi\\n\""
    );
}
