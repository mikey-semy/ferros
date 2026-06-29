//! Системные вызовы времени (M9d): `clock_gettime`, `gettimeofday`, `time`.
//!
//! Источник — uptime от таймера PIT ([`crate::arch::uptime_ns`]): наносекунды с загрузки. Реального
//! стенного времени (RTC) пока нет, поэтому отсчёт идёт от 0 на момент загрузки — для `CLOCK_REALTIME`
//! это «неправильно» (не 1970), но монотонно и достаточно для таймаутов/относительных интервалов
//! (см. HARDENING). Разрешение — один тик таймера (~54.9 мс).
//!
//! Структуры пишем в память пользователя по точным смещениям ABI (как `struct stat` в M9c):
//! `struct timespec { i64 tv_sec @0; i64 tv_nsec @8 }`, `struct timeval { i64 tv_sec @0; i64 tv_usec @8 }`.

use super::{abi, uaccess};

const NS_PER_SEC: u64 = 1_000_000_000;
const NS_PER_USEC: u64 = 1_000;

/// Сериализует пару 64-битных полей (sec, frac) в 16-байтную структуру `timespec`/`timeval`.
fn pack_time(sec: u64, frac: u64) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[0..8].copy_from_slice(&(sec as i64).to_le_bytes());
    b[8..16].copy_from_slice(&(frac as i64).to_le_bytes());
    b
}

/// `clock_gettime(clk_id, tp)` (M9d): пишет время часов в `struct timespec` (сек+нс). `CLOCK_REALTIME`
/// и `CLOCK_MONOTONIC` дают один и тот же uptime (RTC нет); другие часы — `-EINVAL`.
pub fn sys_clock_gettime(clk_id: u64, tp: u64) -> i64 {
    if clk_id != abi::CLOCK_REALTIME && clk_id != abi::CLOCK_MONOTONIC {
        return -abi::EINVAL;
    }
    let ns = crate::arch::uptime_ns();
    let ts = pack_time(ns / NS_PER_SEC, ns % NS_PER_SEC);
    match uaccess::copy_to_user(tp, &ts) {
        Ok(()) => 0,
        Err(errno) => -errno,
    }
}

/// `gettimeofday(tv, tz)` (M9d): пишет время в `struct timeval` (сек+мкс). `tz` (часовой пояс)
/// игнорируем, как и современный Linux. `tv == NULL` — ничего не пишем, успех.
pub fn sys_gettimeofday(tv: u64, _tz: u64) -> i64 {
    if tv == 0 {
        return 0;
    }
    let ns = crate::arch::uptime_ns();
    let buf = pack_time(ns / NS_PER_SEC, (ns % NS_PER_SEC) / NS_PER_USEC);
    match uaccess::copy_to_user(tv, &buf) {
        Ok(()) => 0,
        Err(errno) => -errno,
    }
}

/// `time(tloc)` (M9d): возвращает секунды текущего времени и, если `tloc != NULL`, пишет их туда же
/// (`time_t` = i64). При ошибке записи — `-errno`.
pub fn sys_time(tloc: u64) -> i64 {
    let sec = crate::arch::uptime_ns() / NS_PER_SEC;
    if tloc != 0 {
        if let Err(errno) = uaccess::copy_to_user(tloc, &(sec as i64).to_le_bytes()) {
            return -errno;
        }
    }
    sec as i64
}
