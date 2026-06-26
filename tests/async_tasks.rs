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
use core::future::Future;
use core::panic::PanicInfo;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use core::task::{Context, Poll};
use ferros::drivers::keyboard;
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::{heap, paging};
use ferros::sched::executor::Executor;
use ferros::sched::simple_executor::SimpleExecutor;
use ferros::sched::Task;
use futures_util::stream::StreamExt;
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

/// `SimpleExecutor` (M4a) должен опросить задачу до `Poll::Ready` — `RESULT` станет 42.
#[test_case]
fn simple_executor_runs_task_to_completion() {
    RESULT.store(0, Ordering::SeqCst);
    let mut executor = SimpleExecutor::new();
    executor.spawn(Task::new(write_result()));
    executor.run();
    assert_eq!(RESULT.load(Ordering::SeqCst), 42);
}

/// Флаг, который задача выставляет после того, как один раз уступила управление.
static YIELDED_DONE: AtomicBool = AtomicBool::new(false);

/// Future, который на первом опросе возвращает `Pending` (разбудив себя через waker),
/// а на втором — `Ready`. Минимальный способ проверить весь путь пробуждения.
struct YieldOnce {
    yielded: bool,
}

impl Future for YieldOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.yielded {
            Poll::Ready(())
        } else {
            self.yielded = true;
            cx.waker().wake_by_ref(); // кладём свой id обратно в очередь готовых
            Poll::Pending
        }
    }
}

async fn yielding_task() {
    YieldOnce { yielded: false }.await;
    YIELDED_DONE.store(true, Ordering::SeqCst);
}

/// Эффективный `Executor` (M4b) должен довести до конца задачу, которая один раз
/// уступила управление: её waker возвращает id в очередь, и повторный опрос даёт
/// `Ready`. Одного прохода `run_ready_tasks` достаточно — self-wake кладёт id обратно
/// в ту же очередь, которую проход и опустошает.
#[test_case]
fn executor_runs_yielding_task() {
    YIELDED_DONE.store(false, Ordering::SeqCst);
    let mut executor = Executor::new();
    executor.spawn(Task::new(yielding_task()));
    executor.run_ready_tasks();
    assert!(YIELDED_DONE.load(Ordering::SeqCst));
}

/// Куда задача-читатель кладёт полученный скан-код (`0xffff_ffff` = «ещё не пришёл»).
static GOT_SCANCODE: AtomicU32 = AtomicU32::new(0xffff_ffff);

async fn read_one_scancode() {
    let mut scancodes = keyboard::ScancodeStream::new();
    if let Some(scancode) = scancodes.next().await {
        GOT_SCANCODE.store(scancode as u32, Ordering::SeqCst);
    }
}

/// Полный путь async-клавиатуры (M4c): задача ждёт на пустом потоке (Pending +
/// регистрирует waker); затем `add_scancode` — как из прерывания — кладёт байт и
/// будит задачу; следующий проход доводит её до полученного скан-кода.
#[test_case]
fn keyboard_stream_wakes_on_scancode() {
    keyboard::init();
    GOT_SCANCODE.store(0xffff_ffff, Ordering::SeqCst);

    let mut executor = Executor::new();
    executor.spawn(Task::new(read_one_scancode()));

    // Первый проход: очередь пуста → задача уходит в Pending и регистрирует waker.
    executor.run_ready_tasks();
    assert_eq!(GOT_SCANCODE.load(Ordering::SeqCst), 0xffff_ffff);

    // «Прерывание»: кладём скан-код и будим задачу (waker возвращает её в очередь).
    keyboard::add_scancode(0x1E); // 'a' в scancode set 1

    // Второй проход: задача просыпается, читает байт и записывает его.
    executor.run_ready_tasks();
    assert_eq!(GOT_SCANCODE.load(Ordering::SeqCst), 0x1E);
}
