//! ferros — тонкая точка входа поверх библиотеки [`ferros`](../ferros/index.html).
//!
//! Вся «начинка» (драйверы, прерывания, память, инфраструктура тестов) живёт в
//! `src/lib.rs` и подмодулях. Здесь — только вход, обработчик паники и приветствие.
//!
//! M0: загрузка. M1: VGA + serial + тесты. M2: GDT/IDT/PIC.
//! M3a: получаем `BootInfo` через `entry_point!`, строим `OffsetPageTable` и
//! демонстрируем трансляцию виртуальных адресов в физические.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use ferros::{hlt_loop, mm, println, serial_println};
use x86_64::{structures::paging::Translate, VirtAddr};

// `entry_point!` генерирует `_start` за нас: проверяет, что сигнатура `kernel_main`
// совпадает с тем, что передаёт bootloader, и безопасно прокидывает `&BootInfo`.
// Это надёжнее, чем писать `extern "C" fn _start()` вручную и вытаскивать аргумент.
entry_point!(kernel_main);

/// Точка входа ядра. `bootloader` передаёт [`BootInfo`] — карту памяти и оффсет,
/// по которому в виртуальном пространстве отображена вся физическая память.
fn kernel_main(boot_info: &'static BootInfo) -> ! {
    ferros::init(); // GDT, IDT, PIC и включение прерываний

    println!("ferros booting...");
    serial_println!("[serial] ferros COM1 online — debug channel ready");

    // M3a: строим OffsetPageTable над активной иерархией таблиц и переводим
    // несколько виртуальных адресов в физические — видно, что дерево таблиц
    // прочитано верно (отображённые адреса дают Some, неотображённые — None).
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет получен от bootloader (фича map_physical_memory) и корректен;
    // init вызывается ровно один раз.
    let mapper = unsafe { mm::paging::init(phys_mem_offset) };

    let addresses = [
        0xb8000,                          // VGA-буфер → ожидаем Some(физ. адрес)
        boot_info.physical_memory_offset, // база отображения физпамяти → Some
        0xdead_beef,                      // ничем не отображён → None
    ];
    serial_println!("[mm] virt -> phys translations:");
    for &address in &addresses {
        let virt = VirtAddr::new(address);
        let phys = mapper.translate_addr(virt);
        // Диагностику шлём в serial — это наш отладочный канал (виден в логах/CI).
        serial_println!("  {virt:?} -> {phys:?}");
    }

    println!("ferros ready. Timer ticks below; type on the keyboard:");

    // В тестовом режиме сразу запускаем тесты вместо обычной работы.
    #[cfg(test)]
    test_main();

    hlt_loop()
}

/// Обработчик паники в обычном режиме: печатаем причину на экран и в serial.
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
