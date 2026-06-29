//! Интеграционный тест M8c: ferros получает IP по **DHCP** через `smoltcp` поверх virtio-net.
//!
//! Поднимаем драйвер virtio-net, затем `net::dhcp_acquire` создаёт интерфейс smoltcp с нашим MAC,
//! запускает DHCP-клиента и крутит `poll`. User-mode сеть QEMU (SLIRP) держит DHCP-сервер и выдаёт
//! гостю **10.0.2.15/24**. Успех = получили именно этот адрес — значит весь стек работает: наш
//! `phy::Device` поверх драйвера, ARP, UDP и DHCP-обмен (DISCOVER→OFFER→REQUEST→ACK).

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
use ferros::serial_println;
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
        virtio_net::init(phys_mem_offset, &mut frame_allocator),
        "virtio-net driver init failed"
    );

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// DHCP через SLIRP выдаёт гостю 10.0.2.15/24.
#[test_case]
fn dhcp_gets_slirp_lease() {
    // 5 секунд аптайма на весь DHCP-обмен — SLIRP отвечает за миллисекунды, запас на повторы.
    let (addr, prefix) =
        ferros::net::dhcp_acquire(5_000_000_000).expect("DHCP did not configure within timeout");
    serial_println!(
        "[test] DHCP lease: {}.{}.{}.{}/{}",
        addr[0],
        addr[1],
        addr[2],
        addr[3],
        prefix
    );
    assert_eq!(addr, [10, 0, 2, 15], "unexpected DHCP address");
    assert_eq!(prefix, 24, "unexpected DHCP prefix length");
}
