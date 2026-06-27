//! Интеграционный тест M5c3: пользовательский ELF запускается как **планируемая задача**
//! в своём адресном пространстве и вытесняется планировщиком наравне с потоками ядра.
//!
//! `main` поднимает пейджинг/кучу, заводит планировщик и спавнит процесс из встроенного
//! ELF (`user/hello`), затем включает вытеснение и крутится на «нулевом» потоке, пока
//! процесс не отработает: таймер переключает на него (со сменой `CR3` и rsp0), он печатает
//! строку через `write` и завершается `exit` (планировщик помечает его мёртвым и
//! возвращается к «нулевому» потоку). Тест сверяет зафиксированное ядром.
//!
//! Почему это доказательство: без рабочих переключения `CR3`/rsp0, входа в кольцо 3 и
//! завершения процесса был бы тройной сброс/зависание → таймаут. Плюс проверяем
//! **изоляцию**: пользовательский регион не виден в адресном пространстве ядра.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};
use ferros::arch::x86_64::syscall::{spawn_user, USER_STACK_VA};
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::{heap, paging};
use ferros::sched::thread;
use ferros::syscall::elf::HELLO_ELF;
use ferros::syscall::EXIT_CALLS;
use x86_64::structures::paging::Translate;
use x86_64::VirtAddr;

entry_point!(main);

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
    heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");

    // Планировщик + пользовательский процесс как задача.
    thread::init();
    // SAFETY: phys_mem_offset корректен, куча поднята; адреса процесса — в свободном у ядра слоте.
    unsafe { spawn_user(HELLO_ELF, phys_mem_offset, &mut frame_allocator) };
    thread::start_preemption();

    // Крутимся на «нулевом» потоке, пока процесс не отработает и не завершится. Вытеснение
    // переключит на него и обратно; страховка-лимит ловит зависание.
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) == 0 {
        spins += 1;
        assert!(spins < 5_000_000_000, "user process never ran/exited");
        core::hint::spin_loop();
    }
    thread::stop_preemption();

    // Изоляция: пользовательская память жила в PML4 процесса — в таблице ядра её нет.
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

/// Процесс отработал в кольце 3 как планируемая задача, завершился и изолирован от ядра.
#[test_case]
fn user_process_scheduled_runs_and_exits() {
    use ferros::syscall::{LAST_EXIT_CODE, LAST_WRITE_FD, LAST_WRITE_LEN, LAST_WRITE_USER_RSP};

    // (1) процесс сделал `write` в stdout (fd=1) с непустым буфером.
    assert_eq!(LAST_WRITE_FD.load(Ordering::SeqCst), 1, "write fd mismatch");
    assert!(
        LAST_WRITE_LEN.load(Ordering::SeqCst) > 0,
        "write wrote nothing"
    );
    // (2) шёл в кольце 3 на пользовательском стеке: user_rsp внутри страницы стека.
    let top = USER_STACK_VA + 4096;
    let rsp = LAST_WRITE_USER_RSP.load(Ordering::SeqCst);
    assert!(
        rsp > USER_STACK_VA && rsp <= top,
        "write did not run on the ring-3 user stack: rsp={rsp:#x} not in ({USER_STACK_VA:#x}, {top:#x}]"
    );
    // (3) `exit` дошёл до ядра и завершил процесс (мы вернулись к «нулевому» потоку) с кодом 0.
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
