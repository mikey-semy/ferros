//! Интеграционный тест M8d3: прикладной C делает **сетевой ввод-вывод через сокет-сисколлы**.
//!
//! Спавним `DNSCLIENT_ELF` — C-программу (над libc), которая `socket`/`sendto`/`recvfrom` резолвит
//! `dns.google` через DNS-сервер SLIRP 10.0.2.3 и сама сверяет ответ со стабильными адресами Google
//! (8.8.8.8 / 8.8.4.4) → выход 0. Проверяем код выхода 0.
//!
//! Почему это доказательство: обычная C-программа в кольце 3 ходит в сеть **теми же POSIX-сокетами**,
//! что и Linux-софт (socket/sendto/recvfrom поверх нашего общего стека smoltcp) — сеть сомкнулась с
//! north-star целью. Нужен рабочий DNS на хосте (как и в M8d2; на сетевой dev-машине он есть).

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
use ferros::drivers::{console, virtio_net};
use ferros::mm::frame::{self, BootInfoFrameAllocator};
use ferros::mm::{heap, paging};
use ferros::sched::thread;
use ferros::syscall::elf::DNSCLIENT_ELF;
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
    console::init();
    // dnsclient ходит в сеть — нужна сетевая карта (стек поднимется лениво по DHCP в первом socket()).
    assert!(
        virtio_net::init(phys_mem_offset, &mut frame_allocator),
        "virtio-net not initialized"
    );

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(DNSCLIENT_ELF, phys_mem_offset, &mut frame_allocator) };
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "dnsclient never exited");
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

/// `dnsclient` вышла с 0 — значит через сокет-сисколлы она отправила DNS-запрос и получила ответ с
/// ожидаемым A-адресом (`socket`→`sendto`→`recvfrom` поверх общего стека сработали из кольца 3).
#[test_case]
fn c_program_resolves_dns_via_sockets() {
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "dnsclient failed (socket/sendto/recvfrom, or unexpected A record)"
    );
}
