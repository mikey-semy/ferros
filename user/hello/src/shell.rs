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
const SYS_OPEN: u64 = 2;
const SYS_CLOSE: u64 = 3;
const SYS_PIPE: u64 = 22;
const SYS_DUP2: u64 = 33;
const SYS_FORK: u64 = 57;
const SYS_EXECVE: u64 = 59;
const SYS_EXIT: u64 = 60;
const SYS_WAIT4: u64 = 61;
const SYS_GETCWD: u64 = 79;
const SYS_CHDIR: u64 = 80;

// Флаги open(2) для редиректов.
const O_RDONLY: u64 = 0;
const O_WRONLY: u64 = 0o1;
const O_CREAT: u64 = 0o100;
const O_TRUNC: u64 = 0o1000;
const O_APPEND: u64 = 0o2000;

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

/// Содержит ли нуль-терминированная строка `/` (тогда это путь, а не голое имя команды).
///
/// # Safety
/// `p` указывает на читаемую нуль-терминированную строку.
unsafe fn has_slash(p: *const u8) -> bool {
    let mut i = 0;
    loop {
        match *p.add(i) {
            0 => return false,
            b'/' => return true,
            _ => i += 1,
        }
    }
}

/// Собирает в `buf` путь `"/bin/" + cmd` (нуль-терминированный) и возвращает указатель на него —
/// простой поиск команды без `/` в каталоге `/bin` (PATH из одного каталога). Усечёт по размеру
/// буфера, но `/bin/`+имя 8.3 заведомо влезает.
///
/// # Safety
/// `cmd` — читаемая нуль-терминированная строка; `buf` — наш буфер.
unsafe fn build_bin_path(buf: &mut [u8], cmd: *const u8) -> *const u8 {
    let prefix = b"/bin/";
    let mut n = 0;
    while n < prefix.len() {
        buf[n] = prefix[n];
        n += 1;
    }
    let mut i = 0;
    while n < buf.len() - 1 {
        let c = *cmd.add(i);
        if c == 0 {
            break;
        }
        buf[n] = c;
        n += 1;
        i += 1;
    }
    buf[n] = 0;
    buf.as_ptr()
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

/// Редиректы команды (M7g1): куда направить stdin/stdout. Пустой путь (`null`) — без редиректа.
struct Redirects {
    /// Файл для stdin (`< файл`) или `null`.
    in_path: *const u8,
    /// Файл для stdout (`> файл` / `>> файл`) или `null`.
    out_path: *const u8,
    /// Дописывать (`>>`) вместо перезаписи (`>`).
    append: bool,
}

/// Вынимает из `tokens` (слова одной команды) операторы редиректа `<`/`>`/`>>` и их цели,
/// складывая прочие слова в `clean` (с завершающим `NULL`). Возвращает число слов в `clean` и сами
/// редиректы. Оператор без следующей цели игнорируется. Операторы — отдельные слова (вокруг пробелы).
fn parse_redirects(tokens: &[*const u8], clean: &mut [*const u8; MAX_ARGS]) -> (usize, Redirects) {
    let mut clean_argc = 0;
    let mut redir = Redirects {
        in_path: ptr::null(),
        out_path: ptr::null(),
        append: false,
    };
    let mut i = 0;
    // Оставляем место под завершающий NULL (clean_argc ≤ MAX_ARGS-1).
    while i < tokens.len() && clean_argc < MAX_ARGS - 1 {
        let tok = tokens[i];
        // SAFETY: tok — нуль-терминированное слово в буфере строки.
        let (out, app, inp) = unsafe {
            (
                cstr_eq(tok, b">"),
                cstr_eq(tok, b">>"),
                cstr_eq(tok, b"<"),
            )
        };
        if out || app || inp {
            if i + 1 < tokens.len() {
                let target = tokens[i + 1];
                if inp {
                    redir.in_path = target;
                } else {
                    redir.out_path = target;
                    redir.append = app;
                }
                i += 2;
            } else {
                i += 1; // оператор без цели — игнорируем
            }
            continue;
        }
        clean[clean_argc] = tok;
        clean_argc += 1;
        i += 1;
    }
    clean[clean_argc] = ptr::null();
    (clean_argc, redir)
}

/// Индекс первого `|` в словах команды (для пайплайна), либо `None`.
fn find_pipe(tokens: &[*const u8]) -> Option<usize> {
    tokens.iter().position(|&t| unsafe { cstr_eq(t, b"|") })
}

/// Заменяет образ ребёнка запрошенной программой: применяет редиректы, резолвит команду (голое имя
/// → `/bin`, путь с `/` — как есть) и `execve`. Не возвращается; при неудаче — код 127.
///
/// # Safety
/// Вызывать ТОЛЬКО в дочернем процессе (меняет его дескрипторы). `cmd_argv` нуль-терминирован.
unsafe fn exec_command(cmd_argv: &[*const u8; MAX_ARGS], redir: &Redirects) -> ! {
    apply_redirects(redir);
    let mut pathbuf = [0u8; 256];
    let cmd = if has_slash(cmd_argv[0]) {
        cmd_argv[0]
    } else {
        build_bin_path(&mut pathbuf, cmd_argv[0])
    };
    sc3(SYS_EXECVE, cmd as u64, cmd_argv.as_ptr() as u64, 0);
    write_str(b"ferros: command not found\n");
    sys_exit(127);
}

/// Запускает пайплайн `left | right` (M7g2): создаёт канал, форкает два процесса (stdout левого →
/// канал → stdin правого) и ждёт оба. Родитель ОБЯЗАН закрыть оба конца канала, иначе правый не
/// увидит EOF. Поддержан один `|` (две команды).
///
/// # Safety
/// `*_argv` нуль-терминированы; вызывать из shell-процесса (форкает детей).
unsafe fn run_pipeline(
    left_argv: &[*const u8; MAX_ARGS],
    left_redir: &Redirects,
    right_argv: &[*const u8; MAX_ARGS],
    right_redir: &Redirects,
) {
    let mut fds = [0i32; 2];
    if sc1(SYS_PIPE, fds.as_mut_ptr() as u64) < 0 {
        write_str(b"ferros: pipe failed\n");
        return;
    }
    let (rfd, wfd) = (fds[0] as u64, fds[1] as u64);

    // Левый: stdout → конец записи канала.
    let lpid = sc0(SYS_FORK);
    if lpid == 0 {
        sc2(SYS_DUP2, wfd, 1);
        sc1(SYS_CLOSE, rfd);
        sc1(SYS_CLOSE, wfd);
        exec_command(left_argv, left_redir);
    }
    if lpid < 0 {
        // Левый форк не удался — без него пайплайн бессмыслен; закрываем оба конца и выходим.
        write_str(b"ferros: fork failed\n");
        sc1(SYS_CLOSE, rfd);
        sc1(SYS_CLOSE, wfd);
        return;
    }
    // Правый: stdin → конец чтения канала.
    let rpid = sc0(SYS_FORK);
    if rpid == 0 {
        sc2(SYS_DUP2, rfd, 0);
        sc1(SYS_CLOSE, rfd);
        sc1(SYS_CLOSE, wfd);
        exec_command(right_argv, right_redir);
    }

    // Родитель: закрываем оба конца (иначе правый не дождётся EOF), ждём обоих детей.
    sc1(SYS_CLOSE, rfd);
    sc1(SYS_CLOSE, wfd);
    if lpid > 0 {
        sc4(SYS_WAIT4, lpid as u64, 0, 0, 0);
    }
    if rpid > 0 {
        sc4(SYS_WAIT4, rpid as u64, 0, 0, 0);
    }
}

/// Применяет редиректы в ДОЧЕРНЕМ процессе перед `execve`: открывает файлы и `dup2` их на fd 0/1.
/// При ошибке открытия печатает сообщение и завершает ребёнка кодом 1.
///
/// # Safety
/// Пути нуль-терминированы; вызывать только в ребёнке (меняет его дескрипторы 0/1).
unsafe fn apply_redirects(redir: &Redirects) {
    if !redir.in_path.is_null() {
        let fd = sc3(SYS_OPEN, redir.in_path as u64, O_RDONLY, 0);
        if fd < 0 {
            write_str(b"ferros: cannot open input file\n");
            sys_exit(1);
        }
        sc2(SYS_DUP2, fd as u64, 0); // stdin ← файл
        sc1(SYS_CLOSE, fd as u64);
    }
    if !redir.out_path.is_null() {
        let extra = if redir.append { O_APPEND } else { O_TRUNC };
        let fd = sc3(SYS_OPEN, redir.out_path as u64, O_WRONLY | O_CREAT | extra, 0);
        if fd < 0 {
            write_str(b"ferros: cannot open output file\n");
            sys_exit(1);
        }
        sc2(SYS_DUP2, fd as u64, 1); // stdout → файл
        sc1(SYS_CLOSE, fd as u64);
    }
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

        // Пайплайн `left | right` (M7g2)? Делим по первому `|` и запускаем два процесса.
        if let Some(pidx) = find_pipe(&argv[..argc]) {
            let mut lclean = [ptr::null::<u8>(); MAX_ARGS];
            let (lc, lredir) = parse_redirects(&argv[..pidx], &mut lclean);
            let mut rclean = [ptr::null::<u8>(); MAX_ARGS];
            let (rc, rredir) = parse_redirects(&argv[pidx + 1..argc], &mut rclean);
            if lc == 0 || rc == 0 {
                write_str(b"ferros: syntax error near `|`\n");
                continue;
            }
            // SAFETY: clean-argv'ы нуль-терминированы; запускаем из shell-процесса.
            unsafe { run_pipeline(&lclean, &lredir, &rclean, &rredir) };
            continue;
        }

        // Вынимаем редиректы (`<`/`>`/`>>` + цели); в `cmd_argv` остаётся команда с аргументами.
        let mut cmd_argv = [ptr::null::<u8>(); MAX_ARGS];
        let (cmd_argc, redir) = parse_redirects(&argv[..argc], &mut cmd_argv);
        if cmd_argc == 0 {
            continue; // только редирект без команды
        }

        // --- Встроенные команды (редиректы к ним пока не применяются — MVP) ---
        // SAFETY: cmd_argv[0] — нуль-терминированное слово в нашем буфере.
        if unsafe { cstr_eq(cmd_argv[0], b"exit") } {
            let code = if cmd_argc > 1 {
                // SAFETY: cmd_argv[1] нуль-терминирован.
                unsafe { atoi(cmd_argv[1]) }
            } else {
                0
            };
            // SAFETY: exit.
            unsafe { sys_exit(code as u64) };
        }
        // SAFETY: см. выше.
        if unsafe { cstr_eq(cmd_argv[0], b"cd") } {
            // `cd` без аргумента — в корень.
            let target = if cmd_argc > 1 {
                cmd_argv[1] as u64
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
        if unsafe { cstr_eq(cmd_argv[0], b"pwd") } {
            let mut cwd = [0u8; 256]; // как и в приглашении — чтобы глубокий cwd не дал -ERANGE

            // SAFETY: getcwd в наш буфер.
            let m = unsafe { sc2(SYS_GETCWD, cwd.as_mut_ptr() as u64, cwd.len() as u64) };
            if m > 1 {
                write_str(&cwd[..(m as usize - 1)]);
                write_str(b"\n");
            }
            continue;
        }

        // --- Внешняя программа: fork + (редиректы) + execve + wait4 ---
        // SAFETY: fork.
        let pid = unsafe { sc0(SYS_FORK) };
        if pid == 0 {
            // Ребёнок: редиректы + замена образа (не возвращается).
            // SAFETY: в ребёнке; cmd_argv/redir — наша память.
            unsafe { exec_command(&cmd_argv, &redir) };
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
