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
use pc_keyboard::{layouts::Us104Key, DecodedKey, HandleControl, PS2Keyboard, ScancodeSet1};
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

/// Обработчик таймера (PIT, ~18 Гц). Печатает точку и сигналит PIC об окончании.
extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    crate::print!(".");
    // SAFETY: вектор корректен; без EOI следующего тика не будет.
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
}

/// Глобальный декодер PS/2-клавиатуры (раскладка US, scancode set 1).
static KEYBOARD: LazyLock<Mutex<PS2Keyboard<Us104Key, ScancodeSet1>>> = LazyLock::new(|| {
    Mutex::new(PS2Keyboard::new(
        ScancodeSet1::new(),
        Us104Key,
        HandleControl::Ignore,
    ))
});

/// Обработчик клавиатуры. Читает скан-код из порта `0x60`, декодирует и печатает
/// символ.
extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let mut port = Port::new(0x60);
    // SAFETY: 0x60 — порт данных PS/2-контроллера клавиатуры.
    let scancode: u8 = unsafe { port.read() };

    let mut keyboard = KEYBOARD.lock();
    if let Ok(Some(event)) = keyboard.add_byte(scancode) {
        if let Some(key) = keyboard.process_keyevent(event) {
            match key {
                DecodedKey::Unicode(c) => crate::print!("{c}"),
                DecodedKey::RawKey(k) => crate::print!("{k:?}"),
            }
        }
    }

    // SAFETY: вектор корректен.
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}
