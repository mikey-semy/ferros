//! Драйвер сетевой карты **virtio-net** (legacy virtio-pci): приём и передача Ethernet-кадров
//! (M8b; детекция + MAC — M8a).
//!
//! # Как virtio-net передаёт кадры
//!
//! У карты ДВЕ virtqueue: **0 = приём (RX)**, **1 = передача (TX)** — те же кольца дес크/avail/used,
//! что у virtio-blk, только данные тут сетевые кадры. Перед каждым кадром идёт служебный
//! **virtio_net_hdr** (10 байт без mergeable-буферов): на передаче мы его обнуляем, на приёме —
//! пропускаем. На RX заранее раздаём устройству пустые буферы (оно впишет в них пришедшие кадры);
//! на TX кладём кадр и пинаем очередь.
//!
//! Опрашиваем (без прерываний), как virtio-blk. Общие legacy virtio-pci константы продублированы из
//! `virtio_blk` намеренно — вынос в общий модуль virtio-pci отдельным шагом (HARDENING).

use crate::arch::io;
use crate::drivers::pci::{self, Bar};
use crate::mm::frame::BootInfoFrameAllocator;
use crate::serial_println;
use core::sync::atomic::{fence, Ordering};
use spin::Mutex;
use x86_64::VirtAddr;

// --- PCI ---
const PCI_VENDOR_VIRTIO: u16 = 0x1AF4;
const PCI_DEVICE_VIRTIO_NET: u16 = 0x1000; // transitional virtio-net (legacy I/O BAR)
const PCI_COMMAND: u8 = 0x04;
const PCI_CMD_IO_SPACE: u32 = 1 << 0;
const PCI_CMD_BUS_MASTER: u32 = 1 << 2;

// --- legacy virtio-pci регистры от базы I/O-BAR0 (без MSI-X) ---
const REG_DEVICE_FEATURES: u16 = 0x00;
const REG_DRIVER_FEATURES: u16 = 0x04;
const REG_QUEUE_PFN: u16 = 0x08;
const REG_QUEUE_SIZE: u16 = 0x0C;
const REG_QUEUE_SELECT: u16 = 0x0E;
const REG_QUEUE_NOTIFY: u16 = 0x10;
const REG_DEVICE_STATUS: u16 = 0x12;
const REG_CONFIG: u16 = 0x14; // device config: mac[6] @0

// --- Device Status ---
const STATUS_ACK: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FAILED: u8 = 0x80;

// --- Флаги дескрипторов/колец (кадр = один дескриптор, цепочек нет) ---
const VIRTQ_DESC_F_WRITE: u16 = 2; // буфер пишет устройство (для RX)
const VIRTQ_AVAIL_F_NO_INTERRUPT: u16 = 1;

const VIRTIO_NET_F_MAC: u32 = 1 << 5; // устройство даёт MAC
const QUEUE_ALIGN: usize = 4096;

/// Длина служебного заголовка `virtio_net_hdr` перед кадром (legacy, без mergeable-буферов).
const NET_HDR_LEN: usize = 10;
/// Размер одного приёмного буфера: заголовок + макс. Ethernet-кадр (1514), округлён.
const RX_BUF_STRIDE: usize = 2048;
/// Сколько приёмных буферов раздаём устройству.
const RX_BUF_COUNT: usize = 8;
/// Размер передающего буфера (одна страница) — против него и проверяем длину кадра.
const TX_BUF_SIZE: usize = QUEUE_ALIGN;
/// Индексы очередей.
const QUEUE_RX: u16 = 0;
const QUEUE_TX: u16 = 1;

/// Округление вверх до кратного `align` (степень двойки).
const fn align_up(x: usize, align: usize) -> usize {
    (x + align - 1) & !(align - 1)
}

/// Одна virtqueue: адрес колец + размер + последний виденный `used.idx`.
struct Virtq {
    vq_virt: u64,
    avail_offset: usize,
    used_offset: usize,
    queue_size: u16,
    last_used_idx: u16,
}

impl Virtq {
    /// Записывает дескриптор `i` (адрес/длина/флаги/next).
    /// # Safety: `vq_virt` — отображённая таблица дескрипторов; `i < queue_size`.
    unsafe fn write_desc(&self, i: usize, addr: u64, len: u32, flags: u16, next: u16) {
        let d = self.vq_virt + (i as u64) * 16;
        core::ptr::write_volatile(d as *mut u64, addr);
        core::ptr::write_volatile((d + 8) as *mut u32, len);
        core::ptr::write_volatile((d + 12) as *mut u16, flags);
        core::ptr::write_volatile((d + 14) as *mut u16, next);
    }

