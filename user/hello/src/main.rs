//! Первая НАСТОЯЩАЯ пользовательская программа ferros (M5c1).
//!
//! Это отдельный `no_std`-бинарь: компилируется в статический ELF, ядро его загружает в
//! память кольца 3 и прыгает в `_start`. Программа делает два системных вызова по Linux
//! x86-64 ABI (номер в `rax`, аргументы в `rdi/rsi/rdx`): печатает строку через `write` в
//! stdout и завершается через `exit(0)`.
//!
//! Никакого рантайма/стандартной библиотеки: `#![no_std]` + `#![no_main]`, своя точка
//! входа `_start` и обработчик паники.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

/// Номер системного вызова Linux x86-64 `write`.
const SYS_WRITE: u64 = 1;
/// Номер системного вызова Linux x86-64 `exit`.
const SYS_EXIT: u64 = 60;

/// Точка входа программы (адрес `e_entry` в ELF). Печатает строку и завершается.
#[no_mangle]
pub extern "C" fn _start() -> ! {
    let msg = b"hello from a real ELF program in ring 3\n";

    // write(fd=1, buf=msg, count=msg.len())
    // SAFETY: `syscall` с корректными по Linux-ABI регистрами; ядро прочитает буфер из
    // нашей памяти. `rcx`/`r11` затираются инструкцией `syscall` — помечаем как clobber.
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_WRITE,
            in("rdi") 1u64,
            in("rsi") msg.as_ptr(),
            in("rdx") msg.len(),
            lateout("rax") _,
            lateout("rcx") _,
            lateout("r11") _,
        );
    }

    // exit(0) — не возвращается.
    // SAFETY: `syscall` с номером exit; управление в программу уже не вернётся.
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_EXIT,
            in("rdi") 0u64,
            options(noreturn),
        );
    }
}

/// Паника пользовательской программы: просто крутимся (в M5c1 нет ни сигналов, ни вывода
/// паники). Ядро всё равно вернёт себе управление по таймеру/следующему вызову позже.
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
