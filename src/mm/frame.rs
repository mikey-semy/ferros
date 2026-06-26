//! Аллокатор физических фреймов из карты памяти bootloader (M3b).
//!
//! # Зачем нужен аллокатор фреймов
//!
//! «Фрейм» (frame) — это физическая страница 4 КиБ: единица, которой оперирует
//! железо при пейджинге. Когда мы создаём новый маппинг ([`super::paging`]),
//! процессору может не хватить промежуточных таблиц страниц (L3/L2/L1) — их надо
//! куда-то положить, то есть выделить под них **свободные физические фреймы**.
//! Кто-то должен знать, какие фреймы свободны. Это и есть аллокатор фреймов.
//!
//! # Откуда берём список свободных фреймов
//!
//! При загрузке BIOS/bootloader составляет **карту памяти** — список регионов
//! физической памяти с их назначением (свободно, занято ядром, зарезервировано
//! железом, ACPI и т.д.). Bootloader передаёт её в `BootInfo::memory_map`. Нам
//! нужны только регионы типа [`MemoryRegionType::Usable`] — из них и нарезаем
//! фреймы по 4 КиБ.
//!
//! Эта реализация — простейшая (как в blog_os): храним индекс `next` и при каждом
//! запросе пересчитываем итератор свободных фреймов и берём `next`-й. Это O(n) на
//! аллокацию и фреймы не возвращаются обратно — для bring-up M3 нормально; реальный
//! аллокатор (bitmap/buddy) и освобождение придут позже (см. `docs/HARDENING.md`).

use bootloader::bootinfo::{MemoryMap, MemoryRegionType};
use x86_64::{
    structures::paging::{FrameAllocator, PhysFrame, Size4KiB},
    PhysAddr,
};

/// Выдаёт свободные физические фреймы, читая карту памяти от bootloader.
pub struct BootInfoFrameAllocator {
    memory_map: &'static MemoryMap,
    next: usize,
}

impl BootInfoFrameAllocator {
    /// Создаёт аллокатор поверх переданной карты памяти.
    ///
    /// # Safety
    ///
    /// Вызывающий гарантирует, что карта памяти валидна, а её `Usable`-регионы
    /// действительно свободны (фреймы из них ещё никем не используются). Иначе
    /// аллокатор может выдать занятый фрейм — это нарушит контракт
    /// [`FrameAllocator`] и приведёт к UB.
    pub unsafe fn init(memory_map: &'static MemoryMap) -> Self {
        BootInfoFrameAllocator {
            memory_map,
            next: 0,
        }
    }

    /// Итератор по всем свободным фреймам из карты памяти.
    ///
    /// Цепочка: регионы → только `Usable` → диапазоны физ. адресов → шаг 4 КиБ →
    /// [`PhysFrame`] по каждому стартовому адресу.
    fn usable_frames(&self) -> impl Iterator<Item = PhysFrame> {
        let regions = self.memory_map.iter();
        let usable = regions.filter(|r| r.region_type == MemoryRegionType::Usable);
        let addr_ranges = usable.map(|r| r.range.start_addr()..r.range.end_addr());
        let frame_addresses = addr_ranges.flat_map(|r| r.step_by(4096));
        frame_addresses.map(|addr| PhysFrame::containing_address(PhysAddr::new(addr)))
    }
}

// SAFETY: `usable_frames` берёт фреймы только из `Usable`-регионов карты памяти, а
// растущий `next` гарантирует, что один и тот же фрейм не выдаётся дважды — то есть
// контракт `FrameAllocator` (только уникальные неиспользуемые фреймы) соблюдён.
unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        let frame = self.usable_frames().nth(self.next);
        self.next += 1;
        frame
    }
}
