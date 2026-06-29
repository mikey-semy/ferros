//! Интеграционный тест M8a: ferros видит сетевую карту virtio-net и читает её MAC.
//!
//! Поднимаем кучу (нужна `pci::find`), зовём `virtio_net::init()` — он находит устройство
//! `1af4:1000` на PCI, делает базовое квитирование и читает MAC из device-config. Проверяем, что
//! карта найдена и MAC начинается с OUI QEMU `52:54:00` (адрес по умолчанию его user-mode NIC).
//!
//! Это первый шаг сетевого тира: подтверждаем, что QEMU-конфиг даёт нам NIC и мы умеем его видеть.
//! Очереди приёма/передачи и обмен кадрами — M8b.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use ferros::drivers::virtio_net;
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::{heap, paging};
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

    assert!(
        virtio_net::init(),
        "virtio-net NIC not detected (is the QEMU -device virtio-net-pci present?)"
    );

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// virtio-net найдена и MAC прочитан: OUI совпадает с дефолтным QEMU `52:54:00`.
#[test_case]
fn detects_virtio_net_and_reads_mac() {
    let mac = virtio_net::mac().expect("MAC was not read (virtio-net init failed)");
    assert_eq!(
        &mac[0..3],
        &[0x52, 0x54, 0x00],
        "unexpected MAC OUI (QEMU user NIC default is 52:54:00); got {mac:02x?}"
    );
}