    /// Кладёт голову цепочки `desc_head` в «доступное» кольцо и двигает `avail.idx`. Возвращает
    /// новое значение `avail.idx` (= сколько дескрипторов всего отдано устройству) — для TX это и
    /// есть цель ожидания: когда `last_used_idx` дорастёт до неё, наш кадр забран.
    /// # Safety: вызывается под мьютексом устройства; кольца отображены.
    unsafe fn push_avail(&self, desc_head: u16) -> u16 {
        let avail = self.vq_virt + self.avail_offset as u64;
        let idx_ptr = (avail + 2) as *mut u16;
        let idx = core::ptr::read_volatile(idx_ptr);
        let slot = (avail + 4 + (idx % self.queue_size) as u64 * 2) as *mut u16;
        core::ptr::write_volatile(slot, desc_head);
        fence(Ordering::SeqCst); // дескриптор/кольцо видны устройству ДО продвижения idx
        let new_idx = idx.wrapping_add(1);
        core::ptr::write_volatile(idx_ptr, new_idx);
        fence(Ordering::SeqCst);
        new_idx
    }

    /// Снимает следующий завершённый элемент «использованного» кольца (id дескриптора + длина),
    /// если устройство продвинуло `used.idx`. `None` — пока ничего нового.
    /// # Safety: вызывается под мьютексом устройства; кольца отображены.
    unsafe fn take_used(&mut self) -> Option<(u32, u32)> {
        fence(Ordering::SeqCst);
        let used = self.vq_virt + self.used_offset as u64;
        let used_idx = core::ptr::read_volatile((used + 2) as *const u16);
        if used_idx == self.last_used_idx {
            return None;
        }
        let ring_slot = (self.last_used_idx % self.queue_size) as u64;
        let elem = used + 4 + ring_slot * 8; // {id: u32, len: u32}
        let id = core::ptr::read_volatile(elem as *const u32);
        let len = core::ptr::read_volatile((elem + 4) as *const u32);
        self.last_used_idx = self.last_used_idx.wrapping_add(1);
        Some((id, len))
    }
}

/// Состояние карты virtio-net (одно устройство). Адреса — `u64` (структура `Send`, живёт в `Mutex`).
struct VirtioNet {
    io_base: u16,
    rx: Virtq,
    tx: Virtq,
    rx_buf_virt: u64,
    rx_buf_phys: u64,
    tx_buf_virt: u64,
    tx_buf_phys: u64,
    mac: [u8; 6],
}

static DEVICE: Mutex<Option<VirtioNet>> = Mutex::new(None);

/// Настраивает одну virtqueue `queue_index`: считывает её размер, выделяет физически непрерывную
/// память под кольца, обнуляет, просит «не слать прерывания» и отдаёт устройству PFN.
///
/// # Safety
/// `io_base` корректен; `phys_offset` — оффсет физпамяти; вызывается на старте инициализации.
unsafe fn setup_virtq(
    io_base: u16,
    queue_index: u16,
    phys_offset: VirtAddr,
    fa: &mut BootInfoFrameAllocator,
) -> Option<Virtq> {
    io::outw(io_base + REG_QUEUE_SELECT, queue_index);
    let queue_size = io::inw(io_base + REG_QUEUE_SIZE);
    if queue_size == 0 {
        return None;
    }
    let qsize = queue_size as usize;
    let avail_offset = 16 * qsize;
    let used_offset = align_up(16 * qsize + 6 + 2 * qsize, QUEUE_ALIGN);
    let vq_bytes = used_offset + 6 + 8 * qsize;
    let vq_pages = align_up(vq_bytes, QUEUE_ALIGN) / QUEUE_ALIGN;

    let vq_frame = fa.allocate_contiguous(vq_pages)?;
    let vq_phys = vq_frame.start_address().as_u64();
    let vq_virt = (phys_offset + vq_phys).as_u64();
    core::ptr::write_bytes(vq_virt as *mut u8, 0, vq_pages * QUEUE_ALIGN);
    core::ptr::write_volatile(
        (vq_virt + avail_offset as u64) as *mut u16,
        VIRTQ_AVAIL_F_NO_INTERRUPT,
    );
    io::outl(io_base + REG_QUEUE_PFN, (vq_phys >> 12) as u32);

    Some(Virtq {
        vq_virt,
        avail_offset,
        used_offset,
        queue_size,
        last_used_idx: 0,
    })
}

