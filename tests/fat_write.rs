//! Интеграционный тест M6g2: создание/перезапись файлов на FAT32 и чтение их обратно.
//!
//! Работаем ТОЛЬКО со своими файлами (`NEWFILE.TXT`, `BIGFILE.BIN`), не трогая `HELLO.TXT`/
//! программу `HELLO`, — чтобы не сломать другие тесты, читающие образ. Файлы остаются на образе
//! после теста (удаления пока нет), но повторный прогон их просто перезаписывает.
//!
//! Проверяем три случая: создать новый файл и прочитать; перезаписать его содержимым другого
//! размера (старая цепочка кластеров освобождается); записать файл больше одного кластера и
//! прочитать целиком (выделение и связывание цепочки кластеров).
//!
//! Почему это доказательство: чтобы прочитать записанное, надо верно выделить свободные
//! кластеры, связать цепочку в FAT, создать/обновить запись каталога (первый кластер + размер)
//! и записать данные по секторам — любая ошибка даст не тот размер/содержимое или промах.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::vec::Vec;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};
use ferros::drivers::virtio_blk;
use ferros::fs::fat::Fat32;
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::{heap, paging};
use x86_64::VirtAddr;

entry_point!(main);

static CREATE_OK: AtomicBool = AtomicBool::new(false);
static OVERWRITE_OK: AtomicBool = AtomicBool::new(false);
static MULTICLUSTER_OK: AtomicBool = AtomicBool::new(false);

/// Пишет файл, читает обратно и сверяет с ожидаемым.
fn roundtrip(fs: &Fat32, name: &str, content: &[u8]) -> bool {
    if fs.write_file(name, content).is_err() {
        return false;
    }
    match fs.read_file(name) {
        Ok(back) => back.as_slice() == content,
        Err(_) => false,
    }
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

    // 1) Создать новый небольшой файл и прочитать обратно.
    CREATE_OK.store(
        roundtrip(&fs, "NEWFILE.TXT", b"created by ferros M6g2\n"),
        Ordering::SeqCst,
    );

    // 2) Перезаписать тот же файл другим (более коротким) содержимым: старая цепочка должна
    //    освободиться, новое содержимое — прочитаться без «хвоста» прежнего.
    OVERWRITE_OK.store(
        roundtrip(&fs, "NEWFILE.TXT", b"shorter\n"),
        Ordering::SeqCst,
    );

    // 3) Файл больше одного кластера: содержимое — узор, чтобы поймать перепутанные кластеры.
    let mut big = Vec::new();
    for i in 0..20_000usize {
        big.push((i as u8) ^ 0x5A);
    }
    MULTICLUSTER_OK.store(roundtrip(&fs, "BIGFILE.BIN", &big), Ordering::SeqCst);

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

#[test_case]
fn create_then_read() {
    assert!(
        CREATE_OK.load(Ordering::SeqCst),
        "creating a new file and reading it back failed"
    );
}

#[test_case]
fn overwrite_shrinks_and_reads() {
    assert!(
        OVERWRITE_OK.load(Ordering::SeqCst),
        "overwriting a file with shorter content failed"
    );
}

#[test_case]
fn multicluster_roundtrips() {
    assert!(
        MULTICLUSTER_OK.load(Ordering::SeqCst),
        "writing/reading a multi-cluster file failed"
    );
}
