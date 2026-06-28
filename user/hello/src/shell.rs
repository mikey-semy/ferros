//! Простейший интерактивный shell ferros (M7d) — первый «настоящий» пользователь системы.
//!
//! Цикл REPL: печатает приглашение (`<cwd>$ `), читает строку со stdin, разбивает на слова, и
//! либо выполняет встроенную команду (`cd`/`pwd`/`exit`), либо запускает внешнюю программу —
//! `fork` + `execve(argv[0], argv)` + `wait4`. Запускается ядром как PID 1.
//!
//! Без рантайма и кучи (`#![no_std]`): строка и массив argv — буферы на стеке; токенизатор режет
//! строку на месте, заменяя пробелы нулями, и собирает массив указателей на нуль-терминированные
//! слова — ровно то, что ждёт `execve`.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;
use core::ptr;

const SYS_READ: u64 = 0;
const SYS_WRITE: u64 = 1;
const SYS_FORK: u64 = 57;
const SYS_EXECVE: u64 = 59;
const SYS_EXIT: u64 = 60;
const SYS_WAIT4: u64 = 61;
const SYS_GETCWD: u64 = 79;
const SYS_CHDIR: u64 = 80;

/// Максимум слов в команде (argv), включая место под завершающий `NULL`.
const MAX_ARGS: usize = 16;

// --- Тонкие обёртки над `syscall` (Linux x86-64 ABI) ---

/// # Safety
/// Номер/аргументы должны соответствовать вызываемому сисколлу.
unsafe fn sc0(nr: u64) -> i64 {
    let ret: i64;
    asm!("syscall", inlateout("rax") nr => ret, lateout("rcx") _, lateout("r11") _);
    ret
}
/// # Safety
/// Как [`sc0`].
unsafe fn sc1(nr: u64, a: u64) -> i64 {
    let ret: i64;
    asm!("syscall", inlateout("rax") nr => ret, in("rdi") a, lateout("rcx") _, lateout("r11") _);
    ret
}
/// # Safety
/// Как [`sc0`].
unsafe fn sc2(nr: u64, a: u64, b: u64) -> i64 {
    let ret: i64;
    asm!(
        "syscall",
        inlateout("rax") nr => ret,
        in("rdi") a, in("rsi") b,
        lateout("rcx") _, lateout("r11") _,
    );
    ret
}
/// # Safety
/// Как [`sc0`].
unsafe fn sc3(nr: u64, a: u64, b: u64, c: u64) -> i64 {
    let ret: i64;
    asm!(
        "syscall",
        inlateout("rax") nr => ret,
        in("rdi") a, in("rsi") b, in("rdx") c,
        lateout("rcx") _, lateout("r11") _,
    );
    ret
}
/// `wait4` и прочие 4-аргументные: четвёртый аргумент по ABI идёт в `r10`.
///
/// # Safety
/// Как [`sc0`].
unsafe fn sc4(nr: u64, a: u64, b: u64, c: u64, d: u64) -> i64 {
    let ret: i64;
    asm!(
        "syscall",
        inlateout("rax") nr => ret,
        in("rdi") a, in("rsi") b, in("rdx") c, in("r10") d,
        lateout("rcx") _, lateout("r11") _,
    );
    ret
}

/// `exit(code)` — не возвращается.
///
/// # Safety
/// Управление не вернётся.
unsafe fn sys_exit(code: u64) -> ! {
    asm!("syscall", in("rax") SYS_EXIT, in("rdi") code, options(noreturn));
}

/// Пишет байты в stdout (fd 1).
fn write_str(s: &[u8]) {
    // SAFETY: write в stdout с корректным буфером программы.
    unsafe {
        sc3(SYS_WRITE, 1, s.as_ptr() as u64, s.len() as u64);
    }
}

/// Это пробельный разделитель слов?
fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

/// Сравнивает нуль-терминированную C-строку по указателю `p` с `expected` (точно, включая
/// завершающий нуль — чтобы `cd` не совпало с `cdrom`).
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

/// Парсит неотрицательное десятичное число из начала нуль-терминированной строки (для `exit N`).
///
/// # Safety
/// `p` указывает на читаемую нуль-терминированную строку.
unsafe fn atoi(p: *const u8) -> i32 {
    let mut v: i32 = 0;
    let mut i = 0;
    loop {
        let c = *p.add(i);
        if !c.is_ascii_digit() {
            break;
        }
        v = v.wrapping_mul(10).wrapping_add((c - b'0') as i32);
        i += 1;
    }
    v
}

