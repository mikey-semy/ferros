//! Интеграционный тест M6d2: пользовательский процесс читает файл с диска через сисколлы.
//!
//! Спавним программу «reader»: она `open("HELLO.TXT")` → `read` → `write(1, …)` → `exit(0)`.
//! Файл лежит на FAT32-диске (его записал build.rs). Тест ждёт завершения процесса и
//! проверяет по наблюдаемости ядра, что reader записал в stdout **ровно содержимое файла**
//! (значит open+read через VFS→FAT→virtio-blk и copy-to-user отработали) и вышел с кодом 0.
//!
//! Почему это сквозное доказательство: длина/сумма последнего `write` совпадут с файлом,
//! только если процесс реально открыл файл, прочитал его байты с диска и получил их в свой
//! буфер.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ferros::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use core::sync::atomic::Ordering;
use ferros::arch::x86_64::syscall::spawn_user;
use ferros::mm::frame::BootInfoFrameAllocator;
use ferros::mm::{heap, paging};
use ferros::sched::thread;
use ferros::syscall::elf::READER_ELF;
use ferros::syscall::{EXIT_CALLS, LAST_EXIT_CODE, LAST_WRITE_FD, LAST_WRITE_LEN, LAST_WRITE_SUM};
use x86_64::VirtAddr;

entry_point!(main);

/// То же содержимое, что build.rs пишет в `HELLO.TXT` (отдельный крейт — константу не
/// пошарить; держать синхронно с `FAT_TEST_CONTENT` в build.rs).
const EXPECTED: &[u8] = b"ferros M6c: hello from FAT32!\n";

fn main(boot_info: &'static BootInfo) -> ! {
    ferros::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    // SAFETY: оффсет от bootloader корректен; init вызывается один раз.
    let mut mapper = unsafe { paging::init(phys_mem_offset) };
    // SAFETY: карта памяти валидна, Usable-регионы свободны.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    heap::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");

    // Диск virtio-blk должен подняться, иначе reader не сможет открыть файл.
    assert!(
        ferros::drivers::virtio_blk::init(phys_mem_offset, &mut frame_allocator),
        "virtio-blk not initialized"
    );

    thread::init();
    // SAFETY: phys_mem_offset корректен, куча поднята; адреса процесса свободны у ядра.
    unsafe { spawn_user(READER_ELF, phys_mem_offset, &mut frame_allocator) };
    thread::start_preemption();

    // Крутимся на «нулевом» потоке, пока reader не отработает и не выйдет.
    let mut spins = 0u64;
    while EXIT_CALLS.load(Ordering::SeqCst) == 0 {
        spins += 1;
        assert!(spins < 5_000_000_000, "reader process never exited");
        core::hint::spin_loop();
    }
    thread::stop_preemption();

    test_main();
    ferros::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    ferros::test_panic_handler(info)
}

/// Reader открыл и прочитал файл и записал в stdout его содержимое, затем вышел с кодом 0.
#[test_case]
fn reader_reads_file_via_syscalls() {
    // Последний `write` от reader — в stdout (fd=1), ровно содержимое файла.
    assert_eq!(
        LAST_WRITE_FD.load(Ordering::SeqCst),
        1,
        "write was not to stdout"
    );
    assert_eq!(
        LAST_WRITE_LEN.load(Ordering::SeqCst) as usize,
        EXPECTED.len(),
        "written length != file length (open/read failed?)"
    );
    let expected_sum: u64 = EXPECTED.iter().map(|&b| b as u64).sum();
    assert_eq!(
        LAST_WRITE_SUM.load(Ordering::SeqCst),
        expected_sum,
        "written bytes != file content"
    );
    // И процесс завершился штатно (open≥0, read≥0 → exit(0)).
    assert_eq!(
        LAST_EXIT_CODE.load(Ordering::SeqCst),
        0,
        "reader exited with an error code"
    );
}
