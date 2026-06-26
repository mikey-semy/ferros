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

// Крейт `alloc` (Box/Vec/String) — работает после инициализации кучи (M3c).
extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::{hlt_loop, mm, println, serial_println};
use x86_64::structures::paging::{Page, Translate};
use x86_64::VirtAddr;

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
    let mut mapper = unsafe { mm::paging::init(phys_mem_offset) };

    // M3a: трансляция нескольких виртуальных адресов в физические.
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

    // M3b: строим аллокатор физических фреймов и создаём НОВЫЙ маппинг.
    // SAFETY: карта памяти от bootloader валидна; её Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };

    // Берём заведомо свободную страницу (вне отображённых регионов) и отображаем её
    // на физический фрейм VGA-буфера: теперь запись в эту страницу = запись на экран.
    let page = Page::containing_address(VirtAddr::new(0x4444_4444_0000));
    mm::paging::create_example_mapping(page, &mut mapper, &mut frame_allocator);

    // Пишем "New!" через свежий маппинг. Литерал — четыре VGA-ячейки `[символ][атрибут]`
    // в порядке little-endian: 4e='N', 65='e', 77='w', 21='!', каждая с атрибутом 0xf0.
    let page_ptr: *mut u64 = page.start_address().as_mut_ptr();
    // SAFETY: страница только что отображена на VGA-буфер с правом на запись;
    // смещение 400 (×8 байт = 3200) попадает в пределах одной страницы 4 КиБ.
    unsafe {
        page_ptr
            .offset(400)
            .write_volatile(0xf0_21_f0_77_f0_65_f0_4e)
    };
    serial_println!("[mm] new mapping ok; wrote 'New!' to VGA via the fresh page");

    // M3c: инициализируем кучу — после этого доступна динамическая память.
    mm::heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");

    // Динамические аллокации поверх кучи: Box (одно значение) и Vec (растущий массив).
    let boxed = Box::new(42);
    let mut numbers = Vec::new();
    for i in 1..=10 {
        numbers.push(i);
    }
    serial_println!(
        "[mm] heap up: Box={} at {:p}, Vec sum(1..=10)={}",
        boxed,
        boxed,
        numbers.iter().sum::<i32>()
    );

    // String живёт на куче — печатаем на VGA, чтобы куча была видна и глазами.
    let greeting = String::from("heap online: Box + Vec + String work!");
    println!("{greeting}");

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