/// Находит карту virtio-net, поднимает очереди RX/TX, раздаёт устройству приёмные буферы и
/// объявляет готовность. После этого работают [`send`]/[`recv`]. `false` — устройства нет / нет
/// памяти / нет нужной фичи. Зовётся после кучи и с аллокатором фреймов (virtqueue требует
/// физически непрерывной памяти).
pub fn init(phys_offset: VirtAddr, fa: &mut BootInfoFrameAllocator) -> bool {
    let Some(dev) = pci::find(PCI_VENDOR_VIRTIO, PCI_DEVICE_VIRTIO_NET) else {
        serial_println!("[virtio-net] device not found");
        return false;
    };

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

    // SAFETY: io_base корректен; смещения регистров — legacy virtio-pci; память virtqueue/буферов
    // берём у аллокатора (физически непрерывную) и читаем/пишем по `phys + phys_offset`.
    unsafe {
        // 1) Квитирование и согласование фич (берём только F_MAC: без mergeable-буферов и offload,
        //    тогда заголовок ровно 10 байт и один буфер на кадр).
        io::outb(io_base + REG_DEVICE_STATUS, 0);
        io::outb(io_base + REG_DEVICE_STATUS, STATUS_ACK);
        io::outb(io_base + REG_DEVICE_STATUS, STATUS_ACK | STATUS_DRIVER);
        let features = io::inl(io_base + REG_DEVICE_FEATURES);
        if features & VIRTIO_NET_F_MAC == 0 {
            serial_println!("[virtio-net] no F_MAC");
            io::outb(io_base + REG_DEVICE_STATUS, STATUS_FAILED);
            return false;
        }
        io::outl(io_base + REG_DRIVER_FEATURES, VIRTIO_NET_F_MAC);

        let mut mac = [0u8; 6];
        for (i, b) in mac.iter_mut().enumerate() {
            *b = io::inb(io_base + REG_CONFIG + i as u16);
        }

        // 2) Очереди RX (0) и TX (1).
        let (Some(rx), Some(tx)) = (
            setup_virtq(io_base, QUEUE_RX, phys_offset, fa),
            setup_virtq(io_base, QUEUE_TX, phys_offset, fa),
        ) else {
            serial_println!("[virtio-net] queue setup failed");
            io::outb(io_base + REG_DEVICE_STATUS, STATUS_FAILED);
            return false;
        };

        // 3) Буферы: RX_BUF_COUNT приёмных (одной непрерывной областью) + один передающий.
        let rx_pages = align_up(RX_BUF_COUNT * RX_BUF_STRIDE, QUEUE_ALIGN) / QUEUE_ALIGN;
        let (Some(rx_frame), Some(tx_frame)) =
            (fa.allocate_contiguous(rx_pages), fa.allocate_contiguous(1))
        else {
            serial_println!("[virtio-net] no memory for buffers");
            io::outb(io_base + REG_DEVICE_STATUS, STATUS_FAILED);
            return false;
        };
        let rx_buf_phys = rx_frame.start_address().as_u64();
        let rx_buf_virt = (phys_offset + rx_buf_phys).as_u64();
        let tx_buf_phys = tx_frame.start_address().as_u64();
        let tx_buf_virt = (phys_offset + tx_buf_phys).as_u64();

        let net = VirtioNet {
            io_base,
            rx,
            tx,
            rx_buf_virt,
            rx_buf_phys,
            tx_buf_virt,
            tx_buf_phys,
            mac,
        };

        // 4) Раздаём устройству все приёмные буферы (дескриптор i ↔ буфер i; пишет устройство).
        for i in 0..RX_BUF_COUNT {
            let addr = net.rx_buf_phys + (i * RX_BUF_STRIDE) as u64;
            net.rx
                .write_desc(i, addr, RX_BUF_STRIDE as u32, VIRTQ_DESC_F_WRITE, 0);
            net.rx.push_avail(i as u16);
        }

        // 5) Готовность и пинок RX (буферы доступны).
        io::outb(
            io_base + REG_DEVICE_STATUS,
            STATUS_ACK | STATUS_DRIVER | STATUS_DRIVER_OK,
        );
        io::outw(io_base + REG_QUEUE_NOTIFY, QUEUE_RX);

        serial_println!(
            "[virtio-net] ready: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, rx={} tx={}",
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5],
            net.rx.queue_size,
            net.tx.queue_size
        );
        *DEVICE.lock() = Some(net);
    }
    true
}

