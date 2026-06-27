//! Обход шины PCI и модель устройства (переносимая часть драйвера, M6a).
//!
//! Механику портов (0xCF8/0xCFC) знает только [`crate::arch::pci`] (арх-шов, CONVENTIONS
//! §1); здесь — модель устройства, разбор BAR'ов и перебор всех `(шина, слот, функция)`.
//!
//! # Зачем перечислять
//!
//! Чтобы найти нужное устройство (в M6b нам потребуется диск virtio-blk), сперва надо
//! узнать, что вообще висит на шине. Перебираем все возможные адреса и читаем у каждого
//! идентификатор производителя: `0xFFFF` означает «здесь никого нет».

use crate::arch::pci;
use crate::serial_println;
use alloc::vec::Vec;

/// Идентификатор производителя «никого нет» (на этом адресе устройства не существует).
const VENDOR_NONE: u16 = 0xFFFF;
/// Бит в поле header type: устройство многофункциональное (есть функции 1..8).
const HEADER_MULTIFUNCTION: u8 = 0x80;

/// Одно устройство на шине: его «координаты» `(bus, slot, func)` и прочитанная анкета.
/// `bars` — сырые 32-битные слова шести Base Address Registers (расшифровываются методом
/// [`PciDevice::bar`]).
#[derive(Debug, Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub slot: u8,
    pub func: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    /// Старший байт кода класса (что это за устройство в целом: 0x01 — контроллер хранилища…).
    pub class: u8,
    /// Подкласс (уточнение внутри класса).
    pub subclass: u8,
    /// Программный интерфейс (ещё уточнение).
    pub prog_if: u8,
    /// Тип заголовка; бит 0x80 — устройство многофункциональное.
    pub header_type: u8,
    pub bars: [u32; 6],
}

/// Расшифрованный Base Address Register — где у устройства его регистры/память.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bar {
    /// BAR не используется (нулевой).
    None,
    /// Регистры в пространстве портов ввода-вывода (так у legacy virtio-blk — это нужно M6b).
    Io { base: u32 },
    /// Регистры в физической памяти (MMIO).
    Mem {
        base: u64,
        prefetchable: bool,
        is_64: bool,
    },
}

impl PciDevice {
    /// Расшифровывает `index`-й BAR (`index` в `0..6`). Младший бит сырого слова отличает
    /// I/O от памяти; у 64-битного MMIO старшие 32 бита базы лежат в следующем BAR.
    ///
    /// Внимание: при последовательном переборе `bar(0..6)` после 64-битного BAR следующий
    /// индекс — это его *старшая половина*, а не отдельный BAR (вернётся «мусорный» `Mem`).
    /// Пока ни один вызывающий не перебирает BAR'ы (M6b читает у virtio только `bar(0)` —
    /// I/O), поэтому достаточно знать о caveat; корректный итератор BAR'ов — в M6c+
    /// (см. HARDENING.md).
    pub fn bar(&self, index: usize) -> Bar {
        let raw = self.bars[index];
        if raw == 0 {
            return Bar::None;
        }
        if raw & 1 != 0 {
            // I/O-пространство: база — слово без двух младших служебных бит.
            Bar::Io {
                base: raw & 0xFFFF_FFFC,
            }
        } else {
            // MMIO: биты [2:1] — тип (0b10 = 64-битный), бит 3 — prefetchable.
            let is_64 = (raw >> 1) & 0b11 == 0b10;
            let prefetchable = raw & 0b1000 != 0;
            let mut base = (raw & 0xFFFF_FFF0) as u64;
            if is_64 && index + 1 < self.bars.len() {
                base |= (self.bars[index + 1] as u64) << 32;
            }
            Bar::Mem {
                base,
                prefetchable,
                is_64,
            }
        }
    }
}

/// Читает 16-битное поле конфигурации: берём слово и выбираем нужную половину.
fn config_read_u16(bus: u8, slot: u8, func: u8, offset: u8) -> u16 {
    let dword = pci::config_read_u32(bus, slot, func, offset);
    let shift = (offset as u32 & 2) * 8;
    (dword >> shift) as u16
}

/// Читает 8-битное поле конфигурации: берём слово и выбираем нужный байт.
fn config_read_u8(bus: u8, slot: u8, func: u8, offset: u8) -> u8 {
    let dword = pci::config_read_u32(bus, slot, func, offset);
    let shift = (offset as u32 & 3) * 8;
    (dword >> shift) as u8
}

/// Читает анкету устройства по адресу `(bus, slot, func)`. `None` — на этом адресе пусто.
fn read_device(bus: u8, slot: u8, func: u8) -> Option<PciDevice> {
    let vendor_id = config_read_u16(bus, slot, func, 0x00);
    if vendor_id == VENDOR_NONE {
        return None;
    }
    let device_id = config_read_u16(bus, slot, func, 0x02);
    let prog_if = config_read_u8(bus, slot, func, 0x09);
    let subclass = config_read_u8(bus, slot, func, 0x0A);
    let class = config_read_u8(bus, slot, func, 0x0B);
    let header_type = config_read_u8(bus, slot, func, 0x0E);

    let mut bars = [0u32; 6];
    for (i, bar) in bars.iter_mut().enumerate() {
        *bar = pci::config_read_u32(bus, slot, func, 0x10 + (i as u8) * 4);
    }

    Some(PciDevice {
        bus,
        slot,
        func,
        vendor_id,
        device_id,
        class,
        subclass,
        prog_if,
        header_type,
        bars,
    })
}

/// Полный перебор шины: для каждого `(bus, slot)` читаем функцию 0, а если устройство
/// многофункциональное — ещё функции 1..8. Требует кучу (результат в `Vec`).
///
/// Перебор «в лоб» (все 256 шин) допустим на QEMU; умное перечисление по мостам — в
/// HARDENING.md.
pub fn enumerate() -> Vec<PciDevice> {
    let mut devices = Vec::new();
    for bus in 0..=255u8 {
        for slot in 0..32u8 {
            let Some(dev0) = read_device(bus, slot, 0) else {
                continue;
            };
            let multifunction = dev0.header_type & HEADER_MULTIFUNCTION != 0;
            devices.push(dev0);
            if multifunction {
                for func in 1..8u8 {
                    if let Some(dev) = read_device(bus, slot, func) {
                        devices.push(dev);
                    }
                }
            }
        }
    }
    devices
}

/// Ищет на шине устройство с заданными производителем и идентификатором.
pub fn find(vendor_id: u16, device_id: u16) -> Option<PciDevice> {
    enumerate()
        .into_iter()
        .find(|d| d.vendor_id == vendor_id && d.device_id == device_id)
}

/// Перечисляет шину PCI и печатает список устройств в serial — видно, что и где висит.
/// Вызывается из загрузки ядра после поднятия кучи.
pub fn init() {
    let devices = enumerate();
    serial_println!("[pci] {} device(s) on the bus:", devices.len());
    for d in &devices {
        serial_println!(
            "  {:02x}:{:02x}.{} vendor={:04x} device={:04x} class={:02x}:{:02x}",
            d.bus,
            d.slot,
            d.func,
            d.vendor_id,
            d.device_id,
            d.class,
            d.subclass,
        );
    }
}
