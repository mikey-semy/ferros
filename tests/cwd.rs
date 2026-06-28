//! Интеграционный тест M7c: рабочий каталог процесса (`getcwd`/`chdir`/относительные пути).
//!
//! Спавним `cwdtest`: он проверяет, что стартует в `/`, спускается в `SUB` (`chdir`), видит это в
//! `getcwd` (`/SUB`), открывает `INSIDE.TXT` ОТНОСИТЕЛЬНЫМ путём (резолвится под `/SUB`) и читает
//! его, затем поднимается `..` обратно в `/`. На каждом шаге при ошибке выходит отличимым кодом
//! 1–7; полный успех — 0.
//!
//! Почему это доказательство cwd: код 0 означает, что ядро хранит per-process cwd, `chdir`
//! нормализует и проверяет каталог, `getcwd` отдаёт его обратно, а относительный `open` достроился
//! от cwd (а `..` — поднялся к корню). Каталог `SUB`/`SUB/INSIDE.TXT` кладёт на диск build.rs.

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
use ferros::syscall::elf::CWDTEST_ELF;
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

    // chdir/open читают каталоги/файлы с диска → нужен virtio-blk.
    assert!(
        ferros::drivers::virtio_blk::init(phys_mem_offset, &mut frame_allocator),
        "virtio-blk not initialized"
    );

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(CWDTEST_ELF, phys_mem_offset, &mut frame_allocator) };
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) == 0 {
        spins += 1;
        assert!(spins < 5_000_000_000, "cwdtest never exited");
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

/// `cwdtest` прошёл все шаги cwd и вышел с 0 (1–7 означали бы конкретный провалившийся шаг).
#[test_case]
fn cwd_chdir_getcwd_and_relative_paths() {
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "cwdtest failed a step (1=initial getcwd, 2=chdir SUB, 3=getcwd /SUB, 4=open, 5=read, 6=chdir .., 7=getcwd /)"
    );
}
