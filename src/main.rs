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
use ferros::drivers::{console, keyboard, pci, virtio_blk};
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::sched::executor::Executor;
use ferros::sched::{thread, Task};
use ferros::{mm, print, println, serial_println};
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

    // M4c: очередь скан-кодов клавиатуры (после кучи — она аллоцирует буфер).
    keyboard::init();
    // M7a: линейная дисциплина консоли (буферы — кучевые, поэтому после кучи).
    console::init();

    // M6a: перечисляем шину PCI и печатаем устройства в serial (нужна куча — список в Vec).
    // Видно хост-мост i440fx и подключённый диск virtio-blk — фундамент для M6b.
    pci::init();

    // M6b/M6c: поднимаем драйвер диска virtio-blk; если получилось — монтируем FAT32 и для
    // наглядности читаем тестовый файл (его положил build.rs при форматировании образа).
    if virtio_blk::init(phys_mem_offset, &mut frame_allocator) {
        match ferros::fs::fat::Fat32::mount() {
            Ok(fs) => match fs.read_file("HELLO.TXT") {
                Ok(data) => serial_println!(
                    "[fat] HELLO.TXT ({} bytes): {:?}",
                    data.len(),
                    core::str::from_utf8(&data).unwrap_or("<non-utf8>")
                ),
                Err(e) => serial_println!("[fat] read_file failed: {e:?}"),
            },
            Err(e) => serial_println!("[fat] mount failed: {e:?}"),
        }
    }

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

    // В тестовом режиме сразу запускаем тесты вместо обычной работы (демо ниже до этого
    // момента не доходит — `test_main` завершает QEMU, так что вытеснение в тестах bin
    // не включается; настоящие тесты — в `tests/preemption.rs` и `tests/userspace_ring3.rs`).
    #[cfg(test)]
    test_main();

    // M5c3/M6d2: планировщик + пользовательские ПРОЦЕССЫ как планируемые задачи. Спавним
    // «hello» (печатает строку через `write` и выходит) и «reader» (M6d2: открывает и читает
    // файл с диска через файловые сисколлы, печатает его содержимое). У каждого своё
    // изолированное адресное пространство и свой стек ядра. Рядом — фоновые потоки ядра A/B
    // (M4e). Включаем вытеснение: одно ядро честно делят процессы, потоки A/B и executor.
    thread::init();
    // SAFETY: phys_mem_offset корректен, куча поднята; адреса процессов — в свободном у
    // ядра слоте (у каждого — в своём адресном пространстве).
    for elf in [
        ferros::syscall::elf::HELLO_ELF,
        ferros::syscall::elf::READER_ELF,
        ferros::syscall::elf::EXECTEST_ELF, // M6f2: заменит себя `hello` через execve
        ferros::syscall::elf::FORKTEST_ELF, // M6f3: форкнётся — два процесса завершатся
        ferros::syscall::elf::WAITTEST_ELF, // M6f4: форк + wait4 — родитель соберёт ребёнка
        ferros::syscall::elf::KILLTEST_ELF, // M6f5: форк + kill(SIGTERM) — родитель убьёт ребёнка
        ferros::syscall::elf::WRITETEST_ELF, // M6g3: создаёт файл записью через сисколлы
        ferros::syscall::elf::LSTEST_ELF,   // M6g5: листинг корня через getdents64
    ] {
        unsafe {
            ferros::arch::x86_64::syscall::spawn_user(elf, phys_mem_offset, &mut frame_allocator)
        };
    }
    // M6e3: передаём фрейм-аллокатор в глобальное владение — теперь reaper (в главном цикле)
    // сможет освобождать память завершённых процессов. Это последнее использование локального
    // аллокатора; дальше — только через глобальный.
    ferros::mm::frame::install(frame_allocator);

    thread::spawn(worker_a);
    thread::spawn(worker_b);
    thread::start_preemption();

    println!("ferros ready. User processes (hello + file reader) + threads A/B run preemptively:");

    // M4b: эффективный экзекьютор — это и есть «жизнь» ядра после старта. Он гоняет
    // async-задачи, а когда делать нечего — спит на `hlt` (CPU не жжёт впустую). Теперь
    // он крутится на «нулевом» потоке, который таймер тоже вытесняет в пользу A/B.
    let mut executor = Executor::new();
    executor.spawn(Task::new(example_task()));
    executor.spawn(Task::new(keyboard::process_input()));
    executor.run()
}

/// Грубая активная задержка: крутит `pause`-цикл, чтобы «сердцебиение» фоновых потоков
/// было видно глазами, а не пролетало 18 раз в секунду. Точное время не важно (это
/// демо); важно, что поток занят и НЕ уступает сам — уступить его заставит таймер.
fn busy_delay() {
    for _ in 0..30_000_000u64 {
        core::hint::spin_loop();
    }
}

/// Фоновый поток A: бесконечно печатает `A`, ни разу не уступая добровольно. Его
/// вытесняет таймер — иначе он бы навсегда захватил CPU и `B` никогда бы не напечатал.
extern "C" fn worker_a() -> ! {
    loop {
        print!("A");
        busy_delay();
    }
}

/// Фоновый поток B: то же, что A, но печатает `B`. На экране A и B чередуются —
/// доказательство, что таймер переключает потоки помимо их воли (M4e).
extern "C" fn worker_b() -> ! {
    loop {
        print!("B");
        busy_delay();
    }
}

/// Простейший async-блок: «асинхронно» отдаёт число (демо M4a).
async fn async_number() -> u32 {
    42
}

/// Пример задачи: дожидается `async_number().await` и печатает результат — видно,
/// что `.await` и кооперативное исполнение работают.
async fn example_task() {
    let number = async_number().await;
    println!("async task: got {number} from an .await");
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
