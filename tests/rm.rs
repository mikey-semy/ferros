//! Интеграционный тест M7g3: `rm`/`rmdir` удаляют файл и пустой каталог.
//!
//! «Набираем» shell'у последовательность, которая СОЗДАЁТ и тут же УДАЛЯЕТ: `echo x > файл` →
//! `rm файл`; `mkdir каталог` → `rmdir каталог`. Так тест самоочищается (после прогона ничего не
//! остаётся), поэтому повторные прогоны на том же образе диска не накапливают состояние.
//!
//! Доказательство: `EXIT_CODE_SUM == 0` — все четыре утилиты вышли с 0, т.е. `rm` удалил
//! СУЩЕСТВУЮЩИЙ файл, а `rmdir` — существующий пустой каталог (удаление несуществующего дало бы
//! ненулевой код). А `fs::lookup` подтверждает, что обоих больше нет на диске.

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

    for c in
        "echo x > /SUB/M7GRF.TXT\nrm /SUB/M7GRF.TXT\nmkdir /SUB/M7GRD\nrmdir /SUB/M7GRD\nexit 0\n"
            .chars()
    {
        console::feed_char(c);
    }

    // echo + rm + mkdir + rmdir + shell = 5 завершений.
    while EXIT_CALLS.load(Ordering::SeqCst) < 5 {
        spins += 1;
        assert!(
            spins < 5_000_000_000,
            "rm sequence + shell never all exited"
        );
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

/// `rm`/`rmdir` удалили созданные файл и каталог: все утилиты вышли с 0 и обоих больше нет на диске.
#[test_case]
fn rm_and_rmdir_remove_a_file_and_an_empty_dir() {
    assert_eq!(
        EXIT_CALLS.load(Ordering::SeqCst),
        5,
        "expected echo + rm + mkdir + rmdir + shell to each exit"
    );
    assert_eq!(
        EXIT_CODE_SUM.load(Ordering::SeqCst),
        0,
        "a utility exited non-zero: rm/rmdir failed to remove an existing target"
    );
    assert!(
        ferros::fs::open("/SUB/M7GRF.TXT").is_err(),
        "rm did not remove the file (still on disk)"
    );
    assert!(
        matches!(
            ferros::fs::lookup("/SUB/M7GRD"),
            Err(ferros::fs::fat::FatError::NotFound)
        ),
        "rmdir did not remove the directory (still on disk)"
    );
}
