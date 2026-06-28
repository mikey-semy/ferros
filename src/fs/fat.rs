//! Чтение и запись **FAT32** поверх блочного устройства (M6c; запись — M6g2).
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
//! Только **FAT32**, имена **8.3** (без длинных имён LFN), размер сектора 512 (как у нашего
//! virtio-blk). Ввод-вывод — по одному сектору через [`crate::drivers::virtio_blk`], без кэша
//! (для bring-up достаточно). Запись (M6g2) создаёт/перезаписывает файлы в КОРНЕВОМ каталоге:
//! выделяет свободные кластеры, связывает цепочку, обновляет запись каталога (зеркаля все копии
//! FAT). Подкаталоги, удаление, расширение каталога и кэш — позже (см. `docs/HARDENING.md`).

use crate::drivers::virtio_blk::{self, BlkError, SECTOR_SIZE};
use alloc::vec::Vec;

/// Записей FAT32 в одном секторе (по 4 байта).
const FAT_ENTRIES_PER_SECTOR: u32 = SECTOR_SIZE as u32 / 4;

/// Маска значащих бит записи FAT32 (старшие 4 бита зарезервированы). Конец цепочки (EOC) —
/// значение ≥ 0x0FFFFFF8; такие (как и 0/1 и всё вне тома) отсекает [`Fat32::valid_cluster`].
const FAT32_ENTRY_MASK: u32 = 0x0FFF_FFFF;
/// Значение «конец цепочки» (EOC), которое пишем в последний кластер файла. Любое ≥ 0x0FFFFFF8
/// читается как конец; [`Fat32::valid_cluster`] такой кластер отвергает.
const FAT32_EOC: u32 = 0x0FFF_FFFF;
/// Первый «настоящий» кластер данных (0 и 1 зарезервированы).
const FIRST_DATA_CLUSTER: u32 = 2;
/// Размер одной записи каталога (байт).
const DIR_ENTRY_SIZE: usize = 32;
/// Атрибут «длинное имя» (LFN): такие записи пропускаем.
const ATTR_LONG_NAME: u8 = 0x0F;
/// Атрибут «метка тома»: тоже пропускаем.
const ATTR_VOLUME_ID: u8 = 0x08;
/// Атрибут «каталог».
const ATTR_DIRECTORY: u8 = 0x10;
/// Атрибут «архив» (обычный файл).
const ATTR_ARCHIVE: u8 = 0x20;

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
    /// На томе нет свободных кластеров под запись.
    NoSpace,
    /// В каталоге нет свободной записи (расширение каталога пока не реализовано — M6g2).
    DirFull,
    /// Промежуточный компонент пути — не каталог (например, `a/b`, где `a` — файл).
    NotADirectory,
    /// Целевой путь — каталог, а ожидался файл (например, `read` каталога).
    IsADirectory,
    /// Запись с таким именем уже существует (например, `mkdir` существующего каталога).
    AlreadyExists,
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
    /// Сколько копий FAT на томе (обычно 2). Запись зеркалим во все — иначе образ будет
    /// несогласован для других читателей (наш читает только первую).
    num_fats: u32,
    /// Размер одной FAT в секторах (для адресации второй и последующих копий).
    fat_size: u32,
}

