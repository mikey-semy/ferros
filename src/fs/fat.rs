//! Чтение **FAT32** поверх блочного устройства (M6c).
//!
//! # Что такое FAT
//!
//! FAT (File Allocation Table) — простая файловая система: диск разбит на **кластеры**
//! (по несколько секторов), а отдельная таблица — FAT — для каждого кластера хранит номер
//! *следующего* кластера файла (или метку «конец цепочки»). Файл — это цепочка кластеров,
//! связанная через FAT, как односвязный список. Каталог — особый файл из 32-байтных записей
//! (имя 8.3, атрибуты, первый кластер, размер).
//!
//! # Раскладка тома (FAT32)
//!
//! `[ зарезервированные секторы | FAT × N | область данных (кластеры от №2) ]`. Параметры
//! берём из **BPB** (BIOS Parameter Block) в секторе 0. Корневой каталог в FAT32 — обычная
//! цепочка кластеров, начинающаяся с `BPB_RootClus`.
//!
//! # Узко и намеренно (как загрузчик ELF, D9/D10)
//!
//! Только **чтение**, только **FAT32**, имена **8.3** (без длинных имён LFN), размер сектора
//! 512 (как у нашего virtio-blk). Чтение идёт по одному сектору через
//! [`crate::drivers::virtio_blk::read_sector`] — без кэша (для bring-up достаточно).

use crate::drivers::virtio_blk::{self, BlkError, SECTOR_SIZE};
use alloc::vec::Vec;

/// Маска значащих бит записи FAT32 (старшие 4 бита зарезервированы). Конец цепочки (EOC) —
/// значение ≥ 0x0FFFFFF8; такие (как и 0/1 и всё вне тома) отсекает [`Fat32::valid_cluster`].
const FAT32_ENTRY_MASK: u32 = 0x0FFF_FFFF;
/// Первый «настоящий» кластер данных (0 и 1 зарезервированы).
const FIRST_DATA_CLUSTER: u32 = 2;
/// Размер одной записи каталога (байт).
const DIR_ENTRY_SIZE: usize = 32;
/// Атрибут «длинное имя» (LFN): такие записи пропускаем.
const ATTR_LONG_NAME: u8 = 0x0F;
/// Атрибут «метка тома»: тоже пропускаем.
const ATTR_VOLUME_ID: u8 = 0x08;

/// Почему операция с FAT не удалась.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatError {
    /// Ошибка чтения блочного устройства.
    Block(BlkError),
    /// Размер сектора в BPB не 512 (мы поддерживаем только 512).
    UnsupportedSectorSize,
    /// Том не похож на FAT32 (ненулевой корневой каталог FAT12/16 или нулевой размер FAT).
    NotFat32,
    /// Файл не найден в корневом каталоге.
    NotFound,
}

impl From<BlkError> for FatError {
    fn from(e: BlkError) -> Self {
        FatError::Block(e)
    }
}

/// Смонтированный том FAT32: всё, что нужно для адресации (из BPB).
pub struct Fat32 {
    /// Секторов в кластере.
    sectors_per_cluster: u32,
    /// Сектор, с которого начинается первая FAT.
    fat_start_sector: u32,
    /// Сектор, с которого начинается область данных (кластер №2).
    data_start_sector: u32,
    /// Номер первого кластера корневого каталога.
    root_cluster: u32,
    /// Число кластеров данных в томе (для проверки границ: валидны номера
    /// `2 .. 2 + cluster_count`). Защищает от переполнения арифметики адреса сектора и от
    /// «диких» номеров в повреждённой FAT.
    cluster_count: u32,
}

/// Найденная запись каталога (то, что нам нужно для чтения файла).
struct DirEntry {
    first_cluster: u32,
    size: u32,
}

fn read_u16(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([buf[off], buf[off + 1]])
}
fn read_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

/// Преобразует имя вида `"HELLO.TXT"` в 11-байтную форму 8.3 (`b"HELLO   TXT"`,
/// заглавными, дополнено пробелами) — так имена хранятся в записи каталога.
fn short_name_83(name: &str) -> [u8; 11] {
    let mut out = [b' '; 11];
    let mut parts = name.splitn(2, '.');
    let base = parts.next().unwrap_or("");
    let ext = parts.next().unwrap_or("");
    for (i, c) in base.bytes().take(8).enumerate() {
        out[i] = c.to_ascii_uppercase();
    }
    for (i, c) in ext.bytes().take(3).enumerate() {
        out[8 + i] = c.to_ascii_uppercase();
    }
    out
}

