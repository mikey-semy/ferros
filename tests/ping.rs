//! Интеграционный тест M8d1: ferros **пингует шлюз** по ICMP через `smoltcp` поверх virtio-net.
//!
//! Поднимаем драйвер virtio-net, затем `net::ping` получает IP по DHCP и шлёт ICMP echo-запросы
//! шлюзу SLIRP **10.0.2.2**. User-mode сеть QEMU отвечает на ping своего шлюза внутри SLIRP (без
//! участия хоста), поэтому тест детерминирован. Успех = вернулся хотя бы один echo-ответ — значит
//! работает весь путь: наш `phy::Device`, ARP к шлюзу, IPv4 и ICMP (Echo Request → Echo Reply).

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

/// ICMP-ping шлюза SLIRP 10.0.2.2: уходит несколько echo-запросов, возвращаются echo-ответы.
#[test_case]
fn ping_slirp_gateway() {
    // 5 секунд аптайма на весь обмен (DHCP + пинги): SLIRP отвечает за миллисекунды, запас на повторы.
    let stats = ferros::net::ping([10, 0, 2, 2], 3, 5_000_000_000)
        .expect("ping setup failed (no NIC/DHCP)");
    serial_println!(
        "[test] ping 10.0.2.2: {}/{} replies",
        stats.received,
        stats.sent
    );
    assert!(
        stats.received >= 1,
        "no ICMP echo replies from gateway (sent {})",
        stats.sent
    );
}
