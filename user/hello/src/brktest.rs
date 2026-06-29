//! Проверка `brk` (M9a): растит кучу, пишет и читает её, сжимает (страницы снимаются), растит
//! снова (страницы переотображаются) — и проверяет инварианты, включая **обнуление** свежей
//! памяти. Встраивается в ядро; тест `tests/brk.rs` запускает её и ждёт код выхода 0. Любое
//! несовпадение — свой ненулевой код (10..21), чтобы по нему было видно, какой шаг сломался.

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;
use core::ptr::{read_volatile, write_volatile};

const SYS_EXIT: u64 = 60;
const SYS_BRK: u64 = 12;

global_asm!(".global _start", "_start:", "    call main");

/// `brk(addr)` — вернуть новый/текущий разрыв кучи. Адреса кучи < 2^47, поэтому i64→u64 тождественно.
fn brk(addr: u64) -> u64 {
    let ret: i64;
    // SAFETY: единственный аргумент — адрес; ядро вернёт разрыв в rax.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") SYS_BRK => ret,
            in("rdi") addr,
            lateout("rcx") _, lateout("r11") _,
        );
    }
    ret as u64
}

/// # Safety
/// Не возвращается.
unsafe fn sys_exit(code: u64) -> ! {
    asm!("syscall", in("rax") SYS_EXIT, in("rdi") code, options(noreturn));
}

/// Проверяет, что `[ptr, ptr+len)` целиком нулевое (свежая память кучи). `false` — нашёлся
/// ненулевой байт.
///
/// # Safety
/// `[ptr, ptr+len)` должно быть отображено на чтение (вызывающий обеспечил это ростом `brk`).
unsafe fn all_zero(ptr: u64, len: usize) -> bool {
    let p = ptr as *const u8;
    for i in 0..len {
        // SAFETY: i < len, диапазон отображён на чтение.
        if unsafe { read_volatile(p.add(i)) } != 0 {
            return false;
        }
    }
    true
}

/// Записать байтовый паттерн `[ptr, ptr+len)` и тут же сверить чтением. `false` — несовпадение
/// (страница не отображена/не пишется). `byte(i)` даёт ожидаемое значение для смещения `i`.
///
/// # Safety
/// `[ptr, ptr+len)` должно быть отображено на запись (вызывающий обеспечил это ростом `brk`).
unsafe fn fill_and_check(ptr: u64, len: usize, byte: impl Fn(usize) -> u8) -> bool {
    let p = ptr as *mut u8;
    for i in 0..len {
        // SAFETY: i < len, диапазон отображён на запись.
        unsafe { write_volatile(p.add(i), byte(i)) };
    }
    for i in 0..len {
        // SAFETY: тот же отображённый диапазон.
        if unsafe { read_volatile(p.add(i)) } != byte(i) {
            return false;
        }
    }
    true
}

#[no_mangle]
extern "C" fn main() -> ! {
    let pat = |i: usize| (i & 0xff) as u8;

    // SAFETY: все обращения к памяти ниже идут только по диапазонам, которые мы перед этим
    // отобразили ростом `brk` (и не трогаем то, что сняли сжатием).
    unsafe {
        // brk(0) — текущий разрыв = база кучи: ненулевой и выровненный по странице.
        let base = brk(0);
        if base == 0 {
            sys_exit(10);
        }
        if base & 0xFFF != 0 {
            sys_exit(11);
        }

        // Рост на 2 страницы: brk должен вернуть запрошенный адрес.
        if brk(base + 8192) != base + 8192 {
            sys_exit(12);
        }
        // Свежая память кучи обязана быть нулевой (как в Linux).
        if !all_zero(base, 8192) {
            sys_exit(20);
        }
        // Вся выросшая область пишется и читается.
        if !fill_and_check(base, 8192, pat) {
            sys_exit(13);
        }

        // Сжатие на страницу: оставшаяся [base, base+4096) по-прежнему хранит данные.
        if brk(base + 4096) != base + 4096 {
            sys_exit(14);
        }
        if !fill_and_check(base, 4096, pat) {
            sys_exit(15);
        }

        // Сжатие до базы — куча пуста.
        if brk(base) != base {
            sys_exit(16);
        }

        // Повторный рост на страницу: фрейм переотображается, область снова пишется/читается —
        // доказывает, что снятые сжатием страницы корректно отдаются и берутся заново.
        if brk(base + 4096) != base + 4096 {
            sys_exit(17);
        }
        // Самая показательная проверка обнуления: этот фрейм только что освобождён сжатием (в нём
        // лежал паттерн `pat`); при переотображении ядро обязано его обнулить, иначе кольцо 3
        // увидело бы старые данные.
        if !all_zero(base, 4096) {
            sys_exit(21);
        }
        if !fill_and_check(base, 4096, |_| 0xAB) {
            sys_exit(18);
        }

        // brk(0) теперь = текущий разрыв (base+4096).
        if brk(0) != base + 4096 {
            sys_exit(19);
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