/// Найденная запись каталога (то, что нам нужно для чтения файла / обхода пути).
struct DirEntry {
    first_cluster: u32,
    size: u32,
    is_dir: bool,
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
            num_fats,
            fat_size: fat_size_32,
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
                            is_dir: attr & ATTR_DIRECTORY != 0,
                        }));
                    }
                }
            }
            cluster = self.next_cluster(cluster)?;
        }
        Ok(None)
    }

    /// Разбирает путь `/a/b/file` в `(кластер каталога-родителя, имя последнего компонента)`,
    /// спускаясь по промежуточным каталогам от корня. Ведущие/повторные `/` игнорируются.
    /// `-NotADirectory`, если промежуточный компонент оказался файлом; `-NotFound`, если его
    /// нет или путь пуст.
    fn resolve_parent<'a>(&self, path: &'a str) -> Result<(u32, &'a str), FatError> {
        let trimmed = path.trim_matches('/');
        let mut cluster = self.root_cluster;
        let mut name = "";
        let mut iter = trimmed.split('/').filter(|c| !c.is_empty()).peekable();
        while let Some(comp) = iter.next() {
            if iter.peek().is_none() {
                name = comp; // последний компонент — имя файла/каталога
                break;
            }
            // Промежуточный компонент обязан быть существующим каталогом.
            let entry = self.find_in_dir(cluster, comp)?.ok_or(FatError::NotFound)?;
            if !entry.is_dir {
                return Err(FatError::NotADirectory);
            }
            cluster = entry.first_cluster;
        }
        if name.is_empty() {
            return Err(FatError::NotFound); // путь без имени (например, "/")
        }
        Ok((cluster, name))
    }

    /// Читает файл по пути `path` (компоненты 8.3, например `"DIR/HELLO.TXT"`) и возвращает его
    /// содержимое. Спускается по подкаталогам, затем идёт по цепочке кластеров, набирая ровно
    /// `size` байт. `-IsADirectory`, если путь указывает на каталог.
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, FatError> {
        let (dir_cluster, name) = self.resolve_parent(path)?;
        let entry = self
            .find_in_dir(dir_cluster, name)?
            .ok_or(FatError::NotFound)?;
        if entry.is_dir {
            return Err(FatError::IsADirectory);
        }

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

    /// Пишет запись FAT для `cluster` = `value` (значащие 28 бит), сохраняя 4 старших
    /// зарезервированных бита. Зеркалит во ВСЕ копии FAT (read-modify-write по сектору каждой).
    fn write_fat_entry(&self, cluster: u32, value: u32) -> Result<(), FatError> {
        let fat_offset = cluster * 4;
        let within = fat_offset / SECTOR_SIZE as u32; // сектор внутри одной FAT
        let offset = (fat_offset % SECTOR_SIZE as u32) as usize;
        for fat in 0..self.num_fats {
            let sector = self.fat_start_sector + fat * self.fat_size + within;
            let mut buf = [0u8; SECTOR_SIZE];
            virtio_blk::read_sector(sector as u64, &mut buf)?;
            // Сохраняем старшие 4 бита (зарезервированы спекой), меняем значащие 28.
            let old = read_u32(&buf, offset);
            let new = (old & !FAT32_ENTRY_MASK) | (value & FAT32_ENTRY_MASK);
            buf[offset..offset + 4].copy_from_slice(&new.to_le_bytes());
            virtio_blk::write_sector(sector as u64, &buf)?;
        }
        Ok(())
    }

    /// Находит первый свободный кластер (запись FAT == 0), помечает его концом цепочки (EOC) и
    /// возвращает его номер. Сканирует FAT по секторам (а не по кластеру за раз). `-NoSpace`,
    /// если свободных нет.
    fn alloc_cluster(&self) -> Result<u32, FatError> {
        let last = FIRST_DATA_CLUSTER + self.cluster_count; // верхняя граница (исключительно)
        let mut cluster = FIRST_DATA_CLUSTER;
        while cluster < last {
            let sector = self.fat_start_sector + cluster / FAT_ENTRIES_PER_SECTOR;
            let mut buf = [0u8; SECTOR_SIZE];
            virtio_blk::read_sector(sector as u64, &mut buf)?;
            // Перебираем записи этого сектора, начиная с `cluster`.
            while cluster < last
                && cluster / FAT_ENTRIES_PER_SECTOR == (sector - self.fat_start_sector)
            {
                let off = (cluster % FAT_ENTRIES_PER_SECTOR) as usize * 4;
                if read_u32(&buf, off) & FAT32_ENTRY_MASK == 0 {
                    self.write_fat_entry(cluster, FAT32_EOC)?;
                    return Ok(cluster);
                }
                cluster += 1;
            }
        }
        Err(FatError::NoSpace)
    }

    /// Выделяет цепочку из `n` кластеров (n ≥ 1), связывает их и возвращает первый. Последний
    /// помечен EOC (его ставит [`Self::alloc_cluster`]). При нехватке места (`NoSpace`)
    /// освобождает уже выделенную часть — частичная цепочка не утекает.
    fn alloc_chain(&self, n: u32) -> Result<u32, FatError> {
        let mut first = 0u32;
        let mut prev = 0u32;
        for _ in 0..n {
            let c = match self.alloc_cluster() {
                Ok(c) => c,
                Err(e) => {
                    // Откат: возвращаем уже выделенную часть цепочки в пул.
                    if first != 0 {
                        let _ = self.free_chain(first);
                    }
                    return Err(e);
                }
            };
            if first == 0 {
                first = c;
            } else {
                self.write_fat_entry(prev, c)?; // prev → c (перетирает временный EOC у prev)
            }
            prev = c;
        }
        Ok(first)
    }

    /// Освобождает цепочку кластеров, начиная с `first` (каждой записи FAT ставит 0). Нужна при
    /// перезаписи файла — старая цепочка возвращается в пул. Ограничивает число шагов размером
    /// тома (защита от цикла в повреждённой FAT).
    fn free_chain(&self, first: u32) -> Result<(), FatError> {
        let mut cluster = first;
        let mut steps_left = self.cluster_count;
        while self.valid_cluster(cluster) && steps_left > 0 {
            steps_left -= 1;
            let next = self.next_cluster(cluster)?;
            self.write_fat_entry(cluster, 0)?;
            cluster = next;
        }
        Ok(())
    }

    /// Пишет `data` в цепочку, начинающуюся с `first` (она должна быть достаточно длинной).
    /// Последний неполный сектор дополняется нулями.
    fn write_chain(&self, first: u32, data: &[u8]) -> Result<(), FatError> {
        let mut cluster = first;
        let mut written = 0usize;
        let mut steps_left = self.cluster_count;
        while written < data.len() && self.valid_cluster(cluster) && steps_left > 0 {
            steps_left -= 1;
            let first_sec = self.first_sector_of_cluster(cluster);
            for s in 0..self.sectors_per_cluster {
                if written >= data.len() {
                    break;
                }
                let take = (data.len() - written).min(SECTOR_SIZE);
                let mut buf = [0u8; SECTOR_SIZE]; // нули → неполный сектор дополнен нулями
                buf[..take].copy_from_slice(&data[written..written + take]);
                virtio_blk::write_sector((first_sec + s) as u64, &buf)?;
                written += take;
            }
            cluster = self.next_cluster(cluster)?;
        }
        Ok(())
    }

    /// Местоположение записи каталога под имя `target`: возвращает `(сектор, смещение,
    /// старый первый кластер)`. `Some(first)` — запись уже есть (перезапись, нужно освободить её
    /// цепочку); `None` — отдан первый свободный слот (создание). `-DirFull`, если свободного
    /// слота нет (расширение каталога — позже).
    fn locate_or_free_slot(
        &self,
        dir_cluster: u32,
        target: &[u8; 11],
    ) -> Result<(u32, usize, Option<u32>), FatError> {
        let mut first_free: Option<(u32, usize)> = None;
        let mut cluster = dir_cluster;
        let mut steps_left = self.cluster_count;
        while self.valid_cluster(cluster) && steps_left > 0 {
            steps_left -= 1;
            let first = self.first_sector_of_cluster(cluster);
            for s in 0..self.sectors_per_cluster {
                let sector = first + s;
                let mut buf = [0u8; SECTOR_SIZE];
                virtio_blk::read_sector(sector as u64, &mut buf)?;
                for (idx, entry) in buf.chunks_exact(DIR_ENTRY_SIZE).enumerate() {
                    let offset = idx * DIR_ENTRY_SIZE;
                    match entry[0] {
                        // Конец каталога: этот слот свободен и дальше всё пусто. Если раньше
                        // нашли удалённый слот — берём его, иначе этот (терминатор сохранится в
                        // следующем нулевом слоте).
                        0x00 => {
                            let (sec, off) = first_free.unwrap_or((sector, offset));
                            return Ok((sec, off, None));
                        }
                        0xE5 => {
                            if first_free.is_none() {
                                first_free = Some((sector, offset));
                            }
                            continue;
                        }
                        _ => {}
                    }
                    let attr = entry[11];
                    if attr & ATTR_LONG_NAME == ATTR_LONG_NAME || attr & ATTR_VOLUME_ID != 0 {
                        continue;
                    }
                    if entry[..11] == target[..] {
                        let hi = read_u16(entry, 20) as u32;
                        let lo = read_u16(entry, 26) as u32;
                        return Ok((sector, offset, Some((hi << 16) | lo)));
                    }
                }
            }
            cluster = self.next_cluster(cluster)?;
        }
        match first_free {
            Some((sector, offset)) => Ok((sector, offset, None)),
            None => Err(FatError::DirFull),
        }
    }

    /// Записывает 32-байтную запись каталога по `(sector, offset)`: имя 8.3, атрибут, первый
    /// кластер и размер (read-modify-write сектора; остальные записи не трогаем).
    fn write_dir_entry(
        &self,
        sector: u32,
        offset: usize,
        name: &[u8; 11],
        attr: u8,
        first_cluster: u32,
        size: u32,
    ) -> Result<(), FatError> {
        let mut buf = [0u8; SECTOR_SIZE];
        virtio_blk::read_sector(sector as u64, &mut buf)?;
        fill_dir_entry(
            &mut buf[offset..offset + DIR_ENTRY_SIZE],
            name,
            attr,
            first_cluster,
            size,
        );
        virtio_blk::write_sector(sector as u64, &buf)?;
        Ok(())
    }

    /// Создаёт или перезаписывает файл по пути `path` (компоненты 8.3) его содержимым `data`.
    /// Спускается по подкаталогам (родитель должен существовать). Пустой файл → первый кластер
    /// 0 (как в FAT).
    ///
    /// Порядок безопасной замены: СНАЧАЛА строим новое содержимое (новая цепочка + данные),
    /// ПОТОМ одним обновлением записи каталога переключаем файл на него (коммит), и лишь ЗАТЕМ
    /// освобождаем старую цепочку. Поэтому сбой (нет места / ошибка диска) до коммита оставляет
    /// СТАРЫЙ файл нетронутым, а свежая цепочка откатывается — провал записи не разрушает файл.
    /// Заодно это гарантирует, что новые кластеры не пересекаются со старыми (старые в момент
    /// выделения ещё заняты).
    pub fn write_file(&self, path: &str, data: &[u8]) -> Result<(), FatError> {
        let (dir_cluster, name) = self.resolve_parent(path)?;
        let target = short_name_83(name);

        // 1) Существующая запись (под перезапись, с её первым кластером) или свободный слот.
        let (slot_sector, slot_offset, old_first) =
            self.locate_or_free_slot(dir_cluster, &target)?;

        // 2) Строим новое содержимое, НЕ трогая существующий файл (пустой файл — без кластеров).
        let cluster_bytes = self.sectors_per_cluster as usize * SECTOR_SIZE;
        let new_first = if data.is_empty() {
            0
        } else {
            let n = data.len().div_ceil(cluster_bytes) as u32;
            let first = self.alloc_chain(n)?;
            if let Err(e) = self.write_chain(first, data) {
                let _ = self.free_chain(first); // откат свежей цепочки
                return Err(e);
            }
            first
        };

        // 3) КОММИТ: переключаем запись каталога на новое содержимое. До этого файл — старый.
        if let Err(e) = self.write_dir_entry(
            slot_sector,
            slot_offset,
            &target,
            ATTR_ARCHIVE,
            new_first,
            data.len() as u32,
        ) {
            if new_first != 0 {
                let _ = self.free_chain(new_first); // запись не закоммитилась — откат
            }
            return Err(e);
        }

        // 4) Старое содержимое больше ни на что не ссылается — освобождаем (best-effort: его
        //    провал лишь оставит старые кластеры неиспользуемыми, файл уже корректно новый).
        if let Some(first) = old_first {
            if self.valid_cluster(first) {
                let _ = self.free_chain(first);
            }
        }
        Ok(())
    }

    /// Инициализирует кластер нового каталога: первый сектор получает записи `.` (на себя) и
    /// `..` (на родителя; 0, если родитель — корень, как требует спека FAT), остальное — нули.
    fn init_dir_cluster(&self, cluster: u32, parent: u32) -> Result<(), FatError> {
        let first_sec = self.first_sector_of_cluster(cluster);
        let mut buf = [0u8; SECTOR_SIZE];
        fill_dir_entry(
            &mut buf[0..DIR_ENTRY_SIZE],
            b".          ",
            ATTR_DIRECTORY,
            cluster,
            0,
        );
        let dotdot = if parent == self.root_cluster {
            0
        } else {
            parent
        };
        fill_dir_entry(
            &mut buf[DIR_ENTRY_SIZE..2 * DIR_ENTRY_SIZE],
            b"..         ",
            ATTR_DIRECTORY,
            dotdot,
            0,
        );
        virtio_blk::write_sector(first_sec as u64, &buf)?;
        // Остальные секторы кластера обнуляем (чтобы 0x00 в первой же записи завершал каталог).
        let zero = [0u8; SECTOR_SIZE];
        for s in 1..self.sectors_per_cluster {
            virtio_blk::write_sector((first_sec + s) as u64, &zero)?;
        }
        Ok(())
    }

    /// Создаёт каталог по пути `path` (родитель должен существовать). Выделяет под него кластер,
    /// кладёт `.`/`..` и записывает запись-каталог в родителя. `-AlreadyExists`, если имя занято.
    pub fn mkdir(&self, path: &str) -> Result<(), FatError> {
        let (parent_cluster, name) = self.resolve_parent(path)?;
        let target = short_name_83(name);

        let (slot_sector, slot_offset, existing) =
            self.locate_or_free_slot(parent_cluster, &target)?;
        if existing.is_some() {
            return Err(FatError::AlreadyExists);
        }

        let new_cluster = self.alloc_cluster()?; // помечен EOC
        if let Err(e) = self.init_dir_cluster(new_cluster, parent_cluster) {
            let _ = self.free_chain(new_cluster);
            return Err(e);
        }
        if let Err(e) = self.write_dir_entry(
            slot_sector,
            slot_offset,
            &target,
            ATTR_DIRECTORY,
            new_cluster,
            0,
        ) {
            let _ = self.free_chain(new_cluster);
            return Err(e);
        }
        Ok(())
    }
}

/// Заполняет 32-байтную запись каталога в срезе `entry`: имя 8.3, атрибут, первый кластер
/// (hi/lo) и размер; поля времени/даты обнуляем.
fn fill_dir_entry(entry: &mut [u8], name: &[u8; 11], attr: u8, first_cluster: u32, size: u32) {
    entry[..11].copy_from_slice(name);
    entry[11] = attr;
    for b in entry[12..20].iter_mut() {
        *b = 0; // NTRes, время создания, дата
    }
    entry[20..22].copy_from_slice(&((first_cluster >> 16) as u16).to_le_bytes()); // high
    for b in entry[22..26].iter_mut() {
        *b = 0; // время/дата записи
    }
    entry[26..28].copy_from_slice(&((first_cluster & 0xFFFF) as u16).to_le_bytes()); // low
    entry[28..32].copy_from_slice(&size.to_le_bytes());
}
