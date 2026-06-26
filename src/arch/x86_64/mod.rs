//! Архитектура x86_64: код, завязанный на конкретный CPU и платформу PC.
//!
//! Здесь живут таблицы дескрипторов (GDT/TSS) и обработка прерываний (IDT/PIC) —
//! всё, что нельзя перенести на ARM/RISC-V без переписывания. Переносимое ядро
//! обращается сюда только через [`init`] и публичные элементы подмодулей.

pub mod context;
pub mod gdt;
pub mod interrupts;
pub mod syscall;

/// Инициализация процессора под x86_64.
///
/// Порядок критичен: сначала GDT+TSS (даёт IST-стек для double fault и селекторы колец),
/// затем IDT (ссылается на IST-стек), затем настройка `syscall` (берёт селекторы из GDT),
/// затем перемап PIC и `sti`. После `sti` ядро реагирует на аппаратные прерывания.
pub fn init() {
    gdt::init();
    interrupts::init_idt();
    // M5: настройка инструкции `syscall` (MSR STAR/LSTAR/SFMASK, EFER.SCE). После GDT —
    // нужны её селекторы колец 0/3.
    syscall::init();
    // SAFETY: PIC перемаплен на свободные векторы 32..47 (см. interrupts.rs);
    // исключения CPU (0..31) не затронуты.
    unsafe { interrupts::PICS.lock().initialize() };
    // `sti` — разрешаем аппаратные прерывания.
    x86_64::instructions::interrupts::enable();
}
