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
    ];

    // Удаляем прошлые ELF перед сборкой: cargo не отслеживает linker.ld / target.json как
    // входы, поэтому при их изменении сам бы не перелинковал. Удаление принуждает к
    // (быстрой) перелинковке, подхватывающей текущий скрипт/таргет. .o-файлы кэшируются,
    // так что core/alloc не пересобираются.
    let out_dir = format!("{user_dir}/target/x86_64-user/release");
    for (bin, _) in binaries {
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

    generate_disk_image(&manifest);
}

/// Имя и содержимое тестового файла в образе. ВАЖНО: те же значения захардкожены в
/// `tests/fat_read.rs` (отдельный крейт — общую константу не пошарить); менять оба места.
const FAT_TEST_FILE: &str = "HELLO.TXT";
const FAT_TEST_CONTENT: &[u8] = b"ferros M6c: hello from FAT32!\n";

/// Создаёт тестовый образ диска (M6c): форматирует его как **FAT32** и кладёт один файл.
/// Образ — `target/ferros-disk.img` (путь относительно корня воркспейса, где QEMU и
/// запускается; `target/` в .gitignore). QEMU подключает его как virtio-blk; ядро читает FAT
/// своим кодом. `fatfs` — только build-зависимость (хост-инструмент создания фикстуры).
///
/// Размер 64 МиБ: FAT32 требует ≥65525 кластеров, в 4 МиБ не помещается. Идемпотентно: не
/// переписываем, если образ уже нужного размера и это FAT (сигнатура загрузсектора 0x55AA).
fn generate_disk_image(manifest: &str) {
    use std::io::Write;
    const DISK_SIZE: u64 = 64 * 1024 * 1024; // 64 МиБ — хватает на FAT32

    let disk = PathBuf::from(manifest)
        .join("target")
        .join("ferros-disk.img");
    if disk_image_is_fat(&disk, DISK_SIZE) {
        return;
    }

    let mut image = vec![0u8; DISK_SIZE as usize];

    // Форматируем буфер как FAT32. В fatfs 0.3 с фичей `std` его трейты реализованы для
    // std::io-типов (`Cursor` подходит напрямую, обёртка не нужна).
    {
        let cursor = std::io::Cursor::new(&mut image);
        fatfs::format_volume(
            cursor,
            fatfs::FormatVolumeOptions::new()
                .fat_type(fatfs::FatType::Fat32)
                .bytes_per_sector(512),
        )
        .expect("failed to format FAT32 image");
    }
    // Монтируем и кладём тестовый файл (Drop у FileSystem сбрасывает изменения в буфер).
    {
        let cursor = std::io::Cursor::new(&mut image);
        let fs = fatfs::FileSystem::new(cursor, fatfs::FsOptions::new())
            .expect("failed to mount FAT32 image");
        let mut file = fs
            .root_dir()
            .create_file(FAT_TEST_FILE)
            .expect("failed to create test file");
        file.write_all(FAT_TEST_CONTENT)
            .expect("failed to write test file");
        file.flush().expect("failed to flush test file");
    }

    if let Some(parent) = disk.parent() {
        std::fs::create_dir_all(parent).expect("failed to create target dir for disk image");
    }
    std::fs::write(&disk, &image).expect("failed to write ferros-disk.img");
}

/// Уже ли на месте FAT-образ нужного размера (по сигнатуре загрузочного сектора 0x55AA).
fn disk_image_is_fat(path: &std::path::Path, size: u64) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    match file.metadata() {
        Ok(meta) if meta.len() == size => {}
        _ => return false,
    }
    let mut boot = [0u8; 512];
    file.read_exact(&mut boot).is_ok() && boot[510] == 0x55 && boot[511] == 0xAA
}
