//! Проверка времени (M9d): `clock_gettime`/`gettimeofday`/`time`. Встраивается в ядро; тест
//! `tests/time.rs` запускает её и ждёт код выхода 0. Любое несовпадение — свой код (10..16).
//!
//! Что доказываем: структуры заполнены корректно (наносекунды < 1e9, микросекунды < 1e6); время
//! **идёт вперёд** — два чтения `CLOCK_MONOTONIC` с ожиданием между ними дают возрастание (значит
//! таймер реально тикает и uptime растёт); `time()` возвращает то же значение, что пишет в `*tloc`.

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

const SYS_GETTIMEOFDAY: u64 = 96;
const SYS_TIME: u64 = 201;
const SYS_CLOCK_GETTIME: u64 = 228;
const SYS_EXIT: u64 = 60;

const CLOCK_MONOTONIC: u64 = 1;

const NS_PER_SEC: u64 = 1_000_000_000;
const US_PER_SEC: u64 = 1_000_000;

global_asm!(".global _start", "_start:", "    call main");

/// Сисколл с одним аргументом (`time`).
fn sc1(nr: u64, a: u64) -> i64 {
    let ret: i64;
    // SAFETY: один аргумент в rdi; rcx/r11 — затираемые syscall'ом.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") nr => ret,
            in("rdi") a,
            lateout("rcx") _, lateout("r11") _,
        );
    }
    ret
}

/// Сисколл с двумя аргументами (`clock_gettime`/`gettimeofday`).
fn sc2(nr: u64, a: u64, b: u64) -> i64 {
    let ret: i64;
    // SAFETY: аргументы в rdi/rsi; rcx/r11 — затираемые syscall'ом.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") nr => ret,
            in("rdi") a, in("rsi") b,
            lateout("rcx") _, lateout("r11") _,
        );
    }
    ret
}

/// # Safety
/// Не возвращается.
unsafe fn sys_exit(code: u64) -> ! {
    asm!("syscall", in("rax") SYS_EXIT, in("rdi") code, options(noreturn));
}

/// Читает 64-битное поле по смещению `off` из 16-байтной структуры времени.
fn field(buf: &[u8; 16], off: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&buf[off..off + 8]);
    u64::from_le_bytes(b)
}

/// Полное время структуры в наносекундах (sec*1e9 + frac), frac в тех же единицах, что поле@8 —
/// здесь используется только для `timespec`, поэтому frac уже в наносекундах.
fn total_ns(buf: &[u8; 16]) -> u64 {
    field(buf, 0) * NS_PER_SEC + field(buf, 8)
}

#[no_mangle]
extern "C" fn main() -> ! {
    let mut ts1 = [0u8; 16];

    // SAFETY: указатели — на локальные буферы достаточного размера.
    unsafe {
        // 1) clock_gettime(CLOCK_MONOTONIC): наносекунды должны быть в пределах [0, 1e9).
        if sc2(SYS_CLOCK_GETTIME, CLOCK_MONOTONIC, ts1.as_mut_ptr() as u64) != 0 {
            sys_exit(10);
        }
        if field(&ts1, 8) >= NS_PER_SEC {
            sys_exit(11);
        }
        let start = total_ns(&ts1);

        // 2) Время идёт вперёд: читаем CLOCK_MONOTONIC, пока оно не превысит первое чтение. Таймер
        //    тикает в фоне (~18 Гц), поэтому пары итераций хватит; верхняя граница — на случай
        //    сломанного таймера (тогда выходим с ошибкой, а не висим).
        let mut advanced = false;
        let mut i = 0u64;
        while i < 500_000_000 {
            let mut ts2 = [0u8; 16];
            if sc2(SYS_CLOCK_GETTIME, CLOCK_MONOTONIC, ts2.as_mut_ptr() as u64) == 0
                && total_ns(&ts2) > start
            {
                advanced = true;
                break;
            }
            i += 1;
        }
        if !advanced {
            sys_exit(12);
        }

        // 3) gettimeofday: микросекунды в пределах [0, 1e6).
        let mut tv = [0u8; 16];
        if sc2(SYS_GETTIMEOFDAY, tv.as_mut_ptr() as u64, 0) != 0 {
            sys_exit(13);
        }
        if field(&tv, 8) >= US_PER_SEC {
            sys_exit(14);
        }

        // 4) time(): возвращает секунды и пишет их же в *tloc.
        let mut t: i64 = -1;
        let ret = sc1(SYS_TIME, &mut t as *mut i64 as u64);
        if ret < 0 {
            sys_exit(15);
        }
        if t != ret {
            sys_exit(16);
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
