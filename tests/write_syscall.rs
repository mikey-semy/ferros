//! Интеграционный тест M6g3: запись файла через сисколлы из кольца 3.
//!
//! Спавним `writetest`: он создаёт файл (`open(O_CREAT|O_WRONLY|O_TRUNC)`), пишет строку,
//! закрывает (сброс на диск), открывает заново на чтение и сверяет прочитанное с записанным.
//! Выходит с 0 при успехе или с отличимым ненулевым кодом на каждом шаге. Файл `WRITTEN.TXT` —
//! собственный для теста (другие тесты его не читают).
//!
//! Почему это доказательство: код выхода 0 означает, что весь путь записи отработал — `open`
//! с флагами (создание/обрезание), `write` в буфер файла, `close` со сбросом на FAT-диск, и
//! повторное `open`/`read` увидело именно записанные байты.

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
use ferros::syscall::elf::WRITETEST_ELF;
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

    // Файловые сисколлы пишут/читают FAT → нужен диск.
    assert!(
        virtio_blk::init(phys_mem_offset, &mut frame_allocator),
        "virtio-blk not initialized"
    );

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(WRITETEST_ELF, phys_mem_offset, &mut frame_allocator) };
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "writetest never exited");
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

/// `writetest` завершился с 0: `open(O_CREAT)` + `write` + `close`(сброс) + повторное чтение
/// вернули записанное.
#[test_case]
fn write_syscall_roundtrips() {
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "writetest failed (nonzero exit code pinpoints the failing step: 1 open,2 write,3 close,4 reopen,5 read,6 mismatch)"
    );
}
