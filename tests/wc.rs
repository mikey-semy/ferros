//! Интеграционный тест M9r: настоящая утилита `wc` на C (`/bin/wc`) запускается shell'ом как coreutil.
//!
//! Кормим shell строкой `wc -lw /HELLO.TXT > /SUB/WCOUT.TXT` — shell находит `/bin/wc` (C-бинарь на
//! диске), запускает через fork/execve с аргументами `-lw /HELLO.TXT`, перенаправив stdout в файл.
//! `wc` (поверх libc) разбирает флаги через **getopt** (`-lw` → строки+слова), читает файл через
//! **FILE*** (`fopen`/`fgetc`), считает и печатает `printf`. Проверяем, что обе программы вышли с 0 и
//! что в `/SUB/WCOUT.TXT` оказалось ровно `1 5 /HELLO.TXT\n` (1 строка, 5 слов).
//!
//! Почему это доказательство: libc реально хватает на НАСТОЯЩУЮ утилиту — getopt + stdio + printf
//! сошлись в одной программе, найденной в `/bin` и запущенной shell'ом с аргументами и редиректом.

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
use ferros::drivers::{console, virtio_blk};
use ferros::mm::frame::{self, BootInfoFrameAllocator};
use ferros::mm::{heap, paging};
use ferros::sched::thread::{self, STDIN_BLOCKS};
use ferros::syscall::elf::SHELL_ELF;
use ferros::syscall::{EXIT_CALLS, EXIT_CODE_SUM};
use x86_64::VirtAddr;

/// Ожидаемый вывод `wc -lw /HELLO.TXT`: 1 строка, 5 слов, затем имя файла.
/// (`/HELLO.TXT` = "ferros M6c: hello from FAT32!\n" — один `\n`, пять слов.)
const EXPECTED: &[u8] = b"1 5 /HELLO.TXT\n";

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
    assert!(
        virtio_blk::init(phys_mem_offset, &mut frame_allocator),
        "virtio-blk not initialized"
    );

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(SHELL_ELF, phys_mem_offset, &mut frame_allocator) };
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    while STDIN_BLOCKS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "shell never blocked on stdin");
        core::hint::spin_loop();
    }

    for c in "wc -lw /HELLO.TXT > /SUB/WCOUT.TXT\nexit 0\n".chars() {
        console::feed_char(c);
    }

    // wc + shell = 2 завершения.
    while EXIT_CALLS.load(Ordering::SeqCst) < 2 {
        spins += 1;
        assert!(spins < 5_000_000_000, "wc + shell never both exited");
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

/// `/bin/wc -lw /HELLO.TXT` посчитал 1 строку и 5 слов, и обе программы вышли с 0.
#[test_case]
fn shell_runs_c_wc_utility() {
    assert_eq!(
        EXIT_CODE_SUM.load(Ordering::SeqCst),
        0,
        "wc or the shell exited non-zero (utility not found / failed to open the file)"
    );
    let out = ferros::fs::open("/SUB/WCOUT.TXT").expect("wc did not create /SUB/WCOUT.TXT");
    assert_eq!(
        out, EXPECTED,
        "wc printed the wrong counts (expected 1 line, 5 words for /HELLO.TXT)"
    );
}
