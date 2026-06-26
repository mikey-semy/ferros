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
        "Cargo.toml",
        "linker.ld",
        "x86_64-user.json",
        ".cargo/config.toml",
    ] {
        println!("cargo:rerun-if-changed={user_dir}/{f}");
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
    ] {
        cmd.env_remove(var);
    }

    let status = cmd
        .status()
        .expect("failed to run `cargo build` for user/hello");
    assert!(status.success(), "building user/hello failed");

    // Абсолютный путь к собранному ELF (через CARGO_MANIFEST_DIR ядра — чистый путь без
    // префикса \\?\, который даёт canonicalize на Windows).
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let elf = PathBuf::from(manifest)
        .join(user_dir)
        .join("target/x86_64-user/release/hello");
    assert!(
        elf.exists(),
        "user ELF not found after build: {}",
        elf.display()
    );
    println!("cargo:rustc-env=USER_HELLO_ELF={}", elf.display());
}
