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
    let binaries = [("hello", "USER_HELLO_ELF"), ("faulter", "USER_FAULTER_ELF")];

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

/// Создаёт raw-образ диска для virtio-blk (M6a): тесту перечисления PCI нужно подключённое
/// устройство, а M6b прочитает сектор 0 и сверит сигнатуру. Образ — `target/ferros-disk.img`
/// (путь относительно корня воркспейса, где QEMU и запускается; `target/` в .gitignore).
///
/// Идемпотентно: не переписываем, если образ уже нужного размера и с нашей сигнатурой.
fn generate_disk_image(manifest: &str) {
    const DISK_SIZE: u64 = 4 * 1024 * 1024; // 4 МиБ
    const SIGNATURE: &[u8] = b"FERROSM6"; // 8 байт в начале сектора 0

    let disk = PathBuf::from(manifest)
        .join("target")
        .join("ferros-disk.img");
    if disk_image_current(&disk, DISK_SIZE, SIGNATURE) {
        return;
    }

    let mut image = vec![0u8; DISK_SIZE as usize];
    image[..SIGNATURE.len()].copy_from_slice(SIGNATURE);
    // Заметный паттерн в остатке сектора 0 — чтобы M6b проверял не только сигнатуру.
    for (i, byte) in image[SIGNATURE.len()..512].iter_mut().enumerate() {
        *byte = (i as u8).wrapping_mul(3).wrapping_add(1);
    }

    if let Some(parent) = disk.parent() {
        std::fs::create_dir_all(parent).expect("failed to create target dir for disk image");
    }
    std::fs::write(&disk, &image).expect("failed to write ferros-disk.img");
}

/// Уже ли на месте диск-образ нужного размера с нашей сигнатурой в начале.
fn disk_image_current(path: &std::path::Path, size: u64, signature: &[u8]) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    match file.metadata() {
        Ok(meta) if meta.len() == size => {}
        _ => return false,
    }
    // Читаем ровно длину сигнатуры (не фиксированный буфер) — чтобы проверка не разъехалась,
    // если сигнатуру когда-нибудь изменят по длине.
    let mut head = vec![0u8; signature.len()];
    file.read_exact(&mut head).is_ok() && head == signature
}
