//! Сборочный скрипт ядра (M5c1): собирает отдельную пользовательскую программу
//! `user/hello` в статический ELF и отдаёт ядру путь к нему через переменную окружения,
//! чтобы ядро встроило байты через `include_bytes!(env!("USER_HELLO_ELF"))`.
//!
//! # Тонкость: вложенный `cargo`
//!
//! Этот скрипт запускается внутри сборки ядра, и внешний `cargo` передаёт нам кучу
//! переменных окружения (`CARGO_ENCODED_RUSTFLAGS`, `CARGO_BUILD_TARGET`, `RUSTC*`, …).
//! Если их не убрать, вложенный `cargo build` для `user/hello` унаследовал бы таргет и
//! rustflags ЯДРА вместо своих — и собрал бы не то. Поэтому перед запуском вычищаем эти
//! переменные; вложенная сборка тогда берёт собственный `user/hello/.cargo/config.toml`.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let user_dir = "user/hello";

    // Пересобирать пользовательскую программу при изменении её исходников/конфигурации.
    for f in [
        "src/main.rs",
        "src/faulter.rs",
        "src/reader.rs",
        "src/getpidtest.rs",
        "src/exectest.rs",
        "src/forktest.rs",
        "src/waittest.rs",
        "src/killtest.rs",
        "src/writetest.rs",
        "src/lstest.rs",
        "src/stdintest.rs",
        "src/argvecho.rs",
        "src/execargv.rs",
        "src/cwdtest.rs",
        "src/shell.rs",
        "src/echo.rs",
        "src/cat.rs",
        "src/ls.rs",
        "src/mkdir.rs",
        "src/rm.rs",
        "src/rmdir.rs",
        "src/brktest.rs",
        "src/tlstest.rs",
        "src/stattest.rs",
        "src/timetest.rs",
        "src/idtest.rs",
        "src/ioveccheck.rs",
        "Cargo.toml",
        "Cargo.lock",
        "linker.ld",
        "x86_64-user.json",
        ".cargo/config.toml",
    ] {
        println!("cargo:rerun-if-changed={user_dir}/{f}");
    }

    // Бинари пользовательского крейта и переменные окружения, под которыми ядро их
    // встраивает. Единый список — чтобы удаление и проброс путей не разъезжались.
    let binaries = [
        ("hello", "USER_HELLO_ELF"),
        ("faulter", "USER_FAULTER_ELF"),
        ("reader", "USER_READER_ELF"),
        ("getpidtest", "USER_GETPIDTEST_ELF"),
        ("exectest", "USER_EXECTEST_ELF"),
        ("forktest", "USER_FORKTEST_ELF"),
        ("waittest", "USER_WAITTEST_ELF"),
        ("killtest", "USER_KILLTEST_ELF"),
        ("writetest", "USER_WRITETEST_ELF"),
        ("lstest", "USER_LSTEST_ELF"),
        ("stdintest", "USER_STDINTEST_ELF"),
        ("execargv", "USER_EXECARGV_ELF"),
        ("cwdtest", "USER_CWDTEST_ELF"),
        ("shell", "USER_SHELL_ELF"),
        ("brktest", "USER_BRKTEST_ELF"),
        ("tlstest", "USER_TLSTEST_ELF"),
        ("stattest", "USER_STATTEST_ELF"),
        ("timetest", "USER_TIMETEST_ELF"),
        ("idtest", "USER_IDTEST_ELF"),
        ("ioveccheck", "USER_IOVECCHECK_ELF"),
    ];

    // Удаляем прошлые ELF перед сборкой: cargo не отслеживает linker.ld / target.json как
    // входы, поэтому при их изменении сам бы не перелинковал. Удаление принуждает к
    // (быстрой) перелинковке, подхватывающей текущий скрипт/таргет. .o-файлы кэшируются,
    // так что core/alloc не пересобираются.
    let out_dir = format!("{user_dir}/target/x86_64-user/release");
    for (bin, _) in binaries {
        let _ = std::fs::remove_file(format!("{out_dir}/{bin}"));
    }
    // Бинарники, что собираются, но в ядро НЕ встраиваются (только кладутся на диск) — в списке
    // `binaries` их нет, поэтому удаляем их ELF отдельно, чтобы изменения linker.ld/target тоже
    // принудительно перелинковали их. `argvecho` — фикстура M7b; echo/cat/ls/mkdir — coreutils M7f.
    for bin in ["argvecho", "echo", "cat", "ls", "mkdir", "rm", "rmdir"] {
        let _ = std::fs::remove_file(format!("{out_dir}/{bin}"));
    }

    let mut cmd = Command::new("cargo");
    cmd.current_dir(user_dir).args(["build", "--release"]);
    // Снимаем «протёкшие» от внешнего cargo переменные, чтобы вложенная сборка
    // использовала свой таргет/флаги, а не ядровые.
    for var in [
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTFLAGS",
        "CARGO_BUILD_TARGET",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_TARGET_DIR",
        "RUSTC",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        // Jobserver внешнего cargo: если унаследовать, вложенная сборка делила бы пул
        // токенов с заблокированным родителем — риск взаимной блокировки. Пусть берёт свой.
        "CARGO_MAKEFLAGS",
        "MAKEFLAGS",
    ] {
        cmd.env_remove(var);
    }

    let status = cmd
        .status()
        .expect("failed to run `cargo build` for user/hello");
    assert!(status.success(), "building user/hello failed");

    // Абсолютные пути к собранным ELF (через CARGO_MANIFEST_DIR ядра — чистый путь без
    // префикса \\?\, который даёт canonicalize на Windows). Отдаём каждый ядру через
    // переменную окружения для `include_bytes!`.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    for (bin, env) in binaries {
        let elf = PathBuf::from(&manifest)
            .join(user_dir)
            .join(format!("target/x86_64-user/release/{bin}"));
        assert!(
            elf.exists(),
            "user ELF not found after build: {}",
            elf.display()
        );
        println!("cargo:rustc-env={env}={}", elf.display());
    }

    build_c_programs(&manifest);
    generate_disk_image(&manifest);
}

