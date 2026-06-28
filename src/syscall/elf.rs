//! Минимальный загрузчик ELF64 (M5c1): разбирает статический исполняемый ELF и загружает
//! его сегменты в память кольца 3.
//!
//! # Что такое ELF и что мы из него берём
//!
//! ELF — стандартный формат исполняемых файлов (его выдаёт линкер для Linux и для нашей
//! пользовательской программы). Нам нужны лишь две вещи: точка входа `e_entry` (откуда
//! начать исполнение) и **сегменты загрузки** `PT_LOAD` из таблицы программных заголовков
//! — каждый говорит «скопируй вот эти байты файла по вот этому виртуальному адресу». Всё
//! остальное (секции, символы, отладка) для запуска не нужно.
//!
//! # Наш случай (узко и намеренно)
//!
//! Грузим ТОЛЬКО статический `ET_EXEC` под x86-64 с фиксированными адресами (без релокаций
//! и динамической линковки) — ровно то, что собирает `user/hello`. Сегменты кладём в
//! общее (ядровое) адресное пространство как пользовательские страницы; изоляция адресных
//! пространств — позже (см. ROADMAP M5c2). Разбор оборонительный (всё в границах файла),
//! хотя бинарь сейчас наш собственный и заведомо корректный.

use crate::arch::USER_SPACE_END;
use alloc::collections::BTreeSet;
use x86_64::structures::paging::{FrameAllocator, Mapper, OffsetPageTable, Page, Size4KiB};
use x86_64::VirtAddr;

/// Встроенная в образ ядра пользовательская программа «hello» (её ELF собирает `build.rs`
/// из крейта `user/hello` и кладёт путь в `USER_HELLO_ELF`).
pub static HELLO_ELF: &[u8] = include_bytes!(env!("USER_HELLO_ELF"));

/// Встроенная программа-«фолтер» (M5c3b): намеренно падает в кольце 3 — для проверки, что
/// ядро завершает процесс, а не падает само.
pub static FAULTER_ELF: &[u8] = include_bytes!(env!("USER_FAULTER_ELF"));

/// Встроенная программа-«читатель» (M6d2): открывает и читает файл с диска через файловые
/// системные вызовы.
pub static READER_ELF: &[u8] = include_bytes!(env!("USER_READER_ELF"));

/// Встроенная проверка `getpid` (M6f1): завершается со своим PID как кодом возврата.
pub static GETPIDTEST_ELF: &[u8] = include_bytes!(env!("USER_GETPIDTEST_ELF"));

/// Встроенная проверка `execve` (M6f2): заменяет себя программой `HELLO` с диска.
pub static EXECTEST_ELF: &[u8] = include_bytes!(env!("USER_EXECTEST_ELF"));

/// Встроенная проверка `fork` (M6f3): форкается; ребёнок и родитель выходят с разными кодами.
pub static FORKTEST_ELF: &[u8] = include_bytes!(env!("USER_FORKTEST_ELF"));

/// Встроенная проверка `fork`+`wait4` (M6f4): родитель дожидается ребёнка и проверяет его статус.
pub static WAITTEST_ELF: &[u8] = include_bytes!(env!("USER_WAITTEST_ELF"));

/// Встроенная проверка `kill` (M6f5): родитель убивает ребёнка SIGTERM и проверяет статус.
pub static KILLTEST_ELF: &[u8] = include_bytes!(env!("USER_KILLTEST_ELF"));

/// Встроенная проверка записи файлов (M6g3): создаёт/пишет/читает файл через сисколлы.
pub static WRITETEST_ELF: &[u8] = include_bytes!(env!("USER_WRITETEST_ELF"));

/// Встроенная проверка листинга каталога (M6g5): читает корень через `getdents64`.
pub static LSTEST_ELF: &[u8] = include_bytes!(env!("USER_LSTEST_ELF"));

/// Встроенная проверка stdin (M7a): читает строку через `read(0)` и печатает её обратно.
pub static STDINTEST_ELF: &[u8] = include_bytes!(env!("USER_STDINTEST_ELF"));

/// Встроенный запускатель (M7b): `execve("ARGVECHO", ["ARGVECHO","ping","pong"])` — проверяет
/// передачу argv через execve. Целевой `ARGVECHO` лежит на диске.
pub static EXECARGV_ELF: &[u8] = include_bytes!(env!("USER_EXECARGV_ELF"));

/// Почему ELF не удалось загрузить.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    /// Файл короче, чем нужно для заголовков.
    TooSmall,
    /// Нет сигнатуры `\x7FELF`.
    BadMagic,
    /// Не 64-битный ELF (EI_CLASS ≠ 2).
    NotElf64,
    /// Не статический исполняемый (e_type ≠ ET_EXEC).
    NotExecutable,
    /// Не для x86-64 (e_machine ≠ 0x3E).
    NotX86_64,
    /// Размер программного заголовка меньше ELF64-минимума (56 байт).
    BadProgramHeaderSize,
    /// Сегмент выходит за пределы файла.
    SegmentOutOfFile,
    /// `p_filesz > p_memsz` — некорректный сегмент (иначе обнуление `.bss` ушло бы в минус).
    FileSizeExceedsMemSize,
    /// Сегмент целится вне пользовательской половины адресного пространства.
    SegmentNotInUserSpace,
    /// Страница сегмента уже отображена чем-то посторонним (не нашей загрузкой) — не затираем.
    SegmentOverlapsExisting,
}

const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 0x3E;
const PT_LOAD: u32 = 1;
const PH_ENTRY_SIZE: usize = 56; // размер программного заголовка ELF64

// Чтение little-endian полей с проверкой границ (любой выход за файл → TooSmall).
fn read_u16(bytes: &[u8], off: usize) -> Result<u16, ElfError> {
    let s = bytes.get(off..off + 2).ok_or(ElfError::TooSmall)?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}
