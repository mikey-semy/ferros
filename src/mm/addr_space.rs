//! Адресные пространства процессов (M5c2): у каждого процесса — свой корень таблиц
//! страниц (PML4), но память ядра в нём общая.
//!
//! # Идея
//!
//! До сих пор всё (ядро + пользовательская программа) жило в одной таблице страниц.
//! Чтобы изолировать процессы друг от друга, каждому нужен **свой PML4**. При этом ядро
//! обязано оставаться отображённым в каждом таком пространстве — иначе после переключения
//! `CR3` первое же прерывание/`syscall` (код ядра!) упало бы. Решение классическое:
//! **верхние уровни ядра — общие, пользовательская часть — приватная**.
//!
//! Конкретно [`AddressSpace::new_sharing_kernel`] выделяет фрейм под новый PML4 и
//! **копирует в него все 512 L4-записей активной таблицы**. Записи указывают на те же
//! нижележащие таблицы ядра — значит всё, что отображено у ядра (код, куча, физпамять,
//! VGA), доступно и здесь. Пользовательские страницы потом маппятся в **пустой** L4-слот
//! (его у ядра нет), поэтому создаётся приватное поддерево — изоляция «из коробки».
//!
//! # Bootloader 0.9 и низкое ядро
//!
//! У нас ядро живёт в нижней половине (физпамять на L4-индексе 3, куча на 170 и т.п.),
//! поэтому под пользователя берём заведомо свободный высокий слот нижней половины
//! (см. `arch::x86_64::syscall`). После миграции на higher-half (M11) пользователю
//! достанется вся нижняя половина — но сам этот механизм (новый PML4 + копия L4 ядра)
//! не изменится.
//!
//! Тейкдаун ([`AddressSpace::destroy`], M6e2) освобождает приватное поддерево процесса и его
//! PML4, не трогая общие с ядром записи — это даёт reaper'у (M6e3) вернуть память процесса.
//!
//! # Слой (D7)
//!
//! `CR3` и таблицы страниц — арх-специфика x86_64; формально этому модулю место за
//! `arch`-швом. Пока он живёт в `mm` — как и существующий [`crate::mm::paging`], который
//! уже напрямую трогает `Cr3`/`OffsetPageTable`. Полная арх-абстракция слоя таблиц —
//! отдельный заход (см. `docs/HARDENING.md`).

use crate::mm::paging::page_table_at;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{
    FrameAllocator, FrameDeallocator, OffsetPageTable, PageTableFlags, PhysFrame, Size4KiB,
};
use x86_64::VirtAddr;

/// Адресное пространство процесса: владеет своим корнем таблиц страниц (PML4).
pub struct AddressSpace {
    /// Физический фрейм PML4 этого пространства.
    pml4_frame: PhysFrame,
}

impl AddressSpace {
    /// Создаёт новое адресное пространство, в котором **вся память ядра общая**: выделяет
    /// фрейм под PML4 и копирует в него все L4-записи **ядровой** таблицы `kernel_pml4`.
    /// Пользовательская (пустая у ядра) часть остаётся свободной под приватные маппинги.
    ///
    /// Важно копировать именно ЯДРОВУЮ таблицу, а не активную: если вызвать из контекста
    /// пользовательского процесса (например, `execve`), активная таблица содержала бы и его
    /// пользовательский слот — он бы протёк в новое пространство и столкнулся бы с загрузкой.
    ///
    /// # Safety
    /// `phys_offset` — корректный оффсет физпамяти; `kernel_pml4` — настоящий корень таблиц
    /// ядра. Возвращённое пространство держит память таблиц ядra общей — нельзя освобождать
    /// нижележащие таблицы ядра, пока живы такие пространства.
    pub unsafe fn new_sharing_kernel(
        phys_offset: VirtAddr,
        kernel_pml4: PhysFrame,
        frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    ) -> AddressSpace {
        let frame = frame_allocator
            .allocate_frame()
            .expect("out of frames for a process PML4");

        // SAFETY: фрейм только что выделен (уникальный) и доступен по phys_offset.
        let new_pml4 = unsafe { page_table_at(frame, phys_offset) };
        new_pml4.zero();

        // Копируем все 512 L4-записей ЯДРОВОЙ таблицы → память ядра общая. `new` и `kernel` —
        // разные фреймы, поэтому два `&mut` на РАЗНЫЕ таблицы не алиасят.
        // SAFETY: kernel_pml4 — настоящая таблица, доступен по phys_offset; другой фрейм.
        let kernel = unsafe { page_table_at(kernel_pml4, phys_offset) };
        for i in 0..512 {
            new_pml4[i] = kernel[i].clone();
        }

        AddressSpace { pml4_frame: frame }
    }

