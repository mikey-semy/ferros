//! Интеграционный тест M8d2: ferros **резолвит имя по DNS** через UDP-сокет поверх virtio-net.
//!
//! Поднимаем драйвер, затем `net::resolve` получает IP по DHCP и шлёт DNS-запрос A-записи серверу
//! SLIRP **10.0.2.3** (user-mode сеть QEMU держит там DNS-прокси, который переспрашивает резолвер
//! хоста). Спрашиваем `dns.google` — у Google это стабильное имя с anycast-адресами 8.8.8.8 / 8.8.4.4,
//! поэтому ответ детерминирован. Успех = вернулся один из них: значит работает весь путь UDP —
//! `bind`/`send`/`recv` сокета smoltcp плюс наша сборка/разбор DNS на проводе.
//!
//! Тесту нужен рабочий DNS на хосте (как и любой сетевой dev-машине — здесь он есть).

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

/// DNS-резолв `dns.google` через сервер SLIRP 10.0.2.3 даёт один из anycast-адресов Google.
#[test_case]
fn resolve_dns_google() {
    // 5 секунд аптайма на весь обмен (DHCP + DNS): SLIRP и хост-резолвер отвечают за миллисекунды.
    let addr = ferros::net::resolve("dns.google", [10, 0, 2, 3], 5_000_000_000)
        .expect("DNS did not resolve within timeout");
    serial_println!(
        "[test] dns.google -> {}.{}.{}.{}",
        addr[0],
        addr[1],
        addr[2],
        addr[3]
    );
    // dns.google — стабильное имя Google с этими двумя IPv4 (anycast).
    assert!(
        addr == [8, 8, 8, 8] || addr == [8, 8, 4, 4],
        "unexpected A record for dns.google: {:?}",
        addr
    );
}
