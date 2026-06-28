//! ferros — тонкая точка входа поверх библиотеки [`ferros`](../ferros/index.html).
//!
//! Вся «начинка» (драйверы, прерывания, память, инфраструктура тестов) живёт в
//! `src/lib.rs` и подмодулях. Здесь — только вход, обработчик паники и запуск shell.
//!
//! M0–M6 подняли железо и ядро; M7d делает загрузку **интерактивной**: ядро инициализирует
//! подсистемы и запускает один пользовательский процесс — `shell` (PID 1), который читает
//! команды с клавиатуры и запускает программы. Демонстрационные потоки/процессы прежних
//! милестоунов отсюда убраны — их роль выполняют интеграционные тесты в `tests/`.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

// Крейт `alloc` (Box/Vec/String) — работает после инициализации кучи (M3c).
extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use ferros::arch::x86_64::syscall::spawn_user;
use ferros::drivers::{console, keyboard, pci, virtio_blk};
use ferros::mm::frame::{self, BootInfoFrameAllocator};
use ferros::sched::executor::Executor;
use ferros::sched::{thread, Task};
use ferros::{mm, println, serial_println};
use x86_64::VirtAddr;

// `entry_point!` генерирует `_start` за нас: проверяет, что сигнатура `kernel_main`
// совпадает с тем, что передаёт bootloader, и безопасно прокидывает `&BootInfo`.
entry_point!(kernel_main);

/// Точка входа ядра. `bootloader` передаёт [`BootInfo`] — карту памяти и оффсет,
/// по которому в виртуальном пространстве отображена вся физическая память.
fn kernel_main(boot_info: &'static BootInfo) -> ! {
    ferros::init(); // GDT, IDT, PIC и включение прерываний

    println!("ferros booting...");
    serial_println!("[serial] ferros COM1 online — debug channel ready");

    // Память: построить таблицы страниц над активной иерархией, аллокатор физических фреймов,
    // затем кучу — после неё доступны Box/Vec/String и динамические структуры подсистем.
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет получен от bootloader (фича map_physical_memory) и корректен; init один раз.
    let mut mapper = unsafe { mm::paging::init(phys_mem_offset) };
    // SAFETY: карта памяти от bootloader валидна; её Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    mm::heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");

    // Драйверы ввода/диска (после кучи — они аллоцируют буферы).
    keyboard::init(); // очередь скан-кодов клавиатуры
    console::init(); // линейная дисциплина консоли (stdin)
    pci::init(); // перечисление шины PCI (находит диск virtio-blk)
    if !virtio_blk::init(phys_mem_offset, &mut frame_allocator) {
        serial_println!("[virtio-blk] init failed — disk programs will be unavailable");
    }

    // В тестовом режиме сразу запускаем тесты вместо обычной работы.
    #[cfg(test)]
    test_main();

    // Планировщик + первый пользовательский процесс. `shell` (PID 1) читает команды со stdin
    // и запускает программы (fork/execve/wait). Рядом — async-экзекьютор с задачей клавиатуры,
    // которая кормит консоль; когда shell блокируется на вводе, CPU уходит к экзекьютору.
    thread::init();
    // SAFETY: phys_offset корректен, куча поднята; адрес процесса — в свободном у ядра слоте.
    unsafe {
        spawn_user(
            ferros::syscall::elf::SHELL_ELF,
            phys_mem_offset,
            &mut frame_allocator,
        )
    };
    // Передаём фрейм-аллокатор в глобальное владение: reaper освобождает завершённые процессы,
    // а fork/execve shell'а строят/разбирают адресные пространства через него.
    frame::install(frame_allocator);

    thread::start_preemption();
    println!("ferros ready — starting shell.");

    // Главный цикл ядра: эффективный экзекьютор гоняет async-задачи (клавиатуру) и спит на
    // `hlt`, когда делать нечего. Крутится на «нулевом» потоке, который таймер тоже вытесняет.
    let mut executor = Executor::new();
    executor.spawn(Task::new(keyboard::process_input()));
    executor.run()
}

/// Обработчик паники в обычном режиме: печатаем причину на экран и в serial.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("KERNEL PANIC: {info}");
    serial_println!("KERNEL PANIC: {info}");
    ferros::hlt_loop()
}

/// В тестовом режиме паника означает провал теста — делегируем в библиотеку.
#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}
