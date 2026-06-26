//! Обработка исключений и аппаратных прерываний CPU.
//!
//! **IDT** (Interrupt Descriptor Table) — таблица из 256 ячеек: «при прерывании
//! номер N прыгай вот в этот обработчик». Процессор находит её через регистр
//! IDTR, загружаемый в [`init_idt`]. Обработчики — `extern "x86-interrupt"`.
//!
//! - Исключения: breakpoint (M2a), double fault на IST-стеке (M2b).
//! - Аппаратные прерывания (M2c): таймер и клавиатура через **PIC 8259**.
//!   PIC перемаплен на векторы 32..47, чтобы не пересекаться с исключениями CPU.

use super::gdt;
use core::sync::atomic::{AtomicU64, Ordering};
use pic8259::ChainedPics;
use spin::{LazyLock, Mutex};
use x86_64::instructions::port::Port;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

/// Вектор, с которого начинается первый (master) PIC.
pub const PIC_1_OFFSET: u8 = 32;
/// Вектор, с которого начинается второй (slave) PIC.
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

/// Пара связанных PIC (master + slave), перемапленных на 32..47.
pub static PICS: Mutex<ChainedPics> =
    // SAFETY: смещения 32 и 40 свободны (исключения CPU занимают 0..31).
    Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

/// Номера векторов аппаратных прерываний (после перемапа PIC).
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard,
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Глобальная IDT, построенная лениво. Живёт вечно (`'static`), её адрес отдаём
/// процессору инструкцией `lidt`.
static IDT: LazyLock<InterruptDescriptorTable> = LazyLock::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    idt.breakpoint.set_handler_fn(breakpoint_handler);
    // SAFETY: индекс настроен в TSS (см. gdt.rs).
    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
    }
    idt[InterruptIndex::Timer.as_u8()].set_handler_fn(timer_interrupt_handler);
    idt[InterruptIndex::Keyboard.as_u8()].set_handler_fn(keyboard_interrupt_handler);
    idt
});

/// Загружает IDT в процессор (`lidt`). Вызывать один раз при старте ядра.
pub fn init_idt() {
    IDT.load();
}

/// Обработчик breakpoint (вектор 3, инструкция `int3`). Печатает кадр и
/// возвращается — выполнение продолжается после `int3`.
extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    crate::println!("EXCEPTION: BREAKPOINT\n{stack_frame:#?}");
}

/// Обработчик double fault. Вернуться нельзя (`-> !`) — паникуем с диагностикой.
extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    panic!("EXCEPTION: DOUBLE FAULT\n{stack_frame:#?}");
}

/// Счётчик тиков таймера (PIT, ~18.2 Гц). Растёт на каждом прерывании; основа отсчёта
/// времени и будущих «усыпить на N тиков».
static TICKS: AtomicU64 = AtomicU64::new(0);

/// Сколько тиков таймера прошло с загрузки.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Обработчик таймера (PIT, ~18.2 Гц). Считает тик, сигналит PIC об окончании и
/// **вытесняет** текущий поток, если включено вытеснение (M4e).
///
/// Порядок важен: EOI шлём ДО переключения. Тогда тик «закрыт» в той же активации
/// обработчика, что его получила, и каждое прерывание ровно один раз парно EOI. После
/// EOI мы всё ещё с IF=0 (вход через interrupt gate), так что новый тик не вложится,
/// пока [`on_timer_tick`] переключает контекст.
///
/// [`on_timer_tick`]: crate::sched::thread::on_timer_tick
extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    TICKS.fetch_add(1, Ordering::Relaxed);

    // SAFETY: вектор корректен; без EOI следующего тика не будет.
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }

    // Вытеснение: переключиться на следующий поток (если включено). Восстановление IF
    // сделает `iretq` при возврате в поток.
    // SAFETY: мы в обработчике прерывания (вход через interrupt gate → IF=0), как и
    // требует on_timer_tick для безопасного переключения контекста.
    unsafe { crate::sched::thread::on_timer_tick() };
}

/// Обработчик клавиатуры. Читает скан-код из порта `0x60` и отдаёт его драйверу
/// клавиатуры (M4c): тот кладёт байт в очередь и будит async-задачу-декодер. Здесь —
/// только короткая работа, как и положено обработчику прерывания (декодирование с
/// аллокациями в ISR недопустимо).
extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let mut port = Port::new(0x60);
    // SAFETY: 0x60 — порт данных PS/2-контроллера клавиатуры.
    let scancode: u8 = unsafe { port.read() };
    crate::drivers::keyboard::add_scancode(scancode);

    // SAFETY: вектор корректен.
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}
