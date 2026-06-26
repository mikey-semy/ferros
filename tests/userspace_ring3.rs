//! Интеграционный тест M5b: первая пользовательская программа в **кольце 3** делает
//! настоящие Linux-вызовы `write` и `exit`.
//!
//! `main` поднимает пейджинг, маппит пользовательские страницы, копирует туда крошечную
//! программу и строку, прыгает в кольцо 3. Программа печатает строку через `write(1, …)`
//! и завершается `exit(0)`; на `exit` ядро раскручивается обратно сюда. Тест затем
//! сверяет зафиксированное ядром.
//!
//! Почему это доказательство: без рабочего перехода кольцо 3 ⇄ ядро был бы тройной сброс
//! (QEMU перезагрузился бы) → таймаут. Совпадение длины и суммы байт доказывает, что ядро
//! прочитало из памяти пользователя именно то, что нужно (через `uaccess`); совпадение
//! `user_rsp` с вершиной user-стека — что код шёл в кольце 3 на своём стеке; а возврат
//! управления — что `exit` дошёл до ядра и раскрутка сработала.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU64, Ordering};
use ferros::arch::x86_64::syscall::{run_user_hello, HELLO_MSG};
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::paging;
use x86_64::VirtAddr;

entry_point!(main);

/// Вершина пользовательского стека, использованная программой (для сверки с зафиксированным
/// `user_rsp`). Заполняется в `main` после запуска программы.
static USER_STACK_TOP: AtomicU64 = AtomicU64::new(0);

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init вызывается один раз.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };

    // Запуск первой пользовательской программы. Возврат — когда она сделает `exit`.
    // SAFETY: вызывается один раз; mapper/frame_allocator относятся к активной таблице,
    // выбранные пользовательские адреса свободны.
    let user_stack_top = unsafe { run_user_hello(&mut mapper, &mut frame_allocator) };
    USER_STACK_TOP.store(user_stack_top, Ordering::SeqCst);

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// Программа кольца 3 сделала `write`, ядро его обработало, затем `exit` вернул управление.
#[test_case]
fn ring3_write_and_exit() {
    use ferros::syscall::{
        EXIT_CALLS, LAST_EXIT_CODE, LAST_WRITE_FD, LAST_WRITE_LEN, LAST_WRITE_SUM,
        LAST_WRITE_USER_RSP,
    };

    let expected_sum: u64 = HELLO_MSG.iter().map(|&b| b as u64).sum();

    // (1) `write` дошёл до ядра в stdout (fd=1) через входной трамплин `syscall`.
    assert_eq!(LAST_WRITE_FD.load(Ordering::SeqCst), 1, "write fd mismatch");
    // (2) ядро прочитало из памяти пользователя ровно нашу строку (длина + сумма байт).
    assert_eq!(
        LAST_WRITE_LEN.load(Ordering::SeqCst),
        HELLO_MSG.len() as u64,
        "write length mismatch"
    );
    assert_eq!(
        LAST_WRITE_SUM.load(Ordering::SeqCst),
        expected_sum,
        "write content mismatch (uaccess read wrong bytes)"
    );
    // (3) программа шла в кольце 3 на своём стеке: user_rsp == вершине user-стека.
    assert_eq!(
        LAST_WRITE_USER_RSP.load(Ordering::SeqCst),
        USER_STACK_TOP.load(Ordering::SeqCst),
        "write did not run on the ring-3 user stack"
    );
    // (4) `exit` дошёл до ядра (раскрутка обратно сюда состоялась) с кодом 0.
    assert!(
        EXIT_CALLS.load(Ordering::SeqCst) >= 1,
        "exit syscall never reached the kernel"
    );
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "exit code mismatch"
    );
}
