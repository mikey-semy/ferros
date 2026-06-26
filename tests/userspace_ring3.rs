//! Интеграционный тест M5c1/M5c2: ядро загружает НАСТОЯЩИЙ, отдельно собранный ELF и
//! запускает его в **кольце 3**, в **собственном адресном пространстве**.
//!
//! `main` поднимает пейджинг, отдаёт встроенный ELF (`user/hello`) загрузчику; тот заводит
//! процессу свой PML4 (память ядра общая), грузит сегменты, переключает `CR3` и прыгает в
//! точку входа. Программа делает `write(1, …)` и `exit(0)`; на `exit` ядро восстанавливает
//! `CR3` и раскручивается обратно сюда. Тест сверяет зафиксированное ядром.
//!
//! Почему это доказательство: без рабочего перехода кольцо 3 ⇄ ядро (или при кривой
//! загрузке/переключении адресного пространства) был бы тройной сброс → таймаут. Плюс мы
//! проверяем **изоляцию**: пользовательский регион НЕ виден в адресном пространстве ядра.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use ferros::arch::x86_64::syscall::{run_user_elf, USER_STACK_VA};
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::paging;
use ferros::syscall::elf::HELLO_ELF;
use x86_64::structures::paging::Translate;
use x86_64::VirtAddr;

entry_point!(main);

/// Вершина пользовательского стека, использованная программой (для сверки с `user_rsp`).
static USER_STACK_TOP: AtomicU64 = AtomicU64::new(0);
/// Изолирован ли пользовательский регион (не виден в адресном пространстве ядра).
static USER_ISOLATED: AtomicBool = AtomicBool::new(false);

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init вызывается один раз. Этот маппер — над
    // таблицей ЯДРА (для последующей проверки изоляции).
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    // Куча нужна загрузчику ELF (дедуп страниц) и доступна процессу (память ядра общая).
    ferros::mm::heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");

    // Загрузка и запуск встроенного ELF в собственном адресном пространстве. Возврат —
    // когда программа сделает `exit` (тогда `CR3` ядра уже восстановлен).
    // SAFETY: вызывается один раз; phys_mem_offset корректен; адреса попадают в свободный
    // у ядра слот.
    let user_stack_top = unsafe { run_user_elf(HELLO_ELF, phys_mem_offset, &mut frame_allocator) };
    USER_STACK_TOP.store(user_stack_top, Ordering::SeqCst);

    // Изоляция: пользовательская память жила в PML4 процесса, поэтому в таблице ядра её
    // быть не должно. Проверяем, что user-адрес не транслируется в ядровом пространстве.
    let isolated = mapper
        .translate_addr(VirtAddr::new(USER_STACK_VA))
        .is_none();
    USER_ISOLATED.store(isolated, Ordering::SeqCst);

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// Отдельно собранный ELF загрузился, отработал в кольце 3 (`write`), завершился (`exit`)
/// и его память изолирована от адресного пространства ядра.
#[test_case]
fn elf_runs_in_isolated_address_space() {
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
    // (2) программа шла в кольце 3 на своём стеке: user_rsp лежит ВНУТРИ страницы стека.
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
    // (4) ИЗОЛЯЦИЯ: пользовательский регион не виден в адресном пространстве ядра.
    assert!(
        USER_ISOLATED.load(Ordering::SeqCst),
        "user memory is visible in the kernel address space (not isolated)"
    );
}