fn read_u32(bytes: &[u8], off: usize) -> Result<u32, ElfError> {
    let s = bytes.get(off..off + 4).ok_or(ElfError::TooSmall)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn read_u64(bytes: &[u8], off: usize) -> Result<u64, ElfError> {
    let s = bytes.get(off..off + 8).ok_or(ElfError::TooSmall)?;
    Ok(u64::from_le_bytes([
        s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
    ]))
}

/// Разбирает и загружает статический ELF64-исполняемый: маппит его `PT_LOAD`-сегменты как
/// пользовательские страницы в активной таблице, копирует туда содержимое файла и обнуляет
/// `.bss`. Возвращает точку входа (`e_entry`).
///
/// Маппинг идёт в активную таблицу (`mapper`), поэтому после маппинга мы (кольцо 0) можем
/// прямо писать по виртуальным адресам сегментов.
pub fn load(
    bytes: &[u8],
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<u64, ElfError> {
    // --- Заголовок ELF64 ---
    if bytes.len() < 64 {
        return Err(ElfError::TooSmall);
    }
    if &bytes[0..4] != b"\x7FELF" {
        return Err(ElfError::BadMagic);
    }
    if bytes[4] != 2 {
        return Err(ElfError::NotElf64); // EI_CLASS = ELFCLASS64
    }
    if read_u16(bytes, 0x10)? != ET_EXEC {
        return Err(ElfError::NotExecutable);
    }
    if read_u16(bytes, 0x12)? != EM_X86_64 {
        return Err(ElfError::NotX86_64);
    }

    let entry = read_u64(bytes, 0x18)?;
    let phoff = read_u64(bytes, 0x20)? as usize;
    let phentsize = read_u16(bytes, 0x36)? as usize;
    let phnum = read_u16(bytes, 0x38)? as usize;
    if phentsize < PH_ENTRY_SIZE {
        return Err(ElfError::BadProgramHeaderSize);
    }
    // Вся таблица программных заголовков обязана лежать в файле — тогда чтения внутри
    // цикла заведомо в границах.
    let ph_table_end = phoff
        .checked_add(phnum.checked_mul(phentsize).ok_or(ElfError::TooSmall)?)
        .ok_or(ElfError::TooSmall)?;
    if ph_table_end > bytes.len() {
        return Err(ElfError::TooSmall);
    }

    // --- Загрузка сегментов PT_LOAD ---
    // Страницы, отображённые ИМЕННО этой загрузкой: два сегмента могут поделить граничную
    // страницу — её надо пропустить (а не маппить дважды). Если же страница занята не нами
    // (например, отображена ядром в общем адресном пространстве процесса), это коллизия —
    // ошибка, а не «тихо затереть». Куча к этому моменту поднята (см. вызов в M5c2).
    let mut mapped: BTreeSet<u64> = BTreeSet::new();
    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        if read_u32(bytes, ph)? != PT_LOAD {
            continue;
        }
        let p_offset = read_u64(bytes, ph + 0x08)? as usize;
        let p_vaddr = read_u64(bytes, ph + 0x10)?;
        let p_filesz = read_u64(bytes, ph + 0x20)? as usize;
        let p_memsz = read_u64(bytes, ph + 0x28)?;

        // Содержимое сегмента обязано лежать в файле.
        let file_end = p_offset
            .checked_add(p_filesz)
            .ok_or(ElfError::SegmentOutOfFile)?;
        if file_end > bytes.len() {
            return Err(ElfError::SegmentOutOfFile);
        }
        // По спеку filesz ≤ memsz; иначе обнуление .bss (memsz-filesz) ушло бы в
        // переполнение usize, а copy(filesz) — за пределы отображённых под memsz страниц.
        if p_filesz > p_memsz as usize {
            return Err(ElfError::FileSizeExceedsMemSize);
        }
        // Сегмент обязан целиться в пользовательскую половину.
        let mem_end = p_vaddr
            .checked_add(p_memsz)
            .ok_or(ElfError::SegmentNotInUserSpace)?;
        if mem_end > USER_SPACE_END {
            return Err(ElfError::SegmentNotInUserSpace);
        }
        if p_memsz == 0 {
            continue;
        }

        // Маппим все страницы, покрывающие [p_vaddr, p_vaddr+p_memsz).
        let first = Page::<Size4KiB>::containing_address(VirtAddr::new(p_vaddr));
        let last = Page::<Size4KiB>::containing_address(VirtAddr::new(mem_end - 1));
        for page in Page::range_inclusive(first, last) {
            let start = page.start_address().as_u64();
            if mapped.contains(&start) {
                continue; // уже отобразили в этой загрузке (поделённая граничная страница)
            }
            if mapper.translate_page(page).is_ok() {
                // Занята не нами (ядро / посторонний маппинг) — не затираем.
                return Err(ElfError::SegmentOverlapsExisting);
            }
            crate::mm::paging::map_user_page(page, mapper, frame_allocator);
            mapped.insert(start);
        }

        // SAFETY: страницы [p_vaddr, mem_end) только что отображены present+writable+user в
        // активной таблице, мы в кольце 0 → можем писать. Диапазон-источник [p_offset,
        // p_offset+p_filesz) проверен в границах `bytes`.
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytes.as_ptr().add(p_offset),
                p_vaddr as *mut u8,
                p_filesz,
            );
            // .bss: байты за filesz до memsz обнуляем.
            let bss = p_memsz as usize - p_filesz;
            if bss > 0 {
                core::ptr::write_bytes((p_vaddr as *mut u8).add(p_filesz), 0, bss);
            }
        }
    }

    Ok(entry)
}
