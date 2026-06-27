//! Интеграционный тест M6e2: разбор адресного пространства возвращает его память.
//!
//! Создаём адресное пространство процесса, маппим в него несколько пользовательских страниц
//! (это выделяет фреймы под PML4, промежуточные таблицы и листы), затем `destroy`. Проверяем,
//! что **все** выделенные фреймы вернулись аллокатору (ничего не утекло и не освобождено
//! лишнего), а общие с ядром записи целы — куча по-прежнему работает.
//!
//! Метод: список свободных пуст после загрузки, поэтому все аллокации идут курсором; после
//! `destroy` длина списка свободных должна равняться приросту курсора за цикл.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec::Vec;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use ferros::mm::addr_space::AddressSpace;
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::paging::map_user_page;
use ferros::mm::{heap, paging};
use x86_64::structures::paging::Page;
use x86_64::VirtAddr;

entry_point!(main);

/// Адрес в пустом (пользовательском) слоте L4 — туда маппим тестовые страницы.
const USER_BASE: u64 = 0x7F80_0000_0000;
const PAGES: u64 = 3;

/// Сколько фреймов цикл create+map выделил курсором.
static CONSUMED: AtomicUsize = AtomicUsize::new(0);
/// Сколько фреймов вернулось в список свободных после `destroy`.
static RECLAIMED: AtomicUsize = AtomicUsize::new(0);
/// Куча работает после `destroy` (общие с ядром записи целы).
static HEAP_OK: AtomicBool = AtomicBool::new(false);

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init один раз.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut fa = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    heap::init_heap(&mut mapper, &mut fa).expect("heap init failed");

    // Список свободных пуст после загрузки → все аллокации ниже идут курсором.
    debug_assert_eq!(fa.free_list_len(), 0);
    let cursor_before = fa.cursor();

    // Создаём пространство и маппим в него PAGES пользовательских страниц.
    // SAFETY: phys_offset корректен.
    let space = unsafe { AddressSpace::new_sharing_kernel(phys_mem_offset, &mut fa) };
    {
        // SAFETY: один живой маппер на это пространство; оно не активно — маппинг (запись
        // записей таблиц) активного CR3 не требует, содержимое страниц мы не пишем.
        let mut m = unsafe { space.mapper(phys_mem_offset) };
        for i in 0..PAGES {
            let page = Page::containing_address(VirtAddr::new(USER_BASE + i * 0x1000));
            map_user_page(page, &mut m, &mut fa);
        }
    } // снимаем маппер до destroy (иначе два &mut на PML4)

    let consumed = fa.cursor() - cursor_before;
    CONSUMED.store(consumed, Ordering::SeqCst);

    // Разбираем пространство — все его фреймы должны вернуться в список свободных.
    // SAFETY: пространство не активно (активна таблица ядра) и без живого маппера.
    unsafe { space.destroy(phys_mem_offset, &mut fa) };
    RECLAIMED.store(fa.free_list_len(), Ordering::SeqCst);

    // Куча всё ещё работает → общие с ядром записи `destroy` не тронул.
    let v: Vec<u32> = (0..100).collect();
    HEAP_OK.store(v.iter().sum::<u32>() == 4950, Ordering::SeqCst);

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// `destroy` вернул ровно столько фреймов, сколько цикл create+map выделил (ничего не утекло
/// и не освобождено лишнего).
#[test_case]
fn teardown_reclaims_all_frames() {
    let consumed = CONSUMED.load(Ordering::SeqCst);
    let reclaimed = RECLAIMED.load(Ordering::SeqCst);
    assert!(consumed > 0, "create+map allocated nothing?");
    assert_eq!(
        reclaimed, consumed,
        "teardown did not reclaim exactly the frames it used"
    );
}

/// После `destroy` куча (общая с ядром память) цела.
#[test_case]
fn teardown_keeps_kernel_mappings() {
    assert!(
        HEAP_OK.load(Ordering::SeqCst),
        "kernel/heap mapping broke after destroying a process address space"
    );
}
