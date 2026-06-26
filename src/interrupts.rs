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
//! M2a: breakpoint. M2b: double fault (на отдельном IST-стеке из [`crate::gdt`]).
//! Аппаратные прерывания — в M2c.

use crate::gdt;
use spin::LazyLock;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

/// Глобальная IDT. Обязана жить вечно (`'static`): её адрес мы отдаём процессору
/// инструкцией `lidt`, и он будет обращаться к ней всё время работы ядра.
/// Строим лениво при первом обращении.
static IDT: LazyLock<InterruptDescriptorTable> = LazyLock::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    idt.breakpoint.set_handler_fn(breakpoint_handler);
    // Обработчик double fault — на отдельном «заведомо хорошем» стеке (IST),
    // на случай сломанного основного стека (например, при переполнении).
    // SAFETY: индекс корректен и настроен в TSS (см. `gdt.rs`).
    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
    }
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

/// Обработчик double fault. Возникает, когда CPU не смог вызвать обработчик
/// первого исключения. Вернуться нельзя (`-> !`) — паникуем с диагностикой.
extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    panic!("EXCEPTION: DOUBLE FAULT\n{stack_frame:#?}");
}
