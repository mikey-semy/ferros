//! Интеграционный тест M9l: прикладной C делает файловый ввод-вывод через libc.
//!
//! Спавним `CATFILE_ELF` — C-программу (над libc), которая `open`/`read`/`close` файла `/HELLO.TXT`,
//! выводит его на stdout (`write`) и сама сверяет содержимое с фикстурой → выход 0. Проверяем код
//! выхода 0 и что на fd 1 ушли ровно байты `/HELLO.TXT`.
//!
//! Почему это доказательство: обычный C-код (через нашу libc) реально читает файл с FAT-диска и
//! пишет его — то есть прикладной C-ввод-вывод работает на ferros, а не только вычисления в памяти.

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
use ferros::syscall::elf::CATFILE_ELF;
use ferros::syscall::{EXIT_CALLS, LAST_EXIT_CODE, LAST_WRITE_FD, LAST_WRITE_LEN, LAST_WRITE_SUM};
use x86_64::VirtAddr;

/// Содержимое `/HELLO.TXT` (build.rs `FAT_TEST_CONTENT`) — то, что catfile прочитает и выведет.
const EXPECTED: &[u8] = b"ferros M6c: hello from FAT32!\n";

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
    // catfile открывает/читает /HELLO.TXT — нужен блочный диск (FAT).
    assert!(
        virtio_blk::init(phys_mem_offset, &mut frame_allocator),
        "virtio-blk not initialized"
    );

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(CATFILE_ELF, phys_mem_offset, &mut frame_allocator) };
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "catfile never exited");
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

/// `catfile` вышла с 0 (прочитала ожидаемое) и вывела содержимое `/HELLO.TXT` на fd 1.
#[test_case]
fn c_program_reads_a_file_via_libc() {
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "catfile failed (1 open; 2 read; 3 content mismatch)"
    );
    assert_eq!(
        LAST_WRITE_FD.load(Ordering::SeqCst),
        1,
        "output was not to fd 1"
    );
    assert_eq!(
        LAST_WRITE_LEN.load(Ordering::SeqCst),
        EXPECTED.len() as u64,
        "catfile wrote the wrong number of bytes"
    );
    let expected_sum: u64 = EXPECTED.iter().map(|&b| b as u64).sum();
    assert_eq!(
        LAST_WRITE_SUM.load(Ordering::SeqCst),
        expected_sum,
        "catfile wrote the wrong bytes (not /HELLO.TXT's content)"
    );
}
