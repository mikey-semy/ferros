//! Интеграционный тест M6g4: подкаталоги FAT32 — `mkdir`, запись/чтение файла по пути с
//! подкаталогами, вложенность и отказ при спуске через файл.
//!
//! Работаем со своими каталогами/файлами (`SUBDIR/...`), не трогая корневые `HELLO.TXT`/`HELLO`.
//! `mkdir` идемпотентен для теста: повторный прогон видит уже существующий каталог (через образ,
//! который сохраняется между запусками) и просто пишет в него.
//!
//! Почему это доказательство: чтобы прочитать файл по пути `SUBDIR/INNER/DEEP.TXT`, надо
//! разобрать путь, спуститься по цепочкам кластеров двух подкаталогов, найти запись и пройти её
//! цепочку — любая ошибка адресации/обхода даст промах или не то содержимое.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};
use ferros::drivers::virtio_blk;
use ferros::fs::fat::{Fat32, FatError};
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::{heap, paging};
use x86_64::VirtAddr;

entry_point!(main);

static SUBDIR_OK: AtomicBool = AtomicBool::new(false);
static NESTED_OK: AtomicBool = AtomicBool::new(false);
static NOTDIR_REJECTED: AtomicBool = AtomicBool::new(false);

/// Гарантирует существование каталога: создаёт его или принимает «уже есть».
fn ensure_dir(fs: &Fat32, path: &str) -> bool {
    matches!(fs.mkdir(path), Ok(()) | Err(FatError::AlreadyExists))
}

/// Пишет файл по пути, читает обратно и сверяет.
fn roundtrip(fs: &Fat32, path: &str, content: &[u8]) -> bool {
    fs.write_file(path, content).is_ok() && fs.read_file(path).is_ok_and(|d| d == content)
}

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init один раз.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");

    assert!(
        virtio_blk::init(phys_mem_offset, &mut frame_allocator),
        "virtio-blk not initialized"
    );

    let fs = Fat32::mount().expect("mount FAT32");

    // 1) Создаём подкаталог и пишем/читаем в нём файл по пути.
    SUBDIR_OK.store(
        ensure_dir(&fs, "SUBDIR") && roundtrip(&fs, "SUBDIR/SUB.TXT", b"file in a subdir\n"),
        Ordering::SeqCst,
    );

    // 2) Вложенный подкаталог: SUBDIR/INNER, и файл в нём (двойной спуск по пути).
    NESTED_OK.store(
        ensure_dir(&fs, "SUBDIR/INNER")
            && roundtrip(&fs, "SUBDIR/INNER/DEEP.TXT", b"two levels deep\n"),
        Ordering::SeqCst,
    );

    // 3) Спуск через файл должен отвергаться: HELLO.TXT — файл, не каталог.
    NOTDIR_REJECTED.store(fs.read_file("HELLO.TXT/x").is_err(), Ordering::SeqCst);

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

#[test_case]
fn file_in_subdir_roundtrips() {
    assert!(
        SUBDIR_OK.load(Ordering::SeqCst),
        "mkdir SUBDIR + write/read SUBDIR/SUB.TXT failed"
    );
}

#[test_case]
fn nested_subdir_roundtrips() {
    assert!(
        NESTED_OK.load(Ordering::SeqCst),
        "nested SUBDIR/INNER + write/read DEEP.TXT failed"
    );
}

#[test_case]
fn descend_through_file_is_rejected() {
    assert!(
        NOTDIR_REJECTED.load(Ordering::SeqCst),
        "resolving a path through a non-directory should fail"
    );
}
