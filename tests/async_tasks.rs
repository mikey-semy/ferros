//! Интеграционный тест кооперативного экзекьютора (M4a): задача реально исполняется
//! до конца. Нужна инициализированная куча (`Task` размещается в `Box`), поэтому тест
//! поднимает пейджинг + аллокатор фреймов + кучу — как `heap_allocation`.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU32, Ordering};
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::{heap, paging};
use ferros::sched::simple_executor::SimpleExecutor;
use ferros::sched::Task;
use x86_64::VirtAddr;

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init вызывается один раз.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// Побочный эффект задачи — становится виден только если экзекьютор довёл её до конца.
static RESULT: AtomicU32 = AtomicU32::new(0);

async fn write_result() {
    RESULT.store(42, Ordering::SeqCst);
}

/// Экзекьютор должен опросить задачу до `Poll::Ready` — тогда `RESULT` станет 42.
#[test_case]
fn executor_runs_task_to_completion() {
    RESULT.store(0, Ordering::SeqCst);
    let mut executor = SimpleExecutor::new();
    executor.spawn(Task::new(write_result()));
    executor.run();
    assert_eq!(RESULT.load(Ordering::SeqCst), 42);
}
