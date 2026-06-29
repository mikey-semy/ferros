//! Интеграционный тест M9h: программа на C со стандартным `int main()` поверх минимальной libc.
//!
//! Спавним `CDEMO_ELF` — C-программу, линкованную с нашими crt0 + libc. Она выделяет 8 КиБ через
//! `malloc` (это заставляет libc вырастить `brk`), заполняет/суммирует буфер, пишет строку на fd 1
//! и возвращает 0 только при совпадении контрольной суммы и `argc == 0`. Проверяем код выхода 0 и
//! записанную строку.
//!
//! Почему это доказательство: код выхода 0 означает, что отработала ВСЯ цепочка libc — crt0 позвал
//! `main` с верным argc, `malloc`/`brk` дали пригодную на запись кучу (>страницы → был рост),
//! `write` сработал, а возврат `main` стал кодом выхода. То есть на ferros запускается обычная
//! C-программа с `int main()` и кучей, а не только ручные asm-обёртки.

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
use ferros::sched::thread;
use ferros::syscall::elf::CDEMO_ELF;
use ferros::syscall::{EXIT_CALLS, LAST_EXIT_CODE, LAST_WRITE_FD, LAST_WRITE_LEN, LAST_WRITE_SUM};
use x86_64::VirtAddr;

/// Та же строка, что печатает `user/c/demo.c` (разные крейты — общую константу не пошарить).
const EXPECTED: &[u8] = b"C libc demo: malloc OK\n";

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init один раз.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");
    console::init(); // demo пишет на fd 1 (VGA)

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята. malloc demo использует brk → глобальный
    // фрейм-аллокатор; ставим его ДО старта процесса.
    unsafe { spawn_user(CDEMO_ELF, phys_mem_offset, &mut frame_allocator) };
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "C libc demo never exited");
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

/// `cdemo` вышла с 0 (вся libc-цепочка отработала) и записала ожидаемую строку на fd 1.
#[test_case]
fn c_program_with_main_and_malloc() {
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "C libc demo failed (1=malloc null, 2=checksum/argc mismatch) — crt0/malloc/exit chain"
    );
    assert_eq!(
        LAST_WRITE_FD.load(Ordering::SeqCst),
        1,
        "write was not to fd 1"
    );
    assert_eq!(
        LAST_WRITE_LEN.load(Ordering::SeqCst),
        EXPECTED.len() as u64,
        "demo wrote the wrong number of bytes"
    );
    let expected_sum: u64 = EXPECTED.iter().map(|&b| b as u64).sum();
    assert_eq!(
        LAST_WRITE_SUM.load(Ordering::SeqCst),
        expected_sum,
        "demo wrote the wrong bytes"
    );
}
