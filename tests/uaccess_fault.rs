//! Интеграционный тест M6d1: отказоустойчивый доступ к памяти пользователя.
//!
//! `with_user_bytes` теперь предпроверяет отображение и возвращает `-EFAULT` вместо падения
//! в page fault на кривом указателе. Проверяем это прямо из ядра (без пользовательского
//! процесса): три «плохих» указателя должны дать `EFAULT`, один валидный — прочитаться.
//!
//! Почему это доказательство: до M6d1 чтение неотображённого пользовательского указателя
//! роняло ядро (page fault → паника). Если бы предпроверка была неверна, «плохой» случай
//! упал бы в fault (таймаут/паника теста), а не вернул `EFAULT`.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use ferros::arch::x86_64::syscall::USER_STACK_VA;
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::heap::HEAP_START;
use ferros::mm::{heap, paging};
use ferros::syscall::abi::EFAULT;
use ferros::syscall::uaccess::with_user_bytes;
use x86_64::structures::paging::Page;
use x86_64::VirtAddr;

entry_point!(main);

/// Адрес в пустом слоте L4 — сюда отобразим валидную пользовательскую страницу.
const TEST_USER_PAGE: u64 = 0x0000_2000_0000_0000;

/// Складывает байты буфера (замыкание для `with_user_bytes`).
fn sum_bytes(b: &[u8]) -> u64 {
    b.iter().map(|&x| x as u64).sum()
}

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init вызывается один раз.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");

    // Готовим валидную пользовательскую страницу: отображаем её как USER и пишем 1..=8.
    let page = Page::containing_address(VirtAddr::new(TEST_USER_PAGE));
    paging::map_user_page(page, &mut mapper, &mut frame_allocator);
    // SAFETY: страница только что отображена present+writable+user, мы в кольце 0.
    unsafe {
        let p = TEST_USER_PAGE as *mut u8;
        for i in 0..8u8 {
            p.add(i as usize).write_volatile(i + 1);
        }
    }

    // Освобождаем `&mut` на L4 ядра (его держит `mapper`) до того, как тесты позовут
    // `user_range_accessible`: тот строит свой `OffsetPageTable` над ТОЙ ЖЕ таблицей ядра —
    // иначе было бы два `&mut` на одну таблицу (UB). В бою такого нет: при syscall активна
    // таблица процесса, а не ядра.
    core::mem::drop(mapper);

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// Неотображённый пользовательский указатель → `EFAULT` (раньше — паника ядра). В тестовом
/// адресном пространстве ядра стек пользователя (`USER_STACK_VA`) не отображён.
#[test_case]
fn rejects_unmapped_user_pointer() {
    assert_eq!(
        with_user_bytes(USER_STACK_VA, 8, sum_bytes),
        Err(EFAULT),
        "unmapped user pointer must be EFAULT, not a kernel fault"
    );
}

/// Указатель в половину ядра отвергается ещё проверкой границ.
#[test_case]
fn rejects_kernel_pointer() {
    assert_eq!(
        with_user_bytes(0xFFFF_8000_0000_0000, 8, sum_bytes),
        Err(EFAULT),
        "kernel-half pointer must be EFAULT"
    );
}

/// Куча ядра отображена в пользовательской половине, но БЕЗ `USER_ACCESSIBLE` — доступ к
/// ней через `with_user_bytes` должен дать `EFAULT` (проверяем, что смотрим именно на флаг
/// пользователя, а не только на «отображено»).
#[test_case]
fn rejects_mapped_non_user_pointer() {
    assert_eq!(
        with_user_bytes(HEAP_START as u64, 8, sum_bytes),
        Err(EFAULT),
        "mapped-but-not-user pointer (kernel heap) must be EFAULT"
    );
}

/// Валидная пользовательская страница читается; возвращаются именно записанные байты.
#[test_case]
fn accepts_valid_user_page() {
    // Записали 1..=8 → сумма 36.
    assert_eq!(
        with_user_bytes(TEST_USER_PAGE, 8, sum_bytes),
        Ok(36),
        "valid user page must read back the written bytes"
    );
}
