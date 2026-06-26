//! Интеграционный тест M5a: переход в **кольцо 3** и круговой системный вызов.
//!
//! `main` поднимает пейджинг, маппит пользовательские страницы кода/стека, копирует туда
//! крошечный пробник и прыгает в кольцо 3. Пробник делает два `syscall`: «пинг» (ядро
//! фиксирует аргумент и указатель пользовательского стека, затем `sysret` обратно в
//! кольцо 3) и «возврат» (ядро раскручивается обратно сюда). Тест затем сверяет
//! зафиксированное.
//!
//! Почему это доказательство: без рабочего перехода кольцо 3 ⇄ ядро мы бы получили
//! тройной сброс (QEMU перезагрузился бы) → таймаут теста. А совпадение `user_rsp` с
//! вершиной пользовательского стека доказывает, что код реально шёл в кольце 3 на своём
//! стеке, и что и `syscall` (вход), и `sysret` (возврат в кольцо 3) отработали.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU64, Ordering};
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::paging;
use x86_64::VirtAddr;

entry_point!(main);

/// Вершина пользовательского стека, использованная пробником (для сверки с зафиксированным
/// `user_rsp`). Заполняется в `main` после запуска пробника.
static USER_STACK_TOP: AtomicU64 = AtomicU64::new(0);

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init вызывается один раз.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };

    // Прыжок в кольцо 3 и круговой syscall. Возврат — когда пробник сделает DEBUG_RETURN.
    // SAFETY: вызывается один раз; mapper/frame_allocator относятся к активной таблице,
    // выбранные пользовательские адреса свободны.
    let user_stack_top = unsafe {
        ferros::arch::x86_64::syscall::run_ring3_probe(&mut mapper, &mut frame_allocator)
    };
    USER_STACK_TOP.store(user_stack_top, Ordering::SeqCst);

    // Пробник выполнялся с выключенными прерываниями (IF=0 в кадре iretq) — вернём IF.
    x86_64::instructions::interrupts::enable();

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// Полный круг: код кольца 3 сделал `syscall`, ядро его обработало и вернуло управление.
#[test_case]
fn ring3_syscall_roundtrip() {
    use ferros::syscall::{DEBUG_PING_COUNT, LAST_DEBUG_ARG, LAST_DEBUG_USER_RSP, PROBE_MAGIC};

    // (1) Вызов из кольца 3 реально дошёл до ядра через входной трамплин `syscall`.
    assert!(
        DEBUG_PING_COUNT.load(Ordering::SeqCst) >= 1,
        "ring-3 syscall never reached the kernel"
    );
    // (2) Аргумент разложен по Linux-ABI (rdi → args[0]).
    assert_eq!(
        LAST_DEBUG_ARG.load(Ordering::SeqCst),
        PROBE_MAGIC,
        "syscall argument was not marshalled per the Linux x86-64 ABI"
    );
    // (3) Код шёл в кольце 3 на пользовательском стеке: зафиксированный user_rsp равен
    //     вершине настроенного нами пользовательского стека.
    assert_eq!(
        LAST_DEBUG_USER_RSP.load(Ordering::SeqCst),
        USER_STACK_TOP.load(Ordering::SeqCst),
        "syscall did not run on the ring-3 user stack"
    );
}
