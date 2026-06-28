//! Интеграционный тест M7d: интерактивный shell читает команды, запускает программы, выходит.
//!
//! Спавним `shell` (PID 1) и «набираем» ему две строки: `ARGVECHO ping pong` и `exit 0`. Shell
//! должен разобрать первую строку на слова, `fork`+`execve("ARGVECHO", ["ARGVECHO","ping","pong"])`
//! (с диска), дождаться ребёнка через `wait4`, затем по второй строке выполнить встроенный `exit 0`.
//!
//! Почему это доказательство: `ARGVECHO` выходит с 0 ТОЛЬКО если получил ровно эти три аргумента
//! (иначе 10–13), а провал `execve` дал бы 127. Значит:
//! - `EXIT_CALLS == 2` — ребёнок и сам shell завершились (shell реально форкнул и запустил программу);
//! - `EXIT_CODE_SUM == 0` — оба вышли с 0, т.е. shell верно разбил строку на argv и `execve` донёс
//!   их до программы (любой сбой дал бы ненулевой вклад в сумму);
//! - `LAST_EXIT_CODE == 0` — последним вышел shell по `exit 0`.
//! Ввод подаём программно (`console::feed_char`, как клавиатура) — тест headless-детерминирован.

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
use ferros::syscall::{EXIT_CALLS, EXIT_CODE_SUM, LAST_EXIT_CODE};
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

    console::init(); // stdin для shell
                     // shell запускает ARGVECHO с диска → нужен virtio-blk.
    assert!(
        ferros::drivers::virtio_blk::init(phys_mem_offset, &mut frame_allocator),
        "virtio-blk not initialized"
    );

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(SHELL_ELF, phys_mem_offset, &mut frame_allocator) };
    // shell форкается и строит адресные пространства детей → нужен глобальный аллокатор.
    frame::install(frame_allocator);

    thread::start_preemption();

    // 1) Ждём, пока shell заблокируется на первом `read(0)` (приглашение напечатано, ввода нет).
    let mut spins = 0u64;
    while STDIN_BLOCKS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "shell never blocked on stdin");
        core::hint::spin_loop();
    }

    // 2) «Набираем» команду с аргументами, затем выход.
    for c in "ARGVECHO ping pong\nexit 0\n".chars() {
        console::feed_char(c);
    }

    // 3) Ждём завершения ребёнка (ARGVECHO) и самого shell.
    while EXIT_CALLS.load(Ordering::SeqCst) < 2 {
        spins += 1;
        assert!(spins < 5_000_000_000, "shell + child never both exited");
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

/// shell запустил программу с верными argv и корректно завершился: ровно два выхода, оба с 0.
#[test_case]
fn shell_runs_a_command_with_args_then_exits() {
    assert_eq!(
        EXIT_CALLS.load(Ordering::SeqCst),
        2,
        "expected the child (ARGVECHO) and the shell to each exit once"
    );
    assert_eq!(
        EXIT_CODE_SUM.load(Ordering::SeqCst),
        0,
        "non-zero exit sum: shell mis-parsed argv or execve failed (127) / wrong args (10-13)"
    );
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "shell did not exit 0 via the `exit 0` builtin"
    );
}