/// Собирает пользовательские программы на **C** через clang (M9g/M9h). Доказывает, что обычный
/// C-код работает на ferros: сперва свободностоящий `hello` (свой `_start`, без libc), затем
/// программа `demo` поверх **минимальной libc** (`user/c/libc`: crt0 + malloc/строки) со
/// стандартным `int main()`. **clang+lld нужны для сборки** (как nightly Rust и QEMU).
///
/// Тонкости clang для нашего таргета: модель кода `large` (база `0x7F80_0000_0000` — высокий адрес,
/// 32-битные релокации модели `small` до неё не достают); линковка через драйвер `clang`
/// (`-fuse-ld=lld`); наш `user/hello/linker.ld` (та же база/`ENTRY(_start)`, что у Rust-программ).
fn build_c_programs(manifest: &str) {
    let c_dir = PathBuf::from(manifest).join("user/c");
    let libc_dir = c_dir.join("libc");
    let linker = PathBuf::from(manifest).join("user/hello/linker.ld");
    let out_dir =
        PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set for build script"));

    for f in [
        c_dir.join("hello.c"),
        c_dir.join("fputest.c"),
        c_dir.join("demo.c"),
        c_dir.join("printftest.c"),
        c_dir.join("libcheck.c"),
        c_dir.join("catfile.c"),
        c_dir.join("ccat.c"),
        c_dir.join("dnsclient.c"),
        c_dir.join("tcpdns.c"),
        c_dir.join("httpget.c"),
        c_dir.join("stdiotest.c"),
        c_dir.join("wc.c"),
        c_dir.join("dns.h"),
        libc_dir.join("crt0.s"),
        libc_dir.join("libc.c"),
        libc_dir.join("libc.h"),
        linker.clone(),
    ] {
        println!("cargo:rerun-if-changed={}", f.display());
    }

    let clang_ok = Command::new("clang")
        .arg("--version")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(
        clang_ok,
        "clang is required to build the C user programs (M9g). Install LLVM/clang and ensure \
         `clang` is on PATH."
    );

    // M9g: свободностоящая `hello` (свой `_start`, без libc) — линкуется в одиночку.
    let hello_o = clang_compile_c(&c_dir.join("hello.c"), &out_dir.join("hello.o"), &[]);
    let hello_elf = clang_link(&[&hello_o], &out_dir.join("hello_c"), &linker);
    println!("cargo:rustc-env=USER_HELLO_C_ELF={}", hello_elf.display());

    // M9i: свободностоящая `fputest` (fork + проверка xmm0) — для теста сохранения FPU при switch.
    let fputest_o = clang_compile_c(&c_dir.join("fputest.c"), &out_dir.join("fputest.o"), &[]);
    let fputest_elf = clang_link(&[&fputest_o], &out_dir.join("fputest"), &linker);
    println!("cargo:rustc-env=USER_FPUTEST_ELF={}", fputest_elf.display());

    // M9h: минимальная libc (crt0 + libc.c) + программа `demo` со стандартным `int main()`.
    let crt0_o = clang_assemble(&libc_dir.join("crt0.s"), &out_dir.join("crt0.o"));
    let libc_o = clang_compile_c(&libc_dir.join("libc.c"), &out_dir.join("libc.o"), &[]);
    let inc = format!("-I{}", libc_dir.display());
    let demo_o = clang_compile_c(&c_dir.join("demo.c"), &out_dir.join("demo.o"), &[&inc]);
    let demo_elf = clang_link(
        &[&crt0_o, &libc_o, &demo_o],
        &out_dir.join("cdemo"),
        &linker,
    );
    println!("cargo:rustc-env=USER_CDEMO_ELF={}", demo_elf.display());

    // M9j: программа `printftest` поверх libc — проверяет printf/snprintf.
    let printf_o = clang_compile_c(
        &c_dir.join("printftest.c"),
        &out_dir.join("printftest.o"),
        &[&inc],
    );
    let printf_elf = clang_link(
        &[&crt0_o, &libc_o, &printf_o],
        &out_dir.join("printftest"),
        &linker,
    );
    println!(
        "cargo:rustc-env=USER_PRINTFTEST_ELF={}",
        printf_elf.display()
    );

    // M9k: программа `libcheck` поверх libc — проверяет строковые/мемори/atoi функции.
    let libcheck_o = clang_compile_c(
        &c_dir.join("libcheck.c"),
        &out_dir.join("libcheck.o"),
        &[&inc],
    );
    let libcheck_elf = clang_link(
        &[&crt0_o, &libc_o, &libcheck_o],
        &out_dir.join("libcheck"),
        &linker,
    );
    println!(
        "cargo:rustc-env=USER_LIBCHECK_ELF={}",
        libcheck_elf.display()
    );

    // M9l: программа `catfile` поверх libc — читает реальный файл с диска (open/read/write).
    let catfile_o = clang_compile_c(
        &c_dir.join("catfile.c"),
        &out_dir.join("catfile.o"),
        &[&inc],
    );
    let catfile_elf = clang_link(
        &[&crt0_o, &libc_o, &catfile_o],
        &out_dir.join("catfile"),
        &linker,
    );
    println!("cargo:rustc-env=USER_CATFILE_ELF={}", catfile_elf.display());

    // M9m: `ccat` — утилита cat на C поверх libc. НЕ встраивается в ядро: кладётся на диск в /BIN
    // (см. generate_disk_image), shell запускает её как обычный coreutil. Собираем в OUT_DIR;
    // переменную окружения не выставляем — образ диска прочитает ELF из OUT_DIR.
    let ccat_o = clang_compile_c(&c_dir.join("ccat.c"), &out_dir.join("ccat.o"), &[&inc]);
    clang_link(&[&crt0_o, &libc_o, &ccat_o], &out_dir.join("ccat"), &linker);

    // M9r: `wc` — утилита подсчёта строк/слов/байт на C поверх libc (getopt + FILE*). Тоже на диск
    // в /BIN (см. generate_disk_image), shell запускает как coreutil; env-переменную не выставляем.
    let wc_o = clang_compile_c(&c_dir.join("wc.c"), &out_dir.join("wc.o"), &[&inc]);
    clang_link(&[&crt0_o, &libc_o, &wc_o], &out_dir.join("wc"), &linker);

    // M8d3: `dnsclient` — резолвит имя по DNS через сокет-сисколлы (socket/sendto/recvfrom).
    let dns_o = clang_compile_c(
        &c_dir.join("dnsclient.c"),
        &out_dir.join("dnsclient.o"),
        &[&inc],
    );
    let dns_elf = clang_link(
        &[&crt0_o, &libc_o, &dns_o],
        &out_dir.join("dnsclient"),
        &linker,
    );
    println!("cargo:rustc-env=USER_DNSCLIENT_ELF={}", dns_elf.display());

    // M8e: `tcpdns` — резолвит имя по DNS поверх TCP (socket/connect/send/recv).
    let tcpdns_o = clang_compile_c(&c_dir.join("tcpdns.c"), &out_dir.join("tcpdns.o"), &[&inc]);
    let tcpdns_elf = clang_link(
        &[&crt0_o, &libc_o, &tcpdns_o],
        &out_dir.join("tcpdns"),
        &linker,
    );
    println!("cargo:rustc-env=USER_TCPDNS_ELF={}", tcpdns_elf.display());

    // M8f: `httpget` — резолвит example.com по DNS и тянет страницу по HTTP (TCP) из кольца 3.
    let http_o = clang_compile_c(
        &c_dir.join("httpget.c"),
        &out_dir.join("httpget.o"),
        &[&inc],
    );
    let http_elf = clang_link(
        &[&crt0_o, &libc_o, &http_o],
        &out_dir.join("httpget"),
        &linker,
    );
    println!("cargo:rustc-env=USER_HTTPGET_ELF={}", http_elf.display());

    // M9n: `stdiotest` — round-trip через FILE* (fopen/fputs/fprintf/fgets/fgetc).
    let stdio_o = clang_compile_c(
        &c_dir.join("stdiotest.c"),
        &out_dir.join("stdiotest.o"),
        &[&inc],
    );
    let stdio_elf = clang_link(
        &[&crt0_o, &libc_o, &stdio_o],
        &out_dir.join("stdiotest"),
        &linker,
    );
    println!("cargo:rustc-env=USER_STDIOTEST_ELF={}", stdio_elf.display());
}

