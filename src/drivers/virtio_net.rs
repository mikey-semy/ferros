//! Драйвер сетевой карты **virtio-net** (legacy virtio-pci, M8a — пока ДЕТЕКЦИЯ + MAC).
//!
//! # Зачем
//!
//! Это первый шаг сетевого тира (M8): научиться видеть сетевую карту и узнавать её аппаратный
//! адрес (MAC). Полный драйвер с очередями приёма/передачи кадров (RX/TX virtqueue) и обмен ARP —
//! следующий шаг (M8b); поверх него ляжет TCP/IP-стек (`smoltcp`, по политике reuse D13 — готовый).
//!
//! # Как (legacy virtio-pci, как у virtio-blk)
//!
//! Карта — устройство PCI `1af4:1000` (transitional virtio-net, отдаёт legacy-регистры в I/O-BAR0).
//! Базовое квитирование (сброс → ACK → DRIVER → согласование фич), затем читаем **MAC** из
//! device-specific config (6 байт по смещению `REG_CONFIG`). Очереди ещё не поднимаем — для
//! детекции достаточно подтвердить, что устройство есть и отдаёт MAC (фича `VIRTIO_NET_F_MAC`).
//!
//! Общие с virtio-blk legacy-константы здесь продублированы намеренно (их немного); вынесем в общий
//! модуль virtio-pci, когда в M8b вырастет полноценный драйвер с virtqueue.

use crate::arch::io;
use crate::drivers::pci::{self, Bar};
use crate::serial_println;
use spin::Mutex;

// --- PCI ---
const PCI_VENDOR_VIRTIO: u16 = 0x1AF4;
/// Transitional virtio-net (legacy-регистры в I/O-BAR0), как у нашего virtio-blk `1af4:1001`.
const PCI_DEVICE_VIRTIO_NET: u16 = 0x1000;
const PCI_COMMAND: u8 = 0x04;
const PCI_CMD_IO_SPACE: u32 = 1 << 0;
const PCI_CMD_BUS_MASTER: u32 = 1 << 2;

// --- legacy virtio-pci регистры от базы I/O-BAR0 (без MSI-X) ---
const REG_DEVICE_FEATURES: u16 = 0x00;
const REG_DRIVER_FEATURES: u16 = 0x04;
const REG_DEVICE_STATUS: u16 = 0x12;
/// Device-specific config: для virtio-net тут лежит MAC (6 байт) при `VIRTIO_NET_F_MAC`.
const REG_CONFIG: u16 = 0x14;

// --- Биты Device Status ---
const STATUS_ACK: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_FAILED: u8 = 0x80;

/// Фича: устройство предоставляет MAC в device-config (QEMU всегда даёт).
const VIRTIO_NET_F_MAC: u32 = 1 << 5;

/// MAC найденной карты. `None`, пока [`init`] не отработала.
static MAC: Mutex<Option<[u8; 6]>> = Mutex::new(None);

/// Находит карту virtio-net, делает базовое квитирование и читает её MAC из device-config.
///
/// Пока БЕЗ очередей приёма/передачи (это M8b) — детекция: подтверждаем, что NIC виден на PCI и
/// отдаёт MAC. Возвращает `false`, если устройства нет, BAR0 не I/O или карта не предлагает MAC.
/// Зовётся после поднятия кучи ([`pci::find`] аллоцирует `Vec`).
pub fn init() -> bool {
    let Some(dev) = pci::find(PCI_VENDOR_VIRTIO, PCI_DEVICE_VIRTIO_NET) else {
        serial_println!("[virtio-net] device not found");
        return false;
    };

    // Включаем I/O-пространство и bus-mastering (нужно будущему DMA очередей) в регистре команд PCI.
    let cmd = crate::arch::pci::config_read_u32(dev.bus, dev.slot, dev.func, PCI_COMMAND);
    crate::arch::pci::config_write_u32(
        dev.bus,
        dev.slot,
        dev.func,
        PCI_COMMAND,
        cmd | PCI_CMD_IO_SPACE | PCI_CMD_BUS_MASTER,
    );

    let io_base = match dev.bar(0) {
        Bar::Io { base } => base as u16,
        other => {
            serial_println!("[virtio-net] BAR0 is not an I/O BAR: {other:?}");
            return false;
        }
    };

    // SAFETY: io_base — корректная база I/O-BAR0 найденного устройства; смещения регистров — из
    // legacy virtio-pci. Делаем базовое квитирование (status/features) и читаем device-config
    // (MAC); virtqueue ещё не поднимаем (это M8b).
    let mac = unsafe {
        io::outb(io_base + REG_DEVICE_STATUS, 0); // сброс
        io::outb(io_base + REG_DEVICE_STATUS, STATUS_ACK);
        io::outb(io_base + REG_DEVICE_STATUS, STATUS_ACK | STATUS_DRIVER);

        let device_features = io::inl(io_base + REG_DEVICE_FEATURES);
        if device_features & VIRTIO_NET_F_MAC == 0 {
            serial_println!("[virtio-net] device does not provide a MAC (F_MAC unset)");
            io::outb(io_base + REG_DEVICE_STATUS, STATUS_FAILED);
            return false;
        }
        // Соглашаемся пока только на F_MAC (фичи RX/TX возьмём в M8b).
        io::outl(io_base + REG_DRIVER_FEATURES, VIRTIO_NET_F_MAC);

        let mut mac = [0u8; 6];
        for (i, b) in mac.iter_mut().enumerate() {
            *b = io::inb(io_base + REG_CONFIG + i as u16);
        }
        mac
    };

    serial_println!(
        "[virtio-net] ready: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5]
    );
    *MAC.lock() = Some(mac);
    true
}

/// MAC-адрес карты, если [`init`] её нашла; иначе `None`.
pub fn mac() -> Option<[u8; 6]> {
    *MAC.lock()
}
