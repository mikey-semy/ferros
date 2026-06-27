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
//! Эта реализация — простая (как в blog_os): храним курсор `next` и при каждом запросе
//! пересчитываем итератор свободных фреймов. **Освобождение** (M6e1, фаза зрелости): к
//! курсору добавлен интрузивный список свободных фреймов — `deallocate_frame` кладёт фрейм
//! в список, `allocate_frame` сперва берёт оттуда. «Интрузивный» = адрес следующего
//! свободного фрейма храним прямо в первых 8 байтах освобождённого фрейма (читаем/пишем по
//! `phys + phys_mem_offset`), поэтому списку не нужна куча. O(n) на bump-аллокацию и
//! отсутствие коалесинга/буддиси — это в HARDENING.md.

use bootloader::bootinfo::{MemoryMap, MemoryRegionType};
use x86_64::{
    structures::paging::{FrameAllocator, FrameDeallocator, PhysFrame, Size4KiB},
    PhysAddr,
};

/// Сентинел «конца списка» свободных фреймов: физический адрес 0 не бывает usable-фреймом
/// (нижняя память зарезервирована), поэтому 0 в поле-ссылке означает «дальше пусто».
const FREE_LIST_END: u64 = 0;

/// Выдаёт свободные физические фреймы, читая карту памяти от bootloader.
pub struct BootInfoFrameAllocator {
    memory_map: &'static MemoryMap,
    next: usize,
    /// Голова интрузивного списка освобождённых фреймов (LIFO). `None` — список пуст, тогда
    /// `allocate_frame` нарезает новый фрейм курсором `next`.
    free_list: Option<PhysAddr>,
}

/// Виртуальный указатель на первое слово фрейма `addr` (там храним ссылку «следующий
/// свободный»). Доступ — через отображение всей физпамяти (`virt = phys + offset`).
fn link_word(addr: PhysAddr) -> *mut u64 {
    (crate::mm::paging::phys_mem_offset() + addr.as_u64()).as_mut_ptr::<u64>()
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
            free_list: None,
        }
    }

    /// (Диагностика/тесты) Сколько фреймов выдано курсором (bump-аллокацией). Повторные
    /// выдачи из списка свободных сюда не входят.
    pub fn cursor(&self) -> usize {
        self.next
    }

    /// (Диагностика/тесты) Длина списка свободных фреймов (обходит его по ссылкам).
    pub fn free_list_len(&self) -> usize {
        let mut len = 0;
        let mut node = self.free_list;
        while let Some(addr) = node {
            len += 1;
            // SAFETY: `addr` попал в список через `deallocate_frame`; первое слово фрейма —
            // наша ссылка на следующий свободный.
            let next = unsafe { core::ptr::read(link_word(addr)) };
            node = (next != FREE_LIST_END).then(|| PhysAddr::new(next));
        }
        len
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

    /// Выделяет серию из `count` **физически идущих подряд** фреймов и возвращает первый.
    /// Нужно для DMA: virtqueue устройства virtio (M6b) должна лежать в непрерывной
    /// физической памяти и адресуется одним «номером страницы» (PFN = phys >> 12).
    ///
    /// Ищем первую серию из `count` соседних фреймов (адрес каждого = предыдущий + 4 КиБ)
    /// начиная с курсора. Фреймы до начала серии (если упёрлись в границу `Usable`-региона)
    /// пропускаются — они и так никогда не освобождаются (M3). `None`, если серии нет.
    ///
    /// Берёт фреймы ТОЛЬКО курсором (не из списка свободных): непрерывность из произвольного
    /// LIFO-списка не собрать. Поэтому непрерывные выделения (virtqueue virtio) никогда не
    /// возвращаются в оборот, а освобождённые одиночные фреймы не ломают будущую серию.
    pub fn allocate_contiguous(&mut self, count: usize) -> Option<PhysFrame> {
        if count == 0 {
            return None;
        }
        let mut run_start: Option<PhysFrame> = None;
        let mut run_offset = 0usize;
        let mut run_len = 0usize;
        let mut prev: Option<PhysFrame> = None;
        for (offset, frame) in self.usable_frames().skip(self.next).enumerate() {
            let contiguous = prev.is_some_and(|p| {
                frame.start_address().as_u64() == p.start_address().as_u64() + 4096
            });
            if contiguous {
                run_len += 1;
            } else {
                run_start = Some(frame);
                run_offset = offset;
                run_len = 1;
            }
            if run_len == count {
                // Серия [run_offset, run_offset+count) от курсора занята целиком.
                self.next += run_offset + count;
                return run_start;
            }
            prev = Some(frame);
        }
        None
    }
}

// SAFETY: фреймы берутся либо из списка освобождённых (туда они попадают только через
// `deallocate_frame`, т.е. больше никем не используются), либо из `Usable`-регионов по
// растущему курсору `next` — один и тот же фрейм не выдаётся дважды. Контракт
// `FrameAllocator` (только уникальные неиспользуемые фреймы) соблюдён.
unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        // Сперва переиспользуем освобождённый фрейм (LIFO): голова списка — это фрейм,
        // в первых 8 байтах которого лежит адрес следующего свободного.
        if let Some(head) = self.free_list {
            // SAFETY: `head` попал в список только через `deallocate_frame`, т.е. фрейм
            // свободен и его первое слово — наша ссылка, записанная при освобождении.
            let next = unsafe { core::ptr::read(link_word(head)) };
            self.free_list = (next != FREE_LIST_END).then(|| PhysAddr::new(next));
            return Some(PhysFrame::containing_address(head));
        }
        let frame = self.usable_frames().nth(self.next);
        self.next += 1;
        frame
    }
}

impl FrameDeallocator<Size4KiB> for BootInfoFrameAllocator {
    /// Возвращает фрейм в список свободных (LIFO): записываем текущую голову в первое слово
    /// фрейма и делаем его новой головой.
    ///
    /// # Safety
    /// `frame` должен быть выдан этим аллокатором и больше нигде не использоваться
    /// (не отображён, никем не читается/пишется) — иначе мы затрём чужие данные его первым
    /// словом, а позже выдадим занятый фрейм (UB). Непрерывные серии (`allocate_contiguous`)
    /// освобождать так нельзя — непрерывность не восстановится.
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame<Size4KiB>) {
        let addr = frame.start_address();
        // Фрейм 0 не бывает usable (нижняя память зарезервирована), поэтому никогда не
        // выдаётся и не освобождается; иначе его адрес столкнулся бы с сентинелом «конец
        // списка» (0) и фрейм потерялся бы из списка. Ловим нарушение в debug.
        debug_assert_ne!(
            addr.as_u64(),
            FREE_LIST_END,
            "frame 0 must never be freed (collides with the free-list sentinel)"
        );
        let prev_head = self.free_list.map_or(FREE_LIST_END, |p| p.as_u64());
        // SAFETY: фрейм только что освобождён вызывающим (его контракт) и отображён через
        // оффсет физпамяти — первое слово можно эксклюзивно записать.
        core::ptr::write(link_word(addr), prev_head);
        self.free_list = Some(addr);
    }
}
