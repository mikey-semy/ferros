//! Интеграционный тест M5c1: ядро загружает НАСТОЯЩИЙ, отдельно собранный ELF и
//! запускает его в **кольце 3**.
//!
//! `main` поднимает пейджинг, отдаёт встроенный ELF (`user/hello`) загрузчику и прыгает в
//! его точку входа. Программа делает `write(1, …)` и `exit(0)`; на `exit` ядро
//! раскручивается обратно сюда. Тест сверяет зафиксированное ядром.
//!
//! Почему это доказательство: без рабочего перехода кольцо 3 ⇄ ядро (или при кривой
//! загрузке ELF) был бы тройной сброс/фолт → таймаут. А зафиксированные fd, ненулевая
//! длина, `user_rsp` на пользовательском стеке и факт `exit` показывают, что отдельно
//! скомпилированная программа реально загрузилась, поработала в кольце 3 и завершилась.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU64, Ordering};
use ferros::arch::x86_64::syscall::run_user_elf;
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::paging;
use ferros::syscall::elf::HELLO_ELF;
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

    // Загрузка и запуск встроенного ELF. Возврат — когда программа сделает `exit`.
    // SAFETY: вызывается один раз; mapper/frame_allocator относятся к активной таблице,
    // адреса сегментов/стека свободны.
    let user_stack_top = unsafe { run_user_elf(HELLO_ELF, &mut mapper, &mut frame_allocator) };
    USER_STACK_TOP.store(user_stack_top, Ordering::SeqCst);

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// Отдельно собранный ELF загрузился, отработал в кольце 3 (`write`) и завершился (`exit`).
#[test_case]
fn elf_loads_runs_and_exits() {
    use ferros::syscall::{
        EXIT_CALLS, LAST_EXIT_CODE, LAST_WRITE_FD, LAST_WRITE_LEN, LAST_WRITE_USER_RSP,
    };

    // (1) программа сделала `write` в stdout (fd=1) через входной трамплин `syscall`...
    assert_eq!(LAST_WRITE_FD.load(Ordering::SeqCst), 1, "write fd mismatch");
    // ...и записала непустой буфер (строку из своего загруженного .rodata через uaccess).
    assert!(
        LAST_WRITE_LEN.load(Ordering::SeqCst) > 0,
        "write wrote nothing"
    );
    // (2) программа шла в кольце 3 на своём стеке: user_rsp лежит ВНУТРИ страницы user-
    //     стека (настоящая программа использует стек в прологе, поэтому rsp ниже вершины,
    //     но в пределах [base, top]).
    let top = USER_STACK_TOP.load(Ordering::SeqCst);
    let base = top - 4096;
    let rsp = LAST_WRITE_USER_RSP.load(Ordering::SeqCst);
    assert!(
        rsp > base && rsp <= top,
        "write did not run on the ring-3 user stack: rsp={rsp:#x} not in ({base:#x}, {top:#x}]"
    );
    // (3) `exit` дошёл до ядра (раскрутка обратно сюда состоялась) с кодом 0.
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
