//! Интеграционный тест M9m: утилита `cat` на C (`/bin/ccat`) запускается shell'ом как coreutil.
//!
//! Кормим shell строкой `ccat /HELLO.TXT > /SUB/CCAT.TXT` — shell находит `/bin/ccat` (C-бинарь на
//! диске), запускает его через fork/execve с аргументом `/HELLO.TXT`, перенаправив stdout в файл.
//! `ccat` (через нашу libc) открывает/читает `/HELLO.TXT` и пишет содержимое в fd 1 (→ файл).
//! Проверяем, что обе программы вышли с 0 и что в `/SUB/CCAT.TXT` оказалось ровно содержимое
//! `/HELLO.TXT`.
//!
//! Почему это доказательство: прикладной C работает как НАСТОЯЩАЯ утилита окружения — поиск в
//! `/bin`, передача `argv` через execve, файловый ввод-вывод и перенаправление shell'а сошлись.

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

/// Содержимое `/HELLO.TXT` (build.rs `FAT_TEST_CONTENT`) — то, что `ccat` должен переписать в файл.
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

    for c in "ccat /HELLO.TXT > /SUB/CCAT.TXT\nexit 0\n".chars() {
        console::feed_char(c);
    }

    // ccat + shell = 2 завершения.
    while EXIT_CALLS.load(Ordering::SeqCst) < 2 {
        spins += 1;
        assert!(spins < 5_000_000_000, "ccat + shell never both exited");
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

/// `/bin/ccat` (C) переписал содержимое `/HELLO.TXT` в `/SUB/CCAT.TXT`, и обе программы вышли с 0.
#[test_case]
fn shell_runs_c_cat_utility() {
    assert_eq!(
        EXIT_CODE_SUM.load(Ordering::SeqCst),
        0,
        "ccat or the shell exited non-zero (utility not found / failed to open the file)"
    );
    let out = ferros::fs::open("/SUB/CCAT.TXT").expect("ccat did not create /SUB/CCAT.TXT");
    assert_eq!(
        out, EXPECTED,
        "ccat wrote the wrong content (expected /HELLO.TXT's bytes)"
    );
}
