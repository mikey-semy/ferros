//! Интеграционный тест: намеренное переполнение стека ядра.
//!
//! Без отдельного IST-стека для double fault это привело бы к triple fault
//! (перезагрузке машины). С ним обработчик double fault срабатывает и завершает
//! QEMU успехом — это и есть доказательство, что IST работает.
//!
//! Это «бинарный» тест (`harness = false` в Cargo.toml): у него свой `_start` и
//! своя IDT с особым обработчиком double fault, который не паникует, а выходит
//! из QEMU с кодом успеха.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use core::panic::PanicInfo;
use ferros::{exit_qemu, gdt, hlt_loop, serial_print, serial_println, QemuExitCode};
use spin::LazyLock;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    serial_print!("stack_overflow::stack_overflow...\t");

    gdt::init();
    TEST_IDT.load();

    stack_overflow();

    panic!("Execution continued after stack overflow");
}

/// Бесконечная рекурсия: каждый вызов кладёт на стек адрес возврата, пока стек
/// не упрётся в guard-страницу.
#[allow(unconditional_recursion)]
fn stack_overflow() {
    stack_overflow();
    // volatile-чтение после рекурсивного вызова мешает компилятору превратить
    // его в tail-call (иначе стек бы не рос и переполнения не случилось).
    unsafe { core::ptr::read_volatile(&0_u8 as *const u8) };
}

/// Своя IDT для теста: обработчик double fault не паникует, а завершает QEMU
/// успехом. Если он сработал — значит IST-стек подменился и triple fault не было.
static TEST_IDT: LazyLock<InterruptDescriptorTable> = LazyLock::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    // SAFETY: индекс настроен в TSS; gdt::init() вызывается до TEST_IDT.load().
    unsafe {
        idt.double_fault
            .set_handler_fn(test_double_fault_handler)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
    }
    idt
});

extern "x86-interrupt" fn test_double_fault_handler(
    _stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    serial_println!("[ok]");
    exit_qemu(QemuExitCode::Success);
    hlt_loop()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}
