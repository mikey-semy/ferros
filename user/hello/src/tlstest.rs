//! Проверка TLS через `arch_prctl(ARCH_SET_FS)` (M9b). Встраивается в ядро; тест `tests/tls.rs`
//! запускает её и ждёт код выхода 0. Любое несовпадение — свой код (10..14).
//!
//! Что доказываем:
//! 1. `ARCH_SET_FS` ставит базу сегмента FS — чтение `fs:[0]` достаёт байты по этому адресу.
//! 2. `ARCH_GET_FS` возвращает ту же базу.
//! 3. База FS **переживает переключения контекста**: в цикле читаем `fs:[0]` много раз; пока мы
//!    крутимся, вытеснение по таймеру уводит CPU на «нулевой» поток ядра (у него FS-база 0) и
//!    обратно — если бы планировщик не восстанавливал нашу базу, `fs:[0]` начал бы читать не
//!    оттуда. Все чтения обязаны давать наше значение.

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;
use core::ptr::{addr_of_mut, write_volatile};

const SYS_EXIT: u64 = 60;
const SYS_ARCH_PRCTL: u64 = 158;
const ARCH_SET_FS: u64 = 0x1002;
const ARCH_GET_FS: u64 = 0x1003;

/// Узнаваемое значение в блоке TLS — его и должны вернуть чтения через FS.
const MARK: u64 = 0x1122_3344_5566_7788;

/// Блок TLS программы (8 байт). База FS будет указывать сюда, поэтому `fs:[0]` читает это слово.
static mut TLS: u64 = 0;

global_asm!(".global _start", "_start:", "    call main");

/// `arch_prctl(code, addr)`.
fn arch_prctl(code: u64, addr: u64) -> i64 {
    let ret: i64;
    // SAFETY: два аргумента в rdi/rsi; rcx/r11 — затираемые syscall'ом.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") SYS_ARCH_PRCTL => ret,
            in("rdi") code, in("rsi") addr,
            lateout("rcx") _, lateout("r11") _,
        );
    }
    ret
}

/// Читает 8 байт по `fs:[0]` (база сегмента FS + смещение 0). Без `nomem`/`pure`, чтобы компилятор
/// не вынес чтение из цикла и не считал его инвариантным — каждое чтение должно идти заново.
fn read_fs0() -> u64 {
    let v: u64;
    // SAFETY: база FS установлена на наш блок TLS, чтение 8 байт по ней корректно.
    unsafe { asm!("mov {0}, qword ptr fs:[0]", out(reg) v) };
    v
}

/// # Safety
/// Не возвращается.
unsafe fn sys_exit(code: u64) -> ! {
    asm!("syscall", in("rax") SYS_EXIT, in("rdi") code, options(noreturn));
}

#[no_mangle]
extern "C" fn main() -> ! {
    let tls_addr = addr_of_mut!(TLS) as u64;

    // SAFETY: пишем своё значение в блок TLS и далее читаем его же через FS.
    unsafe {
        write_volatile(addr_of_mut!(TLS), MARK);

        // 1) Поставить базу FS на блок TLS и прочитать его через сегмент.
        if arch_prctl(ARCH_SET_FS, tls_addr) != 0 {
            sys_exit(10);
        }
        if read_fs0() != MARK {
            sys_exit(11);
        }

        // 2) ARCH_GET_FS возвращает ту же базу.
        let mut got: u64 = 0;
        if arch_prctl(ARCH_GET_FS, addr_of_mut!(got) as u64) != 0 {
            sys_exit(12);
        }
        if got != tls_addr {
            sys_exit(13);
        }

        // 3) База переживает переключения. Ключ: планировщик пишет `IA32_FS_BASE` БЕЗУСЛОВНО, в
        //    т.ч. 0 при переключении на «нулевой» поток ядра. Поэтому пока мы крутим цикл, таймер
        //    уводит CPU на поток ядра (он зануляет FS-базу) и возвращает обратно — и если бы базу
        //    НЕ восстанавливали при переключении на нас, `fs:[0]` читал бы от базы 0 (→ не MARK,
        //    обычно #PF/мусор). Каждое чтение обязано видеть наше значение.
        //
        //    Зависимость теста: цикл должен охватить хотя бы один тик таймера, иначе переключения
        //    не случится. PIT тикает ~18.2 Гц (~55 мс/тик, см. arch/x86_64/interrupts.rs); под
        //    QEMU TCG (без KVM — так гоняет CI) 5e6 чтений `fs:[0]` идут много дольше 55 мс, т.е.
        //    охватывают десятки тиков с большим запасом. На быстрой эмуляции (KVM) цикл надо будет
        //    сделать тик-ограниченным (когда появится сисколл времени, M9c).
        let mut i = 0u64;
        while i < 5_000_000 {
            if read_fs0() != MARK {
                sys_exit(14);
            }
            i += 1;
        }

        sys_exit(0)
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
