//! Куча ядра (M3c): динамическая память для крейта `alloc` — `Box`/`Vec`/`String`.
//!
//! # Что такое куча и зачем
//!
//! До сих пор вся память ядра была статической (`static`) или на стеке: размеры
//! известны на этапе компиляции. Но списки, строки, деревья растут во время работы —
//! им нужна **динамическая** память, которую можно просить и возвращать кусками
//! произвольного размера. Этим заведует **аллокатор кучи**.
//!
//! В Rust это оформлено через `#[global_allocator]`: статический объект, реализующий
//! трейт `GlobalAlloc`. Как только он есть, компилятор подключает крейт `alloc`, и
//! `Box`, `Vec`, `String`, `BTreeMap` и прочее начинают работать «как в обычном Rust».
//!
//! # Два шага инициализации
//!
//! 1. **Замапить регион под кучу.** Выбираем диапазон виртуальных адресов
//!    `[HEAP_START, HEAP_START + HEAP_SIZE)` и отображаем каждую его страницу на
//!    свободный физический фрейм (через [`super::paging`]/[`super::frame`] из M3a/M3b).
//! 2. **Отдать регион аллокатору.** Говорим [`LockedHeap`], что вот этот кусок
//!    виртуальной памяти теперь его — он будет нарезать из него `Box`/`Vec`/…
//!
//! [`linked_list_allocator`] хранит свободные блоки в связном списке (отсюда имя).
//! Аллокация — это поиск подходящей «дырки», O(n) по списку, плюс фрагментация. Для
//! bring-up достаточно; быстрый fixed-size-block аллокатор — в `docs/HARDENING.md`.

use linked_list_allocator::LockedHeap;
use x86_64::{
    structures::paging::{
        mapper::MapToError, FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB,
    },
    VirtAddr,
};

/// Виртуальный адрес начала кучи. Произвольный свободный регион, не пересекающийся
/// с отображением физпамяти и с демо-страницей M3b.
pub const HEAP_START: usize = 0x_5555_5555_0000;
/// Размер кучи — 100 КиБ. Хватает на bring-up и тесты; расширим, когда понадобится.
pub const HEAP_SIZE: usize = 100 * 1024;

/// Глобальный аллокатор ядра. `empty()` — `const`, поэтому это обычный `static`;
/// реальную память выдаём ему позже в [`init_heap`].
#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Отображает регион кучи и передаёт его глобальному аллокатору.
///
/// Вызывать один раз при старте (после инициализации `mapper` и `frame_allocator`).
/// Каждую страницу кучи мапим на свежий физический фрейм с правами `PRESENT |
/// WRITABLE`; затем сообщаем аллокатору границы региона.
pub fn init_heap(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), MapToError<Size4KiB>> {
    // Диапазон страниц кучи (включительно по последней).
    let page_range = {
        let heap_start = VirtAddr::new(HEAP_START as u64);
        let heap_end = heap_start + HEAP_SIZE as u64 - 1u64;
        let heap_start_page = Page::containing_address(heap_start);
        let heap_end_page = Page::containing_address(heap_end);
        Page::range_inclusive(heap_start_page, heap_end_page)
    };

    for page in page_range {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        // SAFETY: `frame` свежевыделен аллокатором (уникальный неиспользуемый), а
        // `page` лежит в выбранном под кучу регионе и нигде больше не маппится.
        unsafe { mapper.map_to(page, frame, flags, frame_allocator)?.flush() };
    }

    // SAFETY: регион [HEAP_START, HEAP_START+HEAP_SIZE) только что целиком отображён
    // на физпамять с правом записи и больше никем не используется.
    unsafe {
        ALLOCATOR.lock().init(HEAP_START as *mut u8, HEAP_SIZE);
    }

    Ok(())
}
