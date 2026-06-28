//! Интеграционный тест M7f: shell находит и запускает внешние утилиты из `/bin`.
//!
//! «Набираем» три команды ГОЛЫМИ именами (без `/`): `echo hi`, `cat /HELLO.TXT`, `ls /`. Shell
//! для каждой ищет программу в `/bin` (`/bin/echo` и т.д.), `fork`+`execve`+`wait4`. Все три —
//! идемпотентны (ничего не пишут на диск), поэтому при любом числе прогонов выходят с 0.
//!
//! Почему это доказательство: `EXIT_CALLS == 4` — все три утилиты И shell завершились через
//! `exit` (а не упали: сбой в кольце 3 убил бы процесс мимо `exit`, и счётчик был бы меньше);
//! `EXIT_CODE_SUM == 0` — каждая вышла с 0, т.е. поиск в `/bin` сработал и утилита отработала
//! без ошибки (любой ненулевой код — не найдена/не открылся файл/каталог — испортил бы сумму).
//! `mkdir` проверяется отдельно (он пишет на диск — см. `tests/mkdir.rs`).

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
use ferros::syscall::elf::SHELL_ELF;
use ferros::syscall::{EXIT_CALLS, EXIT_CODE_SUM};
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
    assert!(
        ferros::drivers::virtio_blk::init(phys_mem_offset, &mut frame_allocator),
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

    for c in "echo hi\ncat /HELLO.TXT\nls /\nexit 0\n".chars() {
        console::feed_char(c);
    }

    // Три утилиты + shell = 4 завершения.
    while EXIT_CALLS.load(Ordering::SeqCst) < 4 {
        spins += 1;
        assert!(spins < 5_000_000_000, "utilities + shell never all exited");
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

/// echo/cat/ls найдены в /bin, запущены и завершились с 0 (как и shell по `exit 0`).
#[test_case]
fn shell_finds_and_runs_bin_utilities() {
    assert_eq!(
        EXIT_CALLS.load(Ordering::SeqCst),
        4,
        "expected echo + cat + ls + shell to each exit (none crashed)"
    );
    assert_eq!(
        EXIT_CODE_SUM.load(Ordering::SeqCst),
        0,
        "a /bin utility exited non-zero (not found, or failed to open its file/dir)"
    );
}