    /// Строит [`OffsetPageTable`] над PML4 этого пространства — чтобы маппить в него
    /// страницы, даже пока оно **не активно**. (`map_to` пишет записи таблиц через
    /// `phys_offset`, поэтому активность не требуется; но запись *содержимого* по
    /// пользовательскому адресу — уже да, требует активного `CR3`.)
    ///
    /// # Safety
    /// `phys_offset` — корректный оффсет физпамяти. Нельзя держать два таких маппера на
    /// одно пространство одновременно (это были бы два `&mut` на одну таблицу).
    pub unsafe fn mapper(&self, phys_offset: VirtAddr) -> OffsetPageTable<'static> {
        // SAFETY: PML4 этого пространства — настоящая таблица, доступен по phys_offset;
        // вызывающий гарантирует отсутствие второго живого маппера на него.
        unsafe { OffsetPageTable::new(page_table_at(self.pml4_frame, phys_offset), phys_offset) }
    }

    /// Физический фрейм PML4 (для записи в `CR3` при активации пространства).
    pub fn pml4_frame(&self) -> PhysFrame {
        self.pml4_frame
    }

    /// Заворачивает уже существующий фрейм PML4 обратно в [`AddressSpace`] — для тейкдауна
    /// (M6e2): `spawn_user` роняет `AddressSpace` после создания, в потоке остаётся лишь
    /// `cr3`-фрейм; reaper (M6e3) пересобирает из него пространство, чтобы освободить.
    ///
    /// # Safety
    /// `frame` должен быть PML4, созданным [`Self::new_sharing_kernel`], и больше не должен
    /// быть активным в `CR3`.
    pub unsafe fn from_pml4_frame(frame: PhysFrame) -> AddressSpace {
        AddressSpace { pml4_frame: frame }
    }

    /// Освобождает **только приватное (пользовательское) поддерево** этого пространства и сам
    /// фрейм PML4, возвращая фреймы аллокатору. Общие с ядром L4-записи (скопированные при
    /// создании) НЕ трогает — иначе повредили бы ядро и другие процессы.
    ///
    /// Пользовательскими считаем L4-записи, которые присутствуют И отличаются от
    /// соответствующей записи активной (ядровой) таблицы (у нас это слот 255). Обход —
    /// снизу вверх: листовые фреймы → L1 → L2 → L3, затем сам PML4.
    ///
    /// # Safety
    /// Это пространство НЕ должно быть активным (`CR3`) и на него не должно быть живого
    /// [`Self::mapper`]. `phys_offset` — корректный оффсет физпамяти.
    pub unsafe fn destroy(self, phys_offset: VirtAddr, fa: &mut impl FrameDeallocator<Size4KiB>) {
        let (kernel_frame, _) = Cr3::read();
        debug_assert_ne!(
            self.pml4_frame, kernel_frame,
            "must not destroy the active address space"
        );

        {
            // SAFETY: оба — настоящие таблицы (разные фреймы), доступны по phys_offset; это
            // пространство неактивно и без живого маппера, поэтому единственные ссылки — наши.
            let pml4 = unsafe { page_table_at(self.pml4_frame, phys_offset) };
            let kernel_pml4 = unsafe { page_table_at(kernel_frame, phys_offset) };
            for i in 0..512 {
                if !pml4[i].flags().contains(PageTableFlags::PRESENT) {
                    continue;
                }
                // Запись, общая с ядром (тот же дочерний фрейм), — не наша, пропускаем.
                let shared = kernel_pml4[i].flags().contains(PageTableFlags::PRESENT)
                    && kernel_pml4[i].addr() == pml4[i].addr();
                if shared {
                    continue;
                }
                // Приватное поддерево (L3) — освобождаем целиком.
                if let Ok(l3) = pml4[i].frame() {
                    // SAFETY: l3 — наша приватная таблица уровня 3; не активна.
                    unsafe { free_subtree(l3, 3, phys_offset, fa) };
                }
            }
        } // снимаем &mut на pml4/kernel_pml4 ДО освобождения фрейма PML4

        // SAFETY: поддеревья освобождены; PML4 неактивен и больше не нужен.
        unsafe { fa.deallocate_frame(self.pml4_frame) };
    }
}

/// Рекурсивно освобождает поддерево таблиц, начиная с `frame` (уровень `level`: 3 = L3,
/// 2 = L2, 1 = L1), снизу вверх: сначала все дочерние записи, потом сам фрейм таблицы.
///
/// # Safety
/// `frame` — приватная таблица уровня `level` неактивного пространства; доступна по
/// `phys_offset`; никто другой её не держит.
unsafe fn free_subtree(
    frame: PhysFrame,
    level: u8,
    phys_offset: VirtAddr,
    fa: &mut impl FrameDeallocator<Size4KiB>,
) {
    {
        // SAFETY: frame — настоящая таблица, доступна по phys_offset; единственная ссылка.
        let table = unsafe { page_table_at(frame, phys_offset) };
        for i in 0..512 {
            let entry = &table[i];
            if !entry.flags().contains(PageTableFlags::PRESENT) {
                continue;
            }
            if level == 1 {
                // Листовая запись — это пользовательский фрейм данных/кода/стека.
                debug_assert!(
                    entry.flags().contains(PageTableFlags::USER_ACCESSIBLE),
                    "freeing a non-user leaf during teardown"
                );
                if let Ok(leaf) = entry.frame() {
                    // SAFETY: лист приватного поддерева неактивного пространства — свободен.
                    unsafe { fa.deallocate_frame(leaf) };
                }
            } else {
                // У нас только страницы 4 КиБ; huge-page на L2/L3 не ожидаются.
                debug_assert!(
                    !entry.flags().contains(PageTableFlags::HUGE_PAGE),
                    "huge pages are not supported in teardown"
                );
                if let Ok(child) = entry.frame() {
                    unsafe { free_subtree(child, level - 1, phys_offset, fa) };
                }
            }
        }
    } // снимаем &mut на table ДО освобождения её фрейма (deallocate пишет в первое слово)
      // SAFETY: все дочерние записи освобождены; сам фрейм таблицы больше не нужен.
    unsafe { fa.deallocate_frame(frame) };
}
