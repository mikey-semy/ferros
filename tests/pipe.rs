//! Интеграционный тест M7g2: пайп `|` соединяет stdout одной программы со stdin другой.
//!
//! «Набираем» shell'у `echo piped | cat > /SUB/M7GPIPE.TXT`. Shell создаёт канал и форкает два
//! процесса: `echo` (stdout → конец записи канала) и `cat` (stdin → конец чтения канала, stdout →
//! файл по редиректу). `echo` пишет `piped\n` в канал и завершается; `cat` читает это из канала,
//! видит EOF (писателей не осталось) и пишет прочитанное в `/SUB/M7GPIPE.TXT`.
//!
//! Почему это доказательство пайпа: `cat` получил данные ИМЕННО через канал (его stdin —
//! конец чтения), а EOF он увидел, потому что `echo` закрыл конец записи на `exit`. Итог —
//! `/SUB/M7GPIPE.TXT` == `"piped\n"`. `EXIT_CALLS == 3` — оба процесса пайпа и shell завершились.

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
use ferros::drivers::console;
use ferros::mm::frame::{self, BootInfoFrameAllocator};
use ferros::mm::{heap, paging};
use ferros::sched::thread::{self, STDIN_BLOCKS};
use ferros::syscall::elf::SHELL_ELF;
use ferros::syscall::EXIT_CALLS;
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
    assert!(
        ferros::drivers::virtio_blk::init(phys_mem_offset, &mut frame_allocator),
        "virtio-blk not initialized"
    );

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(SHELL_ELF, phys_mem_offset, &mut frame_allocator) };
    frame::install(frame_allocator);

    thread::start_preemption();

    let mut spins = 0u64;
    while STDIN_BLOCKS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "shell never blocked on stdin");
        core::hint::spin_loop();
    }

    for c in "echo piped | cat > /SUB/M7GPIPE.TXT\nexit 0\n".chars() {
        console::feed_char(c);
    }

    // echo + cat + shell = 3 завершения.
    while EXIT_CALLS.load(Ordering::SeqCst) < 3 {
        spins += 1;
        assert!(spins < 5_000_000_000, "pipeline + shell never all exited");
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

/// `echo piped | cat > f` пропустил данные через канал: файл содержит то, что напечатал echo.
#[test_case]
fn pipe_carries_output_between_two_programs() {
    let got = ferros::fs::open("/SUB/M7GPIPE.TXT").expect("/SUB/M7GPIPE.TXT was not created");
    assert_eq!(
        got, b"piped\n",
        "pipe did not carry echo's output into cat (pipe/EOF/dup2 broken)"
    );
}
