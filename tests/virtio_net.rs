//! Интеграционный тест M8b: ferros передаёт и принимает Ethernet-кадры через virtio-net.
//!
//! Детерминированная проверка без живой сети: шлём **ARP-запрос «кто такой 10.0.2.2?»** (шлюз
//! user-mode сети QEMU — SLIRP) и ждём **ARP-ответ**. SLIRP отвечает на ARP к своему шлюзу своим
//! MAC — значит полный круг (TX кадра → SLIRP → RX ответа) состоялся, если мы получили ARP-reply,
//! у которого sender-IP = 10.0.2.2.
//!
//! Заодно (как в M8a) проверяем, что MAC карты начинается с OUI QEMU `52:54:00`.

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

// Адреса user-mode сети QEMU (SLIRP): гость 10.0.2.15, шлюз 10.0.2.2.
const GUEST_IP: [u8; 4] = [10, 0, 2, 15];
const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];
const ETHERTYPE_ARP: u16 = 0x0806;
const ARP_REQUEST: u16 = 1;
const ARP_REPLY: u16 = 2;

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
        "virtio-net driver init failed (NIC present? queues/buffers allocated?)"
    );

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// Собирает Ethernet+ARP-запрос «кто такой `target_ip`?» от нашего `mac`/`GUEST_IP` в `buf`,
/// возвращает длину (42 байта).
fn build_arp_request(mac: [u8; 6], target_ip: [u8; 4], buf: &mut [u8; 42]) {
    // Ethernet: dst=broadcast, src=наш MAC, type=ARP.
    buf[0..6].copy_from_slice(&[0xff; 6]);
    buf[6..12].copy_from_slice(&mac);
    buf[12..14].copy_from_slice(&ETHERTYPE_ARP.to_be_bytes());
    // ARP: htype=1(Ethernet), ptype=0x0800(IPv4), hlen=6, plen=4, oper=request.
    buf[14..16].copy_from_slice(&1u16.to_be_bytes());
    buf[16..18].copy_from_slice(&0x0800u16.to_be_bytes());
    buf[18] = 6;
    buf[19] = 4;
    buf[20..22].copy_from_slice(&ARP_REQUEST.to_be_bytes());
    buf[22..28].copy_from_slice(&mac); // sender hardware addr
    buf[28..32].copy_from_slice(&GUEST_IP); // sender protocol addr
    buf[32..38].copy_from_slice(&[0u8; 6]); // target hardware addr (неизвестен)
    buf[38..42].copy_from_slice(&target_ip); // target protocol addr
}

/// Это ARP-ответ от `GATEWAY_IP`? Проверяем ethertype, oper и sender-IP.
fn is_arp_reply_from_gateway(frame: &[u8]) -> bool {
    frame.len() >= 42
        && u16::from_be_bytes([frame[12], frame[13]]) == ETHERTYPE_ARP
        && u16::from_be_bytes([frame[20], frame[21]]) == ARP_REPLY
        && frame[28..32] == GATEWAY_IP
}

/// Круг TX→RX: ARP-запрос к шлюзу SLIRP → приходит ARP-ответ с sender-IP 10.0.2.2.
#[test_case]
fn arp_round_trip_with_slirp_gateway() {
    let mac = virtio_net::mac().expect("MAC not read (init failed)");
    assert_eq!(
        &mac[0..3],
        &[0x52, 0x54, 0x00],
        "unexpected MAC OUI; got {mac:02x?}"
    );

    let mut req = [0u8; 42];
    build_arp_request(mac, GATEWAY_IP, &mut req);
    assert!(virtio_net::send(&req), "TX of ARP request failed");

    // Опрашиваем RX, пока не придёт ARP-ответ от шлюза (или software-таймаут — чтобы не виснуть).
    let mut buf = [0u8; 1514];
    let mut got = None;
    for _ in 0..200_000_000u64 {
        if let Some(n) = virtio_net::recv(&mut buf) {
            if is_arp_reply_from_gateway(&buf[..n]) {
                got = Some(n);
                break;
            }
            // не наш кадр (другой широковещательный) — продолжаем опрос
        }
        core::hint::spin_loop();
    }

    let n = got.expect("no ARP reply from gateway 10.0.2.2 received (TX/RX round-trip failed)");
    let sender_mac = &buf[22..28];
    serial_println!(
        "[test] ARP reply: 10.0.2.2 is at {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} ({n} bytes)",
        sender_mac[0],
        sender_mac[1],
        sender_mac[2],
        sender_mac[3],
        sender_mac[4],
        sender_mac[5]
    );
}
