//! Интеграционный тест M6c: монтируем FAT32 на диске virtio-blk и читаем файл.
//!
//! `build.rs` форматирует образ как FAT32 и кладёт файл `HELLO.TXT` с известным содержимым.
//! `main` поднимает диск, монтирует FAT, читает файл и сверяет байты. Проверка идёт в `main`
//! (нужен аллокатор фреймов), результат — в статиках, а `#[test_case]` их утверждает.
//!
//! Почему это доказательство: чтобы прочитать файл, надо верно разобрать BPB, найти запись в
//! корневом каталоге и пройти цепочку кластеров по таблице FAT — любая ошибка даст не тот
//! размер/содержимое или промах.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec::Vec;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use ferros::drivers::virtio_blk;
use ferros::fs::fat::Fat32;
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::{heap, paging};
use spin::Mutex;
use x86_64::VirtAddr;

entry_point!(main);

/// То же содержимое, что build.rs пишет в `HELLO.TXT` (отдельный крейт — константу не
/// пошарить; держать синхронно с `FAT_TEST_CONTENT` в build.rs).
const EXPECTED: &[u8] = b"ferros M6c: hello from FAT32!\n";

/// FAT смонтировался и файл прочитан без ошибки.
static READ_OK: AtomicBool = AtomicBool::new(false);
/// Длина прочитанного файла.
static READ_LEN: AtomicUsize = AtomicUsize::new(0);
/// Прочитанное содержимое (для сверки в тест-кейсе).
static CONTENT: Mutex<Vec<u8>> = Mutex::new(Vec::new());

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init вызывается один раз.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");

    assert!(
        virtio_blk::init(phys_mem_offset, &mut frame_allocator),
        "virtio-blk not initialized"
    );
    if let Ok(fs) = Fat32::mount() {
        if let Ok(data) = fs.read_file("HELLO.TXT") {
            READ_LEN.store(data.len(), Ordering::SeqCst);
            *CONTENT.lock() = data;
            READ_OK.store(true, Ordering::SeqCst);
        }
    }

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// Файл прочитан с диска через FAT32 и совпадает по длине и содержимому с тем, что записал
/// build.rs.
#[test_case]
fn reads_hello_txt() {
    assert!(READ_OK.load(Ordering::SeqCst), "mount or read_file failed");
    assert_eq!(
        READ_LEN.load(Ordering::SeqCst),
        EXPECTED.len(),
        "file length mismatch"
    );
    assert!(
        CONTENT.lock().as_slice() == EXPECTED,
        "file content mismatch"
    );
}
