//! Системно-информационные вызовы (M9e): `uname` (а идентификаторы `getuid`/`getppid`/… —
//! тривиальные арм-ы прямо в диспетчере [`crate::syscall::dispatch`]).
//!
//! `uname(buf)` заполняет линуксовый `struct utsname` — шесть полей `char[65]`, идущих подряд
//! (`sysname`, `nodename`, `release`, `version`, `machine`, `domainname`; итого 390 байт). Каждое
//! поле — C-строка с завершающим нулём; буфер занулён, поэтому достаточно записать сами строки.

use super::uaccess;

/// Длина одного поля `struct utsname` (Linux: `__NEW_UTS_LEN` 64 + завершающий нуль).
const UTS_FIELD: usize = 65;
/// Полей в `struct utsname`: sysname, nodename, release, version, machine, domainname.
const UTS_FIELDS: usize = 6;
/// Размер `struct utsname` целиком.
const UTSNAME_SIZE: usize = UTS_FIELD * UTS_FIELDS;

/// `uname(buf)` (M9e): пишет `struct utsname` в память пользователя. Значения фиксированы (имя ОС,
/// «версия», архитектура). `nodename`/`domainname` условны — настоящих имени хоста и домена нет.
pub fn sys_uname(buf: u64) -> i64 {
    // sysname, nodename, release, version, machine, domainname — по порядку полей utsname.
    let fields = [
        "ferros",           // sysname
        "ferros",           // nodename (имя хоста — фиксировано)
        "0.1.0",            // release
        "ferros M9 x86_64", // version
        "x86_64",           // machine
        "(none)",           // domainname
    ];
    let mut out = [0u8; UTSNAME_SIZE];
    for (i, s) in fields.iter().enumerate() {
        let bytes = s.as_bytes();
        // Усечь до 64 байт (на завершающий нуль остаётся место — буфер занулён).
        let n = bytes.len().min(UTS_FIELD - 1);
        out[i * UTS_FIELD..i * UTS_FIELD + n].copy_from_slice(&bytes[..n]);
    }
    match uaccess::copy_to_user(buf, &out) {
        Ok(()) => 0,
        Err(errno) => -errno,
    }
}
