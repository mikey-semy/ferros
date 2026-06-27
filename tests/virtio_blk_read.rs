//! Интеграционный тест M6b: читаем сектор 0 диска virtio-blk и сверяем сигнатуру.
//!
//! `build.rs` форматирует образ диска (`target/ferros-disk.img`) как FAT32; QEMU подключает
//! его как virtio-blk (`Cargo.toml`). `main` поднимает драйвер, читает сектор 0 (это
//! загрузочный сектор FAT) и сверяет сигнатуру `0x55AA` в его конце. Проверка идёт в `main`
//! (нужен аллокатор фреймов), результат — в статиках, а `#[test_case]` их утверждает.
//!
//! Почему это доказательство: если бы virtqueue/DMA/опрос были настроены неверно, чтение
//! зависло бы (таймаут) или вернуло мусор, и сигнатура загрузсектора бы не совпала. (Разбор
//! самого FAT проверяет отдельный тест `fat_read`.)

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};
use ferros::drivers::virtio_blk::{self, SECTOR_SIZE};
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::{heap, paging};
use x86_64::VirtAddr;

entry_point!(main);

/// Драйвер инициализировался (устройство найдено и поднято).
static INIT_OK: AtomicBool = AtomicBool::new(false);
/// Чтение сектора 0 завершилось успешно.
static READ_OK: AtomicBool = AtomicBool::new(false);
/// Сектор 0 оканчивается сигнатурой загрузочного сектора 0x55AA (это валидный boot sector).
static SIGNATURE_OK: AtomicBool = AtomicBool::new(false);

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init вызывается один раз.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");

    if virtio_blk::init(phys_mem_offset, &mut frame_allocator) {
        INIT_OK.store(true, Ordering::SeqCst);
        let mut sector = [0u8; SECTOR_SIZE];
        if virtio_blk::read_sector(0, &mut sector).is_ok() {
            READ_OK.store(true, Ordering::SeqCst);
            // Любой загрузочный сектор FAT оканчивается сигнатурой 0x55 0xAA.
            let boot_signature_ok = sector[510] == 0x55 && sector[511] == 0xAA;
            SIGNATURE_OK.store(boot_signature_ok, Ordering::SeqCst);
        }
    }

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// Драйвер нашёл и поднял устройство virtio-blk.
#[test_case]
fn virtio_blk_initialized() {
    assert!(
        INIT_OK.load(Ordering::SeqCst),
        "virtio-blk device was not initialized"
    );
}

/// Чтение сектора 0 прошло, и это валидный загрузочный сектор FAT (сигнатура 0x55AA).
#[test_case]
fn reads_boot_sector() {
    assert!(READ_OK.load(Ordering::SeqCst), "read_sector(0) failed");
    assert!(
        SIGNATURE_OK.load(Ordering::SeqCst),
        "sector 0 is not a valid boot sector (missing 0x55AA signature)"
    );
}