/// MAC-адрес карты, если инициализирована.
pub fn mac() -> Option<[u8; 6]> {
    DEVICE.lock().as_ref().map(|d| d.mac)
}

/// Передаёт один Ethernet-кадр `frame` (без `virtio_net_hdr` — добавим сами). Синхронно: кладём
/// кадр в TX-буфер, пинаем очередь и ждём, пока устройство его заберёт. `false`, если драйвер не
/// поднят или кадр не влез в буфер.
pub fn send(frame: &[u8]) -> bool {
    if frame.len() > TX_BUF_SIZE - NET_HDR_LEN {
        return false;
    }
    let mut guard = DEVICE.lock();
    let Some(net) = guard.as_mut() else {
        return false;
    };
    // SAFETY: TX-буфер и virtqueue принадлежат драйверу и отображены; один кадр за раз под мьютексом.
    unsafe {
        // virtio_net_hdr = нули (без offload), затем сам кадр.
        core::ptr::write_bytes(net.tx_buf_virt as *mut u8, 0, NET_HDR_LEN);
        core::ptr::copy_nonoverlapping(
            frame.as_ptr(),
            (net.tx_buf_virt + NET_HDR_LEN as u64) as *mut u8,
            frame.len(),
        );
        fence(Ordering::SeqCst);
        net.tx.write_desc(
            0,
            net.tx_buf_phys,
            (NET_HDR_LEN + frame.len()) as u32,
            0, // устройство только читает (передача)
            0,
        );
        // Цель ожидания — новое значение avail.idx: ждём, пока used догонит именно его. Так, если
        // прошлый send отвалился по таймауту (оставив незабранный дескриптор), этот корректно
        // дренирует и устаревшее, и своё завершение, а не примет чужое за своё.
        let target = net.tx.push_avail(0);
        io::outw(net.io_base + REG_QUEUE_NOTIFY, QUEUE_TX);

        let mut spins = 0u64;
        while net.tx.last_used_idx != target {
            if net.tx.take_used().is_some() {
                continue; // забрали завершение (своё или устаревшее) — проверяем цель снова
            }
            spins += 1;
            if spins > 100_000_000 {
                return false; // устройство не ответило — не виснем (цель догонит следующий send)
            }
            core::hint::spin_loop();
        }
    }
    true
}

/// Опрашивает RX-очередь: если пришёл кадр — копирует его (без `virtio_net_hdr`) в `out`,
/// возвращает число скопированных байт и **возвращает буфер устройству** под новый приём. `None` —
/// пока кадров нет (или драйвер не поднят).
pub fn recv(out: &mut [u8]) -> Option<usize> {
    let mut guard = DEVICE.lock();
    let net = guard.as_mut()?;
    // SAFETY: RX-буферы/virtqueue принадлежат драйверу и отображены; один приём за раз под мьютексом.
    unsafe {
        let (id, len) = net.rx.take_used()?;
        let id = id as usize;
        // Не доверяем устройству: id и длина приходят из used-кольца. Чужой id увёл бы чтение (и
        // переотданный дескриптор) за пределы RX-области — отбрасываем кадр. Длину зажимаем по
        // размеру буфера, чтобы copy не вышел за буфер id.
        if id >= RX_BUF_COUNT {
            return None;
        }
        let total = (len as usize).min(RX_BUF_STRIDE);
        let frame_len = total.saturating_sub(NET_HDR_LEN);
        let n = frame_len.min(out.len());
        let src = net.rx_buf_virt + (id * RX_BUF_STRIDE + NET_HDR_LEN) as u64;
        core::ptr::copy_nonoverlapping(src as *const u8, out.as_mut_ptr(), n);

        // Возвращаем буфер устройству под следующий кадр.
        let addr = net.rx_buf_phys + (id * RX_BUF_STRIDE) as u64;
        net.rx
            .write_desc(id, addr, RX_BUF_STRIDE as u32, VIRTQ_DESC_F_WRITE, 0);
        net.rx.push_avail(id as u16);
        io::outw(net.io_base + REG_QUEUE_NOTIFY, QUEUE_RX);
        Some(n)
    }
}
