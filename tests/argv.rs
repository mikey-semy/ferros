//! Интеграционный тест M7b: `execve` передаёт argv новому образу через начальный стек.
//!
//! Спавним `execargv`: он `execve("ARGVECHO", ["ARGVECHO","ping","pong"], [])`. Это заменяет его
//! программой `ARGVECHO` с диска (FAT), которой ядро строит System V-стек с argc/argv. `ARGVECHO`
//! читает argv со стека и завершается с 0, только если получила ровно `["ARGVECHO","ping","pong"]`
//! (иначе отличимый код 10–13). Если бы `execve` не сработал, `execargv` вышел бы с 42.
//!
//! Почему это доказательство argv: код выхода 0 означает, что новый образ увидел argc==3 и три
//! верные строки — то есть `execve` скопировал argv из старого пространства и разложил их на стеке
//! нового по ABI, а `_start` нашёл их по `rsp`.

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
use ferros::mm::frame::{self, BootInfoFrameAllocator};
use ferros::mm::{heap, paging};
use ferros::sched::thread;
use ferros::syscall::elf::EXECARGV_ELF;
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

    // execve читает ARGVECHO с диска → нужен virtio-blk.
    assert!(
        ferros::drivers::virtio_blk::init(phys_mem_offset, &mut frame_allocator),
        "virtio-blk not initialized"
    );

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(EXECARGV_ELF, phys_mem_offset, &mut frame_allocator) };

    // execve строит новое адресное пространство и освобождает старое → нужен глобальный аллокатор.
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) == 0 {
        spins += 1;
        assert!(spins < 5_000_000_000, "execargv/argvecho never exited");
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

/// После `execve` процесс стал `ARGVECHO` и увидел верные argv: вышел с 0 (не 42 и не 10–13).
#[test_case]
fn execve_passes_argv_on_the_stack() {
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "argvecho did not receive [ARGVECHO, ping, pong] (42=execve failed, 10-13=wrong argv)"
    );
}
