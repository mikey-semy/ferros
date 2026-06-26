//! Обработка исключений и прерываний CPU.
//!
//! Здесь живёт **IDT** (Interrupt Descriptor Table) — таблица из 256 ячеек:
//! «при прерывании/исключении номер N прыгай вот в этот обработчик». Процессор
//! находит таблицу через регистр IDTR, который мы загружаем в [`init_idt`].
//!
//! Обработчики помечены `extern "x86-interrupt"`: это особое соглашение вызова,
//! при котором компилятор сам сохраняет/восстанавливает регистры и возвращается
//! инструкцией `iretq` (а не обычным `ret`).
//!
//! M2a: только breakpoint. Double fault и аппаратные прерывания — в M2b/M2c.

use spin::LazyLock;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

/// Глобальная IDT. Обязана жить вечно (`'static`): её адрес мы отдаём процессору
/// инструкцией `lidt`, и он будет обращаться к ней всё время работы ядра.
/// Строим лениво при первом обращении.
static IDT: LazyLock<InterruptDescriptorTable> = LazyLock::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    idt.breakpoint.set_handler_fn(breakpoint_handler);
    idt
});

/// Загружает IDT в процессор (`lidt`). Вызывать один раз при старте ядра.
pub fn init_idt() {
    IDT.load();
}

/// Обработчик breakpoint (вектор 3, инструкция `int3`). Печатает кадр прерывания
/// и возвращается — выполнение продолжится с инструкции сразу после `int3`.
extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    crate::println!("EXCEPTION: BREAKPOINT\n{stack_frame:#?}");
}
