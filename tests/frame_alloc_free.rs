//! Интеграционный тест M6e1: фрейм-аллокатор умеет освобождать и переиспользовать фреймы.
//!
//! Раньше `BootInfoFrameAllocator` только нарезал фреймы курсором и никогда не отдавал их
//! обратно (всё текло). Теперь есть интрузивный список свободных: проверяем, что после
//! `deallocate_frame` фреймы возвращаются `allocate_frame` в порядке LIFO, а когда список
//! пуст — аллокатор снова нарезает свежие.
//!
//! Операции делаем в `main` (аллокатор — локальная переменная), результат — в статиках,
//! а `#[test_case]` их утверждает.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::{heap, paging};
use x86_64::structures::paging::{FrameAllocator, FrameDeallocator};
use x86_64::VirtAddr;

entry_point!(main);

/// Освобождённые фреймы вернулись в порядке LIFO.
static LIFO_OK: AtomicBool = AtomicBool::new(false);
/// После опустошения списка аллокатор выдал свежий (не из освобождённых) фрейм.
static FRESH_OK: AtomicBool = AtomicBool::new(false);

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init вызывается один раз. Заодно сохраняет
    // оффсет глобально — список свободных пишет ссылки по `phys + offset`.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut fa = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    heap::init_heap(&mut mapper, &mut fa).expect("heap init failed");

    // Берём три фрейма.
    let a = fa.allocate_frame().expect("frame a");
    let b = fa.allocate_frame().expect("frame b");
    let c = fa.allocate_frame().expect("frame c");

    // Освобождаем в порядке a, b, c → голова списка станет c → b → a.
    // SAFETY: a/b/c только что выданы этим аллокатором и больше нигде не используются.
    unsafe {
        fa.deallocate_frame(a);
        fa.deallocate_frame(b);
        fa.deallocate_frame(c);
    }

    // Переаллокация должна вернуть их в обратном порядке (LIFO): c, b, a.
    let x = fa.allocate_frame().expect("realloc 1");
    let y = fa.allocate_frame().expect("realloc 2");
    let z = fa.allocate_frame().expect("realloc 3");
    LIFO_OK.store(x == c && y == b && z == a, Ordering::SeqCst);

    // Список пуст — следующий фрейм должен быть свежим (не один из a/b/c).
    let w = fa.allocate_frame().expect("fresh frame");
    FRESH_OK.store(w != a && w != b && w != c, Ordering::SeqCst);

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// Освобождённые фреймы переиспользуются в порядке LIFO.
#[test_case]
fn freed_frames_return_lifo() {
    assert!(
        LIFO_OK.load(Ordering::SeqCst),
        "freed frames were not re-allocated in LIFO order"
    );
}

/// Когда список свободных пуст, аллокатор нарезает новый фрейм курсором.
#[test_case]
fn fresh_frame_after_free_list_drains() {
    assert!(
        FRESH_OK.load(Ordering::SeqCst),
        "allocator did not hand out a fresh frame after the free list drained"
    );
}
