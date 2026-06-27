//! Интеграционный тест M6a: перечисление шины PCI.
//!
//! Поднимаем пейджинг + кучу, перечисляем PCI и проверяем, что нашли (а) хост-мост i440fx
//! (присутствует всегда — значит чтение конфигурации работает), и (б) диск virtio-blk
//! (подключён через QEMU-аргументы в `Cargo.toml`), а у него BAR0 декодируется как непустой
//! I/O-BAR — именно на нём M6b будет «ездить» по legacy-интерфейсу virtio.
//!
//! Почему это доказательство: без рабочего механизма 0xCF8/0xCFC хост-мост бы не нашёлся;
//! без подключённого устройства и верного разбора BAR — провалилась бы проверка virtio.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use ferros::drivers::pci::{self, Bar};
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::{heap, paging};
use x86_64::VirtAddr;

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init вызывается один раз.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// Хост-мост i440fx (vendor 0x8086, device 0x1237) есть всегда — базовая проверка, что
/// механизм чтения конфигурационного пространства (0xCF8/0xCFC) вообще работает.
#[test_case]
fn host_bridge_present() {
    assert!(
        pci::find(0x8086, 0x1237).is_some(),
        "i440fx host bridge (8086:1237) not found — PCI config reads broken?"
    );
}

/// Диск virtio-blk найден на шине, и его BAR0 — непустой I/O-BAR. Переходное устройство
/// QEMU отвечает на legacy-идентификатор 0x1AF4:0x1001 и отдаёт legacy I/O-регистры в BAR0
/// (это и есть интерфейс, по которому M6b будет читать секторы).
#[test_case]
fn virtio_blk_present_with_io_bar() {
    let dev = pci::find(0x1AF4, 0x1001).expect("legacy virtio-blk (1af4:1001) not found on bus");
    assert!(
        matches!(dev.bar(0), Bar::Io { base } if base != 0),
        "virtio-blk BAR0 is not a nonzero I/O BAR: {:?}",
        dev.bar(0)
    );
}