/// Общие флаги компиляции C для пользовательского таргета ferros (см. [`build_c_programs`]).
const CLANG_C_FLAGS: &[&str] = &[
    "--target=x86_64-unknown-linux-gnu",
    "-ffreestanding",
    "-nostdlib",
    "-fno-pie",
    "-fno-stack-protector",
    "-fno-asynchronous-unwind-tables",
    "-mno-red-zone",
    "-mcmodel=large",
    "-O2",
    "-c",
];

/// Компилирует C-файл `src` в объектник `obj` (+ доп. флаги `extra`, например `-I`). Возвращает `obj`.
fn clang_compile_c(src: &std::path::Path, obj: &std::path::Path, extra: &[&str]) -> PathBuf {
    let ok = Command::new("clang")
        .args(CLANG_C_FLAGS)
        .args(extra)
        .arg(src)
        .arg("-o")
        .arg(obj)
        .status()
        .expect("failed to run clang (compile C)")
        .success();
    assert!(ok, "clang failed to compile {}", src.display());
    obj.to_path_buf()
}

/// Ассемблирует `.s`-файл `src` в объектник `obj` (синтаксис задаёт директива в файле). Возвращает `obj`.
fn clang_assemble(src: &std::path::Path, obj: &std::path::Path) -> PathBuf {
    let ok = Command::new("clang")
        .args(["--target=x86_64-unknown-linux-gnu", "-c"])
        .arg(src)
        .arg("-o")
        .arg(obj)
        .status()
        .expect("failed to run clang (assemble)")
        .success();
    assert!(ok, "clang failed to assemble {}", src.display());
    obj.to_path_buf()
}

