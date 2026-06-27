//! Интеграционный тест M6b: читаем сектор 0 диска virtio-blk и сверяем сигнатуру.
//!
//! `build.rs` создаёт образ диска (`target/ferros-disk.img`) с сигнатурой `FERROSM6` в
//! начале сектора 0; QEMU подключает его как virtio-blk (`Cargo.toml`). `main` поднимает
//! драйвер, читает сектор 0 и сверяет первые 8 байт. Проверка идёт в `main` (нужен
//! аллокатор фреймов), результат — в статиках, а `#[test_case]` их утверждает.
//!
//! Почему это доказательство: если бы virtqueue/DMA/опрос были настроены неверно, чтение
//! зависло бы (таймаут) или вернуло мусор/ошибку, и сигнатура бы не совпала.

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
/// Первые 8 байт сектора 0 совпали с сигнатурой из build.rs.
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
            SIGNATURE_OK.store(&sector[..8] == b"FERROSM6", Ordering::SeqCst);
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

/// Чтение сектора 0 прошло, и в нём — сигнатура `FERROSM6` из образа диска (build.rs).
#[test_case]
fn reads_sector_zero_signature() {
    assert!(READ_OK.load(Ordering::SeqCst), "read_sector(0) failed");
    assert!(
        SIGNATURE_OK.load(Ordering::SeqCst),
        "sector 0 did not start with the expected FERROSM6 signature"
    );
}
