//! Интеграционный тест M9a: куча процесса через `brk` из кольца 3.
//!
//! Спавним `brktest`: он запрашивает текущий разрыв (`brk(0)`), растит кучу на страницы, пишет и
//! читает их, сжимает (страницы снимаются и их фреймы возвращаются), растит снова (страницы
//! переотображаются) и пишет/читает заново. Выходит с 0 при успехе, иначе с отличимым кодом
//! (10..19 — какой шаг сломался).
//!
//! Почему это доказательство: записанные и прочитанные байты в выросшей области означают, что
//! `brk` реально отобразил физические страницы как пользовательские (USER+WRITABLE) в активном
//! пространстве; успешный повторный рост после сжатия — что снятые страницы корректно
//! освободились и переотобразились. Глобальный фрейм-аллокатор ставится ДО снятия preemption,
//! поэтому к моменту работы `brktest` ему есть откуда брать/куда возвращать фреймы кучи.

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
use ferros::syscall::elf::BRKTEST_ELF;
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

    thread::init();
    // SAFETY: phys_offset корректен, куча поднята.
    unsafe { spawn_user(BRKTEST_ELF, phys_mem_offset, &mut frame_allocator) };
    // Глобальный аллокатор — ДО старта процесса: `brk` берёт/возвращает фреймы кучи через него.
    frame::install(frame_allocator);

    thread::start_preemption();
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) < 1 {
        spins += 1;
        assert!(spins < 5_000_000_000, "brktest never exited");
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

/// `brktest` завершился с 0: рост, запись/чтение, сжатие и повторный рост кучи через `brk` отработали.
#[test_case]
fn brk_grows_writes_shrinks_and_regrows_the_heap() {
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "brktest failed (10/11 brk(0); 12/13 grow+rw; 20 grow-zeroed; 14/15 shrink; 16 to-base; 17/18 regrow; 21 regrow-zeroed; 19 brk(0))"
    );
}
