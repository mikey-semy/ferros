//! Интеграционный тест M8f: прикладной C тянет страницу по **HTTP** из кольца 3 — сетевой клиент.
//!
//! Спавним `HTTPGET_ELF` — C-программу (над libc), которая `socket(SOCK_STREAM)`/`connect`/`send`/
//! `recv` делает `GET / HTTP/1.0` к Cloudflare 1.1.1.1:80 и проверяет, что ответ начинается с
//! `HTTP/1.` → выход 0. Проверяем код выхода 0.
//!
//! Почему это доказательство: обычная C-программа в кольце 3 проходит путь сетевого клиента —
//! TCP-соединение и обмен HTTP — теми же POSIX-сокетами, что и Linux-софт. Нужен исходящий TCP хоста
//! (как и прочие сетевые демо M8d2/M8d3/M8e).

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
use ferros::syscall::elf::HTTPGET_ELF;
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
    // httpget ходит в сеть — нужна сетевая карта (стек поднимется лениво по DHCP в первом socket()).
    assert!(
        virtio_net::init(phys_mem_offset, &mut frame_allocator),
        "virtio-net not initialized"
    );

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(HTTPGET_ELF, phys_mem_offset, &mut frame_allocator) };
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "httpget never exited");
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

/// `httpget` вышла с 0 — значит из кольца 3 она прошла весь путь сетевого клиента: резолв по DNS,
/// TCP-соединение и обмен HTTP (ответ начался с `HTTP/1.`).
#[test_case]
fn c_program_fetches_a_web_page() {
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "httpget failed (DNS resolve, TCP connect/send/recv, or not an HTTP response)"
    );
}