/// Линкует объектники `objs` в статический ELF `elf` по скрипту компоновки `linker`. Возвращает `elf`.
///
/// Скрипт передаём через `-Xlinker -T -Xlinker <путь>` (два отдельных токена), а НЕ `-Wl,-T,<путь>`:
/// `clang` режет `-Wl,` по запятым, и путь с запятой сломал бы поиск скрипта (пробелы — ок).
fn clang_link(
    objs: &[&std::path::Path],
    elf: &std::path::Path,
    linker: &std::path::Path,
) -> PathBuf {
    let ok = Command::new("clang")
        .args([
            "--target=x86_64-unknown-linux-gnu",
            "-nostdlib",
            "-static",
            "-fno-pie",
            "-fuse-ld=lld",
            "-Xlinker",
            "-T",
            "-Xlinker",
        ])
        .arg(linker)
        .args(objs)
        .arg("-o")
        .arg(elf)
        .status()
        .expect("failed to run clang (link)")
        .success();
    assert!(ok, "clang/lld failed to link {}", elf.display());
    elf.to_path_buf()
}

/// Имя и содержимое тестового файла в образе. ВАЖНО: те же значения захардкожены в
/// `tests/fat_read.rs` (отдельный крейт — общую константу не пошарить); менять оба места.
const FAT_TEST_FILE: &str = "HELLO.TXT";
const FAT_TEST_CONTENT: &[u8] = b"ferros M6c: hello from FAT32!\n";

