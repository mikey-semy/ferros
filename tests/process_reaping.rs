//! Интеграционный тест M6e3: reaper освобождает память завершённого процесса.
//!
//! Спавним процесс «reader» (открывает файл с диска и выходит). После его завершения зовём
//! `thread::reap()` и проверяем, что (1) фреймы его адресного пространства вернулись в список
//! свободных, (2) его таблица дескрипторов удалена из реестра процессов, и (3) ядро живо.
//!
//! Почему это доказательство: до reaping всё текло; теперь освобождённые фреймы видны в
//! списке свободных, а запись в `PROCESSES` исчезает (иначе переиспользованный PML4 унаследовал
//! бы чужие дескрипторы).

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicUsize, Ordering};
use ferros::arch::x86_64::syscall::spawn_user;
use ferros::mm::frame::{self, BootInfoFrameAllocator};
use ferros::mm::{heap, paging};
use ferros::sched::thread;
use ferros::syscall::elf::READER_ELF;
use ferros::syscall::{files, EXIT_CALLS};
use x86_64::VirtAddr;

entry_point!(main);

static FREE_BEFORE: AtomicUsize = AtomicUsize::new(0);
static FREE_AFTER: AtomicUsize = AtomicUsize::new(0);
static PROC_BEFORE: AtomicUsize = AtomicUsize::new(0);
static PROC_AFTER: AtomicUsize = AtomicUsize::new(0);

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init один раз.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");

    // reader открывает файл → нужен диск.
    assert!(
        ferros::drivers::virtio_blk::init(phys_mem_offset, &mut frame_allocator),
        "virtio-blk not initialized"
    );

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(READER_ELF, phys_mem_offset, &mut frame_allocator) };

    // Передаём аллокатор в глобальное владение — reaper освобождает через него.
    frame::install(frame_allocator);

    thread::start_preemption();
    // Крутимся, пока reader не отработает (открыл файл, прочитал, вышел).
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) == 0 {
        spins += 1;
        assert!(spins < 5_000_000_000, "reader never exited");
        core::hint::spin_loop();
    }
    thread::stop_preemption();

    // Снимок ДО reaping: процесс завершён, но его память ещё не освобождена.
    FREE_BEFORE.store(
        frame::with_global(|fa| fa.free_list_len()).unwrap(),
        Ordering::SeqCst,
    );
    PROC_BEFORE.store(files::process_count(), Ordering::SeqCst);

    thread::reap();

    FREE_AFTER.store(
        frame::with_global(|fa| fa.free_list_len()).unwrap(),
        Ordering::SeqCst,
    );
    PROC_AFTER.store(files::process_count(), Ordering::SeqCst);

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// Reaping вернул фреймы адресного пространства процесса в список свободных.
#[test_case]
fn reaping_reclaims_frames() {
    let before = FREE_BEFORE.load(Ordering::SeqCst);
    let after = FREE_AFTER.load(Ordering::SeqCst);
    assert!(
        after > before,
        "no frames reclaimed by reaping (before={before}, after={after})"
    );
}

/// Reaping удалил таблицу дескрипторов процесса из реестра (reader открывал файл).
#[test_case]
fn reaping_drops_fd_table() {
    let before = PROC_BEFORE.load(Ordering::SeqCst);
    let after = PROC_AFTER.load(Ordering::SeqCst);
    assert!(
        before >= 1,
        "reader should have had an fd table (it opened a file)"
    );
    assert!(
        after < before,
        "process fd table was not removed by reaping (before={before}, after={after})"
    );
}