/// Режет `buf[..n]` на слова на месте: каждый разделитель становится нулём, в `argv` пишутся
/// указатели на начала нуль-терминированных слов, затем завершающий `NULL`. Возвращает число слов.
fn tokenize(buf: &mut [u8], n: usize, argv: &mut [*const u8; MAX_ARGS]) -> usize {
    let mut argc = 0;
    let mut i = 0;
    // Оставляем место под завершающий NULL (argc ≤ MAX_ARGS-1).
    while i < n && argc < MAX_ARGS - 1 {
        while i < n && is_ws(buf[i]) {
            i += 1;
        }
        if i >= n {
            break;
        }
        let start = i;
        while i < n && !is_ws(buf[i]) {
            i += 1;
        }
        if i < buf.len() {
            buf[i] = 0; // терминируем слово (за концом строки байт буфера есть — читаем ≤ len-1)
        }
        // SAFETY: start < buf.len(); слово нуль-терминировано выше.
        argv[argc] = unsafe { buf.as_ptr().add(start) };
        argc += 1;
        i += 1;
    }
    argv[argc] = ptr::null();
    argc
}

/// Печатает приглашение `<cwd>$ `.
fn print_prompt() {
    // Под cwd столько же, сколько под строку ввода: иначе глубокий путь дал бы `getcwd` -ERANGE
    // и приглашение молча выродилось бы в голый `$ `.
    let mut cwd = [0u8; 256];
    // SAFETY: getcwd пишет в наш буфер, возвращает длину с нулём.
    let n = unsafe { sc2(SYS_GETCWD, cwd.as_mut_ptr() as u64, cwd.len() as u64) };
    if n > 1 {
        write_str(&cwd[..(n as usize - 1)]); // без завершающего нуля
    }
    write_str(b"$ ");
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let mut line = [0u8; 256];
    loop {
        print_prompt();

        // Читаем одну строку (канонический режим: ядро отдаёт по строке). Лимит len-1 —
        // оставить байт под завершающий нуль последнего слова в токенизаторе.
        // SAFETY: read в наш буфер.
        let n = unsafe { sc3(SYS_READ, 0, line.as_mut_ptr() as u64, (line.len() - 1) as u64) };
        if n <= 0 {
            // EOF/ошибка ввода — выходим.
            // SAFETY: exit.
            unsafe { sys_exit(0) };
        }
        let n = n as usize;

        let mut argv = [ptr::null::<u8>(); MAX_ARGS];
        let argc = tokenize(&mut line, n, &mut argv);
        if argc == 0 {
            continue; // пустая строка
        }

        // --- Встроенные команды ---
        // SAFETY: argv[0] — нуль-терминированное слово в нашем буфере.
        if unsafe { cstr_eq(argv[0], b"exit") } {
            let code = if argc > 1 {
                // SAFETY: argv[1] нуль-терминирован.
                unsafe { atoi(argv[1]) }
            } else {
                0
            };
            // SAFETY: exit.
            unsafe { sys_exit(code as u64) };
        }
        // SAFETY: см. выше.
        if unsafe { cstr_eq(argv[0], b"cd") } {
            // `cd` без аргумента — в корень.
            let target = if argc > 1 {
                argv[1] as u64
            } else {
                b"/\0".as_ptr() as u64
            };
            // SAFETY: chdir с нуль-терминированным путём.
            if unsafe { sc1(SYS_CHDIR, target) } < 0 {
                write_str(b"cd: no such directory\n");
            }
            continue;
        }
        // SAFETY: см. выше.
        if unsafe { cstr_eq(argv[0], b"pwd") } {
            let mut cwd = [0u8; 256]; // как и в приглашении — чтобы глубокий cwd не дал -ERANGE

            // SAFETY: getcwd в наш буфер.
            let m = unsafe { sc2(SYS_GETCWD, cwd.as_mut_ptr() as u64, cwd.len() as u64) };
            if m > 1 {
                write_str(&cwd[..(m as usize - 1)]);
                write_str(b"\n");
            }
            continue;
        }

        // --- Внешняя программа: fork + execve + wait4 ---
        // SAFETY: fork.
        let pid = unsafe { sc0(SYS_FORK) };
        if pid == 0 {
            // Ребёнок: заменяем образ на запрошенную программу.
            // SAFETY: execve с argv[0]/argv нашей памяти; envp пуст (NULL).
            unsafe {
                sc3(SYS_EXECVE, argv[0] as u64, argv.as_ptr() as u64, 0);
                // Сюда — только если execve не удался.
                write_str(b"ferros: command not found\n");
                sys_exit(127);
            }
        } else if pid > 0 {
            // Родитель: дожидаемся завершения ребёнка (статус не разбираем).
            // SAFETY: wait4 с нашим (опциональным) буфером статуса = NULL.
            unsafe {
                sc4(SYS_WAIT4, pid as u64, 0, 0, 0);
            }
        } else {
            write_str(b"ferros: fork failed\n");
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