/// Версия содержимого образа (пишется в BS_VolID при форматировании). Бамп при изменении
/// набора файлов/содержимого → образ пересоздаётся, хотя размер прежний. v3: ARGVECHO (M7b);
/// v4: каталог SUB + SUB/INSIDE.TXT (M7c); v5: каталог /BIN с coreutils (M7f); v6: cat читает
/// stdin (M7g1) — нужен новый бинарь /BIN/CAT; v7: свежий образ (тесты редиректов пишут в /SUB,
/// чтобы не переполнять корневой каталог — у FAT нет роста каталога); v8: /BIN/RM + /BIN/RMDIR (M7g3);
/// v9: /BIN/CCAT — утилита cat на C поверх libc (M9m); v10: /BIN/WC — утилита wc на C (M9r).
const DISK_VERSION: u32 = 10;

/// Создаёт тестовый образ диска (M6c/M6f2): форматирует его как **FAT32** и кладёт тестовый
/// файл плюс пользовательские ELF-программы (для `execve` по пути — M6f2). Образ —
/// `target/ferros-disk.img` (относительно корня воркспейса, где запускается QEMU; `target/`
/// в .gitignore). `fatfs` — только build-зависимость (хост-инструмент создания фикстуры).
///
/// Идемпотентно: не переписываем, если образ уже нужного размера, это FAT (сигнатура 0x55AA)
/// и его BS_VolID == `DISK_VERSION`.
fn generate_disk_image(manifest: &str) {
    use std::io::Write;
    const DISK_SIZE: u64 = 64 * 1024 * 1024; // 64 МиБ — хватает на FAT32

    let disk = PathBuf::from(manifest)
        .join("target")
        .join("ferros-disk.img");
    if disk_image_is_current(&disk, DISK_SIZE) {
        return;
    }

    let mut image = vec![0u8; DISK_SIZE as usize];

    // Форматируем буфер как FAT32. В fatfs 0.3 с фичей `std` его трейты реализованы для
    // std::io-типов (`Cursor` подходит напрямую, обёртка не нужна). `volume_id` несёт версию.
    {
        let cursor = std::io::Cursor::new(&mut image);
        fatfs::format_volume(
            cursor,
            fatfs::FormatVolumeOptions::new()
                .fat_type(fatfs::FatType::Fat32)
                .bytes_per_sector(512)
                .volume_id(DISK_VERSION),
        )
        .expect("failed to format FAT32 image");
    }
    // Монтируем и кладём файлы (Drop у FileSystem сбрасывает изменения в буфер).
    {
        let cursor = std::io::Cursor::new(&mut image);
        let fs = fatfs::FileSystem::new(cursor, fatfs::FsOptions::new())
            .expect("failed to mount FAT32 image");
        let root = fs.root_dir();

        let mut file = root
            .create_file(FAT_TEST_FILE)
            .expect("failed to create test file");
        file.write_all(FAT_TEST_CONTENT)
            .expect("failed to write test file");
        file.flush().expect("failed to flush test file");
        drop(file);

        // Программа `hello` на диске под `execve` (имя 8.3 `HELLO`). Когда понадобится больше
        // программ — вынести в список.
        let hello_elf = PathBuf::from(manifest).join("user/hello/target/x86_64-user/release/hello");
        let hello_bytes = std::fs::read(&hello_elf)
            .unwrap_or_else(|e| panic!("read {} for disk image: {e}", hello_elf.display()));
        let mut prog = root
            .create_file("HELLO")
            .expect("create HELLO on disk image");
        prog.write_all(&hello_bytes)
            .expect("write HELLO on disk image");
        prog.flush().expect("flush HELLO on disk image");

        // Программа `argvecho` на диске под `execve` с аргументами (имя 8.3 `ARGVECHO`, M7b).
        let argvecho_elf =
            PathBuf::from(manifest).join("user/hello/target/x86_64-user/release/argvecho");
        let argvecho_bytes = std::fs::read(&argvecho_elf)
            .unwrap_or_else(|e| panic!("read {} for disk image: {e}", argvecho_elf.display()));
        let mut prog = root
            .create_file("ARGVECHO")
            .expect("create ARGVECHO on disk image");
        prog.write_all(&argvecho_bytes)
            .expect("write ARGVECHO on disk image");
        prog.flush().expect("flush ARGVECHO on disk image");

        // Каталог SUB с файлом INSIDE.TXT под проверку cwd (M7c): относительный open из /SUB.
        let sub = root
            .create_dir("SUB")
            .expect("create SUB dir on disk image");
        let mut inside = sub
            .create_file("INSIDE.TXT")
            .expect("create SUB/INSIDE.TXT on disk image");
        inside
            .write_all(b"inside SUB\n")
            .expect("write SUB/INSIDE.TXT on disk image");
        inside.flush().expect("flush SUB/INSIDE.TXT on disk image");

        // Каталог /BIN с coreutils (M7f): shell ищет голые команды в /bin. Имена 8.3 заглавными
        // (без LFN), чтобы наш FAT-читатель сопоставлял их без обработки длинных имён.
        let bin = root
            .create_dir("BIN")
            .expect("create BIN dir on disk image");
        for (file, src) in [
            ("ECHO", "echo"),
            ("CAT", "cat"),
            ("LS", "ls"),
            ("MKDIR", "mkdir"),
            ("RM", "rm"),
            ("RMDIR", "rmdir"),
        ] {
            let elf_path = PathBuf::from(manifest)
                .join(format!("user/hello/target/x86_64-user/release/{src}"));
            let bytes = std::fs::read(&elf_path)
                .unwrap_or_else(|e| panic!("read {} for disk image: {e}", elf_path.display()));
            let mut f = bin
                .create_file(file)
                .unwrap_or_else(|e| panic!("create BIN/{file} on disk image: {e}"));
            f.write_all(&bytes)
                .unwrap_or_else(|e| panic!("write BIN/{file} on disk image: {e}"));
            f.flush()
                .unwrap_or_else(|e| panic!("flush BIN/{file} on disk image: {e}"));
        }

        // C-coreutil `ccat` (M9m): ELF собран build_c_programs в OUT_DIR. Имя 8.3 заглавными.
        let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set for disk image");
        let ccat_elf = PathBuf::from(&out_dir).join("ccat");
        let ccat_bytes = std::fs::read(&ccat_elf)
            .unwrap_or_else(|e| panic!("read {} for disk image: {e}", ccat_elf.display()));
        let mut f = bin
            .create_file("CCAT")
            .expect("create BIN/CCAT on disk image");
        f.write_all(&ccat_bytes)
            .expect("write BIN/CCAT on disk image");
        f.flush().expect("flush BIN/CCAT on disk image");

        // C-coreutil `wc` (M9r): тоже из OUT_DIR на диск как /BIN/WC (8.3 заглавными).
        let wc_elf = PathBuf::from(&out_dir).join("wc");
        let wc_bytes = std::fs::read(&wc_elf)
            .unwrap_or_else(|e| panic!("read {} for disk image: {e}", wc_elf.display()));
        let mut f = bin.create_file("WC").expect("create BIN/WC on disk image");
        f.write_all(&wc_bytes).expect("write BIN/WC on disk image");
        f.flush().expect("flush BIN/WC on disk image");
    }

    if let Some(parent) = disk.parent() {
        std::fs::create_dir_all(parent).expect("failed to create target dir for disk image");
    }
    std::fs::write(&disk, &image).expect("failed to write ferros-disk.img");
}

/// Уже ли на месте актуальный FAT-образ: нужного размера, с сигнатурой загрузсектора 0x55AA
/// и BS_VolID == [`DISK_VERSION`] (FAT32 хранит VolID по смещению 0x43).
fn disk_image_is_current(path: &std::path::Path, size: u64) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    match file.metadata() {
        Ok(meta) if meta.len() == size => {}
        _ => return false,
    }
    let mut boot = [0u8; 512];
    if file.read_exact(&mut boot).is_err() || boot[510] != 0x55 || boot[511] != 0xAA {
        return false;
    }
    let vol_id = u32::from_le_bytes([boot[0x43], boot[0x44], boot[0x45], boot[0x46]]);
    vol_id == DISK_VERSION
}