impl Fat32 {
    /// Читает сектор 0, разбирает BPB и возвращает смонтированный том. Ошибка, если это не
    /// FAT32 или размер сектора не 512.
    pub fn mount() -> Result<Fat32, FatError> {
        let mut boot = [0u8; SECTOR_SIZE];
        virtio_blk::read_sector(0, &mut boot)?;

        let bytes_per_sector = read_u16(&boot, 11);
        if bytes_per_sector as usize != SECTOR_SIZE {
            return Err(FatError::UnsupportedSectorSize);
        }
        let sectors_per_cluster = boot[13] as u32;
        let reserved_sectors = read_u16(&boot, 14) as u32;
        let num_fats = boot[16] as u32;
        let root_entry_count = read_u16(&boot, 17); // FAT32: 0
        let fat_size_16 = read_u16(&boot, 22); // FAT32: 0
        let total_sectors = read_u32(&boot, 32); // FAT32: общее число секторов (TotSec32)
        let fat_size_32 = read_u32(&boot, 36); // FAT32: размер одной FAT в секторах
        let root_cluster = read_u32(&boot, 44);

        // Признаки FAT32: корневой каталог не фиксированной длины (root_entry_count == 0),
        // размер FAT — в 32-битном поле. Также отвергаем заведомо битую геометрию.
        if root_entry_count != 0
            || fat_size_16 != 0
            || fat_size_32 == 0
            || sectors_per_cluster == 0
            || num_fats == 0
        {
            return Err(FatError::NotFat32);
        }

        let data_start_sector = reserved_sectors + num_fats * fat_size_32;
        // Сколько кластеров данных в томе — для проверки границ номеров кластеров.
        let cluster_count = total_sectors.saturating_sub(data_start_sector) / sectors_per_cluster;

        Ok(Fat32 {
            sectors_per_cluster,
            fat_start_sector: reserved_sectors,
            data_start_sector,
            root_cluster,
            cluster_count,
        })
    }

    /// Валиден ли номер кластера: в пределах области данных тома. Отсекает 0/1, метку EOC и
    /// «дикие» значения из повреждённой FAT — значит арифметика адреса сектора не переполнится.
    fn valid_cluster(&self, cluster: u32) -> bool {
        cluster >= FIRST_DATA_CLUSTER && cluster < FIRST_DATA_CLUSTER + self.cluster_count
    }

    /// Первый сектор кластера `cluster` (предполагается валидным — см. [`Self::valid_cluster`]).
    fn first_sector_of_cluster(&self, cluster: u32) -> u32 {
        self.data_start_sector + (cluster - FIRST_DATA_CLUSTER) * self.sectors_per_cluster
    }

    /// Следующий кластер цепочки по таблице FAT (или значение ≥ EOC — конец).
    fn next_cluster(&self, cluster: u32) -> Result<u32, FatError> {
        // Запись для кластера — 4 байта по смещению cluster*4 от начала FAT.
        let fat_offset = cluster * 4;
        let sector = self.fat_start_sector + fat_offset / SECTOR_SIZE as u32;
        let offset = (fat_offset % SECTOR_SIZE as u32) as usize;
        let mut buf = [0u8; SECTOR_SIZE];
        virtio_blk::read_sector(sector as u64, &mut buf)?;
        Ok(read_u32(&buf, offset) & FAT32_ENTRY_MASK)
    }

    /// Ищет файл `name` в каталоге, начинающемся с кластера `start_cluster`.
    fn find_in_dir(&self, start_cluster: u32, name: &str) -> Result<Option<DirEntry>, FatError> {
        let target = short_name_83(name);
        let mut cluster = start_cluster;
        // Ограничиваем число пройденных кластеров: цепочка не может быть длиннее всех
        // кластеров тома — иначе это цикл в повреждённой FAT (иначе зациклились бы).
        let mut steps_left = self.cluster_count;
        while self.valid_cluster(cluster) && steps_left > 0 {
            steps_left -= 1;
            let first = self.first_sector_of_cluster(cluster);
            for s in 0..self.sectors_per_cluster {
                let mut buf = [0u8; SECTOR_SIZE];
                virtio_blk::read_sector((first + s) as u64, &mut buf)?;
                for entry in buf.chunks_exact(DIR_ENTRY_SIZE) {
                    match entry[0] {
                        0x00 => return Ok(None), // конец каталога — дальше пусто
                        0xE5 => continue,        // удалённая запись
                        _ => {}
                    }
                    let attr = entry[11];
                    if attr & ATTR_LONG_NAME == ATTR_LONG_NAME || attr & ATTR_VOLUME_ID != 0 {
                        continue; // LFN-часть или метка тома
                    }
                    if entry[..11] == target {
                        let hi = read_u16(entry, 20) as u32;
                        let lo = read_u16(entry, 26) as u32;
                        return Ok(Some(DirEntry {
                            first_cluster: (hi << 16) | lo,
                            size: read_u32(entry, 28),
                        }));
                    }
                }
            }
            cluster = self.next_cluster(cluster)?;
        }
        Ok(None)
    }

    /// Читает файл `name` (формат 8.3, например `"HELLO.TXT"`) из корневого каталога и
    /// возвращает его содержимое. Идёт по цепочке кластеров, набирая ровно `size` байт.
    pub fn read_file(&self, name: &str) -> Result<Vec<u8>, FatError> {
        let entry = self
            .find_in_dir(self.root_cluster, name)?
            .ok_or(FatError::NotFound)?;

        let mut data = Vec::with_capacity(entry.size as usize);
        let mut remaining = entry.size as usize;
        let mut cluster = entry.first_cluster;
        while remaining > 0 && self.valid_cluster(cluster) {
            let first = self.first_sector_of_cluster(cluster);
            for s in 0..self.sectors_per_cluster {
                if remaining == 0 {
                    break;
                }
                let mut buf = [0u8; SECTOR_SIZE];
                virtio_blk::read_sector((first + s) as u64, &mut buf)?;
                let take = remaining.min(SECTOR_SIZE);
                data.extend_from_slice(&buf[..take]);
                remaining -= take;
            }
            cluster = self.next_cluster(cluster)?;
        }
        Ok(data)
    }
}
