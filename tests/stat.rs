//! Интеграционный тест M9c: `stat`/`fstat` из кольца 3.
//!
//! Спавним `stattest`: `stat` обычного файла (`/HELLO.TXT` — тип REG, размер 30), `stat` каталога
//! (`/SUB` — тип DIR), `fstat(stdout)` (символьное устройство — на этом стоит `isatty`), `fstat`
//! открытого файла (REG + тот же размер). Выходит с 0 при успехе, иначе с отличимым кодом (10..20).
//!
//! Почему это доказательство: чтобы пройти проверки, ядро должно разрешить путь в FAT, синтезировать
//! линуксовый `struct stat` с верными `st_mode`/`st_size` по точным смещениям ABI и скопировать его
//! в пользователя; `fstat` — ещё и различить подложку дескриптора (файл vs консоль).

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
use ferros::drivers::virtio_blk;
use ferros::mm::frame::{self, BootInfoFrameAllocator};
use ferros::mm::{heap, paging};
use ferros::sched::thread;
use ferros::syscall::elf::STATTEST_ELF;
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

    // stattest читает FAT (stat/open), поэтому блочное устройство нужно.
    assert!(
        virtio_blk::init(phys_mem_offset, &mut frame_allocator),
        "virtio-blk not initialized"
    );

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(STATTEST_ELF, phys_mem_offset, &mut frame_allocator) };
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "stattest never exited");
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

/// `stattest` завершился с 0: `stat`/`fstat` отдают верные тип и размер для файла, каталога и
/// дескрипторов.
#[test_case]
fn stat_and_fstat_report_type_and_size() {
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "stattest failed (10/11/12 stat file; 13/14 stat dir; 15/16 fstat stdout; 17..20 fstat fd)"
    );
}
