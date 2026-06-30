//! Интеграционный тест M9n: прикладной C использует **stdio (`FILE *`)** поверх libc.
//!
//! Спавним `STDIOTEST_ELF` — C-программу (над libc), которая пишет файл `/SUB/STDIO.TXT` через
//! `fopen("w")`/`fputs`/`fprintf`/`fputc`/`fclose`, читает его обратно через `fopen("r")`/`fgets`/
//! `fgetc`/`feof` и сама сверяет содержимое → выход 0. Проверяем код выхода 0.
//!
//! Почему это доказательство: обычная C-программа делает буферо-ориентированный (stream) ввод-вывод
//! через `FILE *` — как почти весь реальный C-софт, — а не только сырые дескрипторы. Нужен диск (FAT).

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
use ferros::sched::thread;
use ferros::syscall::elf::STDIOTEST_ELF;
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
    console::init();
    // stdiotest пишет/читает файл на диске — нужен блочный диск (FAT).
    assert!(
        virtio_blk::init(phys_mem_offset, &mut frame_allocator),
        "virtio-blk not initialized"
    );

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(STDIOTEST_ELF, phys_mem_offset, &mut frame_allocator) };
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "stdiotest never exited");
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

/// `stdiotest` вышла с 0 — значит round-trip через `FILE *` (запись `fopen("w")`/`fputs`/`fprintf` и
/// чтение `fopen("r")`/`fgets`/`fgetc` с тем же содержимым) отработал.
#[test_case]
fn c_program_does_stdio_roundtrip() {
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "stdiotest failed (FILE* write/read round-trip mismatch)"
    );
}
