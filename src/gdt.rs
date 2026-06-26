//! GDT (Global Descriptor Table) + TSS с отдельным стеком для double fault.
//!
//! # Зачем
//!
//! Если стек ядра переполнится, попытка процессора сохранить кадр исключения на
//! сломанном стеке вызовет ещё одно исключение → double fault → (если и он на
//! сломанном стеке) → triple fault → перезагрузка машины.
//!
//! Чтобы этого не случилось, обработчик double fault должен работать на **другом,
//! заведомо хорошем стеке**. Такие стеки хранит **TSS** в таблице IST (Interrupt
//! Stack Table, до 7 штук). А чтобы процессор узнал про TSS, его дескриптор кладут
//! в **GDT** и активируют инструкцией `ltr`.

use spin::LazyLock;
use x86_64::instructions::segmentation::{Segment, CS};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

/// Индекс IST-стека, который мы выделяем под обработчик double fault.
pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

/// TSS с одним IST-стеком для double fault.
static TSS: LazyLock<TaskStateSegment> = LazyLock::new(|| {
    let mut tss = TaskStateSegment::new();
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
        /// Размер аварийного стека (5 страниц по 4 КиБ).
        const STACK_SIZE: usize = 4096 * 5;
        static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

        // Берём адрес через `&raw const`, чтобы не создавать ссылку на `static mut`
        // (это было бы небезопасно и вызвало бы предупреждение компилятора).
        let stack_start = VirtAddr::from_ptr(&raw const STACK);
        // Стек растёт вниз, поэтому процессору отдаём ВЕРХНЮЮ границу.
        stack_start + STACK_SIZE as u64
    };
    tss
});

/// Селекторы сегментов, которые нужно установить после загрузки GDT.
struct Selectors {
    code_selector: SegmentSelector,
    tss_selector: SegmentSelector,
}

/// GDT с сегментом кода ядра и дескриптором TSS.
static GDT: LazyLock<(GlobalDescriptorTable, Selectors)> = LazyLock::new(|| {
    let mut gdt = GlobalDescriptorTable::new();
    let code_selector = gdt.append(Descriptor::kernel_code_segment());
    let tss_selector = gdt.append(Descriptor::tss_segment(&TSS));
    (
        gdt,
        Selectors {
            code_selector,
            tss_selector,
        },
    )
});

/// Загружает GDT, перезагружает регистр кода `CS` и активирует TSS (`ltr`).
/// Вызывать до загрузки IDT (та ссылается на IST-индекс из TSS).
pub fn init() {
    GDT.0.load();
    // SAFETY: селекторы получены из только что загруженной GDT и валидны.
    unsafe {
        CS::set_reg(GDT.1.code_selector);
        load_tss(GDT.1.tss_selector);
    }
}
