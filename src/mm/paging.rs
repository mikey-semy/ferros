//! Пейджинг: доступ к таблицам страниц и трансляция адресов (M3a).
//!
//! # «Бумажная» теория пейджинга за пять минут
//!
//! В 64-битном режиме CPU обращается к памяти по **виртуальным** адресам. Каждое
//! обращение «на лету» переводится (транслируется) в **физический** адрес блоком
//! MMU внутри процессора. Правила перевода задаёт дерево **таблиц страниц** —
//! на x86_64 четыре уровня: L4 → L3 → L2 → L1. Виртуальный адрес режется на четыре
//! 9-битных индекса (по одному на уровень) плюс 12 бит смещения внутри страницы:
//!
//! ```text
//!   63        48 47   39 38   30 29   21 20   12 11         0
//!  | sign-ext  | L4 idx | L3 idx | L2 idx | L1 idx | page offset |
//! ```
//!
//! Адрес активной L4-таблицы лежит в регистре **CR3**. Но CR3 и записи таблиц
//! хранят *физические* адреса, а читать их ядру нужно по *виртуальным* — замкнутый
//! круг. Его разрывает bootloader: с фичей `map_physical_memory` он отображает всю
//! физическую память по фиксированному оффсету, поэтому
//!
//! ```text
//!   виртуальный_адрес = физический_адрес + physical_memory_offset
//! ```
//!
//! Этого достаточно, чтобы пройти всё дерево таблиц самим. Готовую реализацию
//! такого «оффсетного» перевода даёт [`OffsetPageTable`] из крейта `x86_64` —
//! её и возвращает [`init`].

use x86_64::{
    registers::control::Cr3,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame,
        Size4KiB,
    },
    PhysAddr, VirtAddr,
};

/// Инициализирует [`OffsetPageTable`] над активной иерархией таблиц.
///
/// `physical_memory_offset` — это `BootInfo::physical_memory_offset`, обёрнутый в
/// [`VirtAddr`]. Возвращённый маппер живёт `'static`: он держит `&mut` на активную
/// L4-таблицу, существующую всё время работы ядра.
///
/// # Safety
///
/// Вызывать **ровно один раз**. `physical_memory_offset` должен реально указывать
/// на начало отображения всей физической памяти (его гарантирует bootloader с
/// фичей `map_physical_memory`). Два живых `&mut` на одну и ту же таблицу нарушили
/// бы алиасинг-инварианты Rust и могли бы повредить память.
pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    let level_4_table = active_level_4_table(physical_memory_offset);
    OffsetPageTable::new(level_4_table, physical_memory_offset)
}

/// Возвращает `&mut` на таблицу страниц во фрейме `frame`, доступную по оффсету
/// отображения физпамяти (`virt = phys + physical_memory_offset`). Единая точка доступа
/// к таблицам через оффсет — её переиспользуют [`active_level_4_table`] и адресные
/// пространства процессов ([`crate::mm::addr_space`]).
///
/// # Safety
///
/// `frame` должен указывать на настоящую таблицу страниц, `physical_memory_offset` —
/// корректный оффсет всей физпамяти. Нельзя держать две `&mut`-ссылки на ОДИН фрейм
/// одновременно (это были бы два `&mut` на одну таблицу).
pub(crate) unsafe fn page_table_at(
    frame: PhysFrame,
    physical_memory_offset: VirtAddr,
) -> &'static mut PageTable {
    let virt = physical_memory_offset + frame.start_address().as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();
    &mut *page_table_ptr
}

/// Возвращает `&mut` на активную таблицу 4-го уровня (её фрейм — в CR3).
///
/// # Safety
///
/// См. [`init`]: оффсет должен быть корректным, и одновременно может существовать
/// только одна `&mut`-ссылка на эту таблицу.
unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    // CR3 хранит физический фрейм активной L4-таблицы (флаги нам тут не нужны).
    let (level_4_table_frame, _) = Cr3::read();
    page_table_at(level_4_table_frame, physical_memory_offset)
}

/// Демонстрация M3b: создаёт новый маппинг — отображает виртуальную страницу `page`
/// на физический фрейм VGA-буфера (`0xb8000`). После этого запись по любому адресу
/// внутри `page` попадёт прямо в видеопамять — наглядно доказывает, что мы умеем
/// сами менять таблицы страниц.
///
/// `map_to` при необходимости выделяет до трёх фреймов под промежуточные таблицы —
/// поэтому ей нужен `frame_allocator`. Возвращённый «флаг» сбрасываем (`flush`),
/// чтобы CPU перечитал маппинг из таблиц, а не из TLB-кеша.
///
/// Только для демонстрации: мапить произвольную страницу на VGA — не то, что делают
/// в проде, поэтому функция помечена `example`.
pub fn create_example_mapping(
    page: Page,
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    let frame = PhysFrame::containing_address(PhysAddr::new(0xb8000));
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

    // SAFETY: фрейм 0xb8000 — это существующий VGA-буфер, отобразить его безопасно.
    // В общем случае `map_to` небезопасна: можно создать алиас или занять уже
    // используемый фрейм. Здесь это осознанная единичная демонстрация.
    let map_to_result = unsafe { mapper.map_to(page, frame, flags, frame_allocator) };
    map_to_result.expect("map_to failed").flush();
}

/// Отображает `page` на свежий физический фрейм как **пользовательскую** (`USER_ACCESSIBLE`)
/// страницу, доступную на чтение/запись (M5). Флаг `USER_ACCESSIBLE` — это и есть та
/// граница, что отделяет память ядра от памяти кольца 3: без него обращение из кольца 3
/// вызвало бы page fault. Используется для кода/стека пользователя.
///
/// Пока без `NO_EXECUTE` и без W^X (страница и пишется, и исполняется) — это в HARDENING.
///
/// # Panics
/// Если фреймов нет или `page` уже отображена (`map_to` вернёт ошибку).
pub fn map_user_page(
    page: Page,
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    let frame = frame_allocator
        .allocate_frame()
        .expect("out of frames for user page");
    let flags =
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;

    // Промежуточные таблицы (L3/L2/L1) ТОЖЕ должны быть USER_ACCESSIBLE, иначе CPU
    // посчитает весь перевод супервизорным и обращение из кольца 3 упадёт в page fault.
    // `map_to` по умолчанию ставит на родительские записи `PRESENT|WRITABLE|USER_ACCESSIBLE`,
    // так что нам достаточно указать флаг на листовой записи.
    //
    // SAFETY: `frame` только что выдан аллокатором (никем не используется), поэтому алиас
    // не создаётся; `page` выбирается из свободной нижней половины адресного пространства.
    let result = unsafe { mapper.map_to(page, frame, flags, frame_allocator) };
    result.expect("map_to (user) failed").flush();
}
