//! Пользовательская проверка argv (M7b): читает argc/argv с начального стека (System V ABI) и
//! завершается с 0, если получила ожидаемые аргументы `["ARGVECHO", "ping", "pong"]`, иначе —
//! отличимым кодом на каждом несовпадении. Запускается через `execve` из `execargv`.
//!
//! По System V на входе в `_start` `rsp` указывает на `argc`, за ним идут указатели `argv` и
//! завершающий `NULL`. Обычная Rust-`_start` могла бы сдвинуть `rsp` прологом, поэтому точка
//! входа — на ассемблере: захватывает `rsp` ДО пролога и передаёт в [`argv_main`].

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

const SYS_EXIT: u64 = 60;

// _start: rdi = начальный rsp (указывает на argc), затем зовём argv_main. `call` (а не `jmp`)
// выравнивает стек по ABI на входе в argv_main.
global_asm!(
    ".global _start",
    "_start:",
    "    mov rdi, rsp",
    "    call argv_main",
);

/// `exit(code)` — не возвращается.
///
/// # Safety
/// Управление в программу уже не вернётся.
unsafe fn sys_exit(code: u64) -> ! {
    asm!("syscall", in("rax") SYS_EXIT, in("rdi") code, options(noreturn));
}

/// Сравнивает C-строку по указателю `p` с `expected` (включая нулевой байт сразу за ней —
/// иначе `"ping"` совпало бы с префиксом `"pinger"`).
///
/// # Safety
/// `p` указывает на читаемую нуль-терминированную строку.
unsafe fn cstr_eq(p: *const u8, expected: &[u8]) -> bool {
    let mut i = 0;
    while i < expected.len() {
        if *p.add(i) != expected[i] {
            return false;
        }
        i += 1;
    }
    *p.add(expected.len()) == 0
}

#[no_mangle]
extern "C" fn argv_main(sp: *const u64) -> ! {
    // sp[0] = argc; sp[1..1+argc] = указатели argv; далее NULL.
    let argc = unsafe { *sp };
    let argv = unsafe { sp.add(1) as *const *const u8 };

    // SAFETY: ядро построило валидный начальный стек; читаем argc и argc указателей argv.
    let code = unsafe {
        if argc != 3 {
            10
        } else if !cstr_eq(*argv, b"ARGVECHO") {
            11
        } else if !cstr_eq(*argv.add(1), b"ping") {
            12
        } else if !cstr_eq(*argv.add(2), b"pong") {
            13
        } else {
            0
        }
    };

    // SAFETY: завершаемся итоговым кодом.
    unsafe { sys_exit(code) }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
