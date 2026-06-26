//! ferros — тонкая точка входа поверх библиотеки [`ferros`](../ferros/index.html).
//!
//! Вся «начинка» (драйверы VGA/serial, инфраструктура тестов) живёт в `src/lib.rs`.
//! Здесь — только загрузочный `_start`, обработчик паники и приветствие.
//!
//! M0: загрузка. M1a: VGA + `println!`. M1b: serial + печать паники.
//! M1c: код вынесен в библиотеку, добавлен тест-фреймворк (`cargo test` в QEMU).

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use ferros::{hlt_loop, println, serial_println};

/// Точка входа ядра. Bootloader (`bootloader` 0.9) прыгает на символ `_start`.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    ferros::init(); // GDT, IDT, PIC и включение прерываний

    println!("ferros booting...");
    println!("VGA writer online: {}x{} text mode.", 80, 25);
    serial_println!("[serial] ferros COM1 online — debug channel ready");
    println!("ferros ready. Timer ticks below; type on the keyboard:");

    // В тестовом режиме сразу запускаем тесты вместо обычной работы.
    #[cfg(test)]
    test_main();

    hlt_loop()
}

/// Обработчик паники в обычном режиме: печатаем причину на экран и в serial.
///
/// Замечание: вызов `println!`/`serial_println!` из паники теоретически может
/// попасть на уже захваченный замок. В M2 (с прерываниями) обернём это в
/// `without_interrupts`, чтобы исключить дедлок.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("KERNEL PANIC: {info}");
    serial_println!("KERNEL PANIC: {info}");
    hlt_loop()
}

/// В тестовом режиме паника означает провал теста — делегируем в библиотеку.
#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}
