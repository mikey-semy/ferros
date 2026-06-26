//! Библиотека ядра ferros: переиспользуемый код (драйверы, прерывания, тесты).
//!
//! Бинарник (`src/main.rs`) — тонкая точка входа поверх этой библиотеки. Вынос в
//! библиотеку нужен, чтобы один и тот же код (включая тест-харнесс) могли
//! использовать и интеграционные тесты из каталога `tests/`.
//!
//! # Тесты на голом железе
//!
//! Обычный `cargo test` опирается на стандартную библиотеку, которой у нас нет.
//! Поэтому включаем нестабильную фичу `custom_test_frameworks`: компилятор соберёт
//! все `#[test_case]` и сгенерирует функцию `test_main`, которую мы вызываем сами.
//! Раннер ([`test_runner`]) печатает результат в serial и завершает QEMU через
//! устройство isa-debug-exit.

#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![feature(abi_x86_interrupt)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]

pub mod arch;
pub mod drivers;
pub mod fs;
pub mod mm;
pub mod net;
pub mod sched;
pub mod syscall;
pub mod util;

use core::panic::PanicInfo;

/// Инициализация ядра: арх-зависимую настройку процессора (GDT+TSS, IDT, PIC,
/// включение прерываний) делегируем в [`arch`]. Вызывается из `_start` до работы.
pub fn init() {
    arch::init();
}

/// Idle-цикл: останавливаем CPU до прерывания.
pub fn hlt_loop() -> ! {
    loop {
        // SAFETY: `hlt` — привилегированная инструкция ожидания прерывания.
        unsafe { core::arch::asm!("hlt") };
    }
}

/// Коды, которые ядро пишет в порт isa-debug-exit, чтобы завершить QEMU.
/// QEMU выходит со статусом `(code << 1) | 1`, поэтому `Success` даёт 33.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

/// Завершает QEMU, записав код в порт устройства isa-debug-exit (`0xf4`).
pub fn exit_qemu(exit_code: QemuExitCode) {
    // SAFETY: порт 0xf4 принадлежит устройству isa-debug-exit, которое мы
    // добавляем в командную строку QEMU в тестовом режиме.
    unsafe {
        core::arch::asm!(
            "out dx, eax",
            in("dx") 0xf4u16,
            in("eax") exit_code as u32,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Трейт, чтобы каждый тест печатал своё имя и `[ok]`.
pub trait Testable {
    fn run(&self);
}

impl<T: Fn()> Testable for T {
    fn run(&self) {
        serial_print!("{}...\t", core::any::type_name::<T>());
        self();
        serial_println!("[ok]");
    }
}

/// Раннер тестов: печатает прогресс в serial, гоняет тесты, завершает QEMU.
pub fn test_runner(tests: &[&dyn Testable]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run();
    }
    exit_qemu(QemuExitCode::Success);
}

/// Обработчик паники в тестовом режиме: печатает `[failed]` и завершает QEMU
/// с кодом провала.
pub fn test_panic_handler(info: &PanicInfo) -> ! {
    serial_println!("[failed]");
    serial_println!("Error: {info}");
    exit_qemu(QemuExitCode::Failed);
    hlt_loop()
}

/// Точка входа для `cargo test --lib` (тесты самой библиотеки).
/// Сначала инициализируем ядро (IDT нужна, чтобы тесты прерываний не падали).
#[cfg(test)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    init();
    test_main();
    hlt_loop()
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

#[cfg(test)]
mod tests {
    /// Базовая проверка, что раннер и assert'ы работают.
    #[test_case]
    fn trivial_assertion() {
        assert_eq!(1 + 1, 2);
    }

    /// Печать большего числа строк, чем строк на экране, не должна паниковать
    /// (заодно прогоняем прокрутку VGA).
    #[test_case]
    fn vga_scrolls_without_panic() {
        for _ in 0..30 {
            crate::println!("vga scroll test line");
        }
    }

    /// breakpoint (`int3`) должен быть обработан IDT и вернуть управление —
    /// если бы обработчика не было, тут был бы тройной сброс CPU.
    #[test_case]
    fn breakpoint_exception_returns() {
        x86_64::instructions::interrupts::int3();
    }
}
