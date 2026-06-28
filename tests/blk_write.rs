//! Интеграционный тест M6g1: запись сектора на диск virtio-blk и чтение обратно.
//!
//! Берём сектор далеко за пределами файловой системы (образ — 64 МиБ = 131072 сектора, FS
//! занимает лишь начало), НЕДЕСТРУКТИВНО: читаем оригинал → пишем узор → читаем обратно →
//! возвращаем оригинал. Так тест не портит образ для других прогонов.
//!
//! Почему это доказательство: узор возвращается побайтно ⇒ цепочка дескрипторов записи
//! (заголовок `VIRTIO_BLK_T_OUT` → данные читает устройство → статус) и DMA память→устройство
//! отработали; а чтение того же сектора видит именно записанное.

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

/// Сектор для пробной записи: далеко за метаданными FS, но в пределах ёмкости (131072 сектора).
const TEST_LBA: u64 = 100_000;

/// Узор прошёл круг «запись → чтение» без искажений.
static ROUNDTRIP_OK: AtomicBool = AtomicBool::new(false);

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init вызывается один раз.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");

    assert!(
        virtio_blk::init(phys_mem_offset, &mut frame_allocator),
        "virtio-blk not initialized"
    );

    // 1) Сохраняем оригинал сектора, чтобы вернуть его в конце (недеструктивно).
    let mut original = [0u8; SECTOR_SIZE];
    virtio_blk::read_sector(TEST_LBA, &mut original).expect("read original");

    // 2) Пишем узор (каждый байт — функция от индекса, чтобы поймать перестановки/сдвиги).
    let mut pattern = [0u8; SECTOR_SIZE];
    for (i, b) in pattern.iter_mut().enumerate() {
        *b = (i as u8) ^ 0xA5;
    }
    virtio_blk::write_sector(TEST_LBA, &pattern).expect("write pattern");

    // 3) Читаем обратно.
    let mut readback = [0u8; SECTOR_SIZE];
    virtio_blk::read_sector(TEST_LBA, &mut readback).expect("read back");

    // 4) Возвращаем оригинал — образ остаётся неизменным для других тестов.
    virtio_blk::write_sector(TEST_LBA, &original).expect("restore original");

    ROUNDTRIP_OK.store(readback == pattern, Ordering::SeqCst);

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// Записанный сектор прочитался обратно байт-в-байт.
#[test_case]
fn write_then_read_roundtrips() {
    assert!(
        ROUNDTRIP_OK.load(Ordering::SeqCst),
        "sector read back did not match what was written"
    );
}
