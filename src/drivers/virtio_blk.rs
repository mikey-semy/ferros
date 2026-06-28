//! Драйвер диска **virtio-blk** (legacy virtio-pci, M6b): чтение и запись секторов (запись —
//! M6g1).
//!
//! # Что такое virtio
//!
//! virtio — стандарт «паравиртуализованных» устройств: гость (мы) и гипервизор (QEMU)
//! договариваются об общей структуре в памяти — **virtqueue** — через которую гость кладёт
//! запросы, а устройство их выполняет по DMA. Это куда быстрее эмуляции реального
//! контроллера (ATA/AHCI) и идеально для QEMU.
//!
//! # Как мы с ним общаемся (legacy)
//!
//! Устройство — на шине PCI (его нашёл M6a: `1af4:1001`). Регистры управления — в
//! пространстве портов ввода-вывода по базе из BAR0 (через [`crate::arch::io`]). Алгоритм:
//! сброс → ACK/DRIVER → согласование фич → отдать устройству физический адрес одной
//! virtqueue → DRIVER_OK. Дальше запрос на чтение — это **цепочка из трёх дескрипторов**
//! (заголовок-команда → буфер данных → байт статуса): кладём её в «доступное» кольцо,
//! пишем в Queue Notify, опрашиваем «использованное» кольцо до завершения.
//!
//! # Опасное — за швами
//!
//! virtqueue должна быть физически непрерывной (адресуется одним PFN = phys>>12) —
//! берём её через [`crate::mm::frame::BootInfoFrameAllocator::allocate_contiguous`].
//! Доступ к кольцам — сырые volatile-обращения по адресам `phys + phys_mem_offset`; весь
//! `unsafe` собран в методах драйвера с `// SAFETY` (D9).

// Два разных модуля `pci`: `crate::arch::pci` — механика портов конфигурации (шов, ниже
// зовём по полному пути), `crate::drivers::pci` — переносимая модель устройства (find/Bar).
use crate::arch::io;
use crate::drivers::pci::{self, Bar};
use crate::mm::frame::BootInfoFrameAllocator;
use crate::serial_println;
use core::sync::atomic::{fence, Ordering};
use spin::Mutex;
use x86_64::VirtAddr;

// --- Идентификаторы PCI и регистр команд ---
const PCI_VENDOR_VIRTIO: u16 = 0x1AF4;
/// «Переходное» (legacy) virtio-blk: устройство, которое отдаёт legacy-регистры в I/O-BAR0.
const PCI_DEVICE_VIRTIO_BLK: u16 = 0x1001;
const PCI_COMMAND: u8 = 0x04;
const PCI_CMD_IO_SPACE: u32 = 1 << 0;
const PCI_CMD_BUS_MASTER: u32 = 1 << 2;

// --- Смещения legacy virtio-pci регистров от базы I/O-BAR0 (без MSI-X) ---
const REG_DEVICE_FEATURES: u16 = 0x00;
const REG_DRIVER_FEATURES: u16 = 0x04;
const REG_QUEUE_PFN: u16 = 0x08;
const REG_QUEUE_SIZE: u16 = 0x0C;
const REG_QUEUE_SELECT: u16 = 0x0E;
const REG_QUEUE_NOTIFY: u16 = 0x10;
const REG_DEVICE_STATUS: u16 = 0x12;
/// Специфичная для устройства конфигурация (для blk: ёмкость u64) начинается тут (без MSI-X).
const REG_CONFIG: u16 = 0x14;

// --- Биты Device Status ---
const STATUS_ACK: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FAILED: u8 = 0x80;

// --- Флаги дескрипторов и колец virtqueue ---
const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;
/// Просим устройство НЕ слать прерывание по завершении — мы опрашиваем (IRQ virtio не
/// обрабатываем; иначе незанятый вектор INTx мог бы прилететь в ядро).
const VIRTQ_AVAIL_F_NO_INTERRUPT: u16 = 1;

// --- virtio-blk ---
const VIRTIO_BLK_T_IN: u32 = 0; // чтение (устройство → память)
const VIRTIO_BLK_T_OUT: u32 = 1; // запись (память → устройство)
/// Размер сектора (байт). virtio-blk оперирует 512-байтными секторами.
pub const SECTOR_SIZE: usize = 512;
/// Выравнивание virtqueue в legacy (вся очередь — на границе страницы; used-кольцо тоже).
const QUEUE_ALIGN: usize = 4096;

/// Глобальный экземпляр драйвера (одно устройство). `None`, пока не инициализирован.
static DEVICE: Mutex<Option<VirtioBlk>> = Mutex::new(None);

/// Почему операция с диском не удалась.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlkError {
    /// Драйвер ещё не инициализирован (устройство не найдено).
    NotInitialized,
    /// Запрошенный сектор за пределами ёмкости диска.
    OutOfRange,
    /// Устройство вернуло ненулевой статус (1 = IOERR, 2 = UNSUPP).
    DeviceError(u8),
}

/// Состояние одного устройства virtio-blk. Храним адреса как `u64` (не сырые указатели) —
/// чтобы структура была `Send` и жила в `Mutex`.
struct VirtioBlk {
    /// База портов ввода-вывода (из BAR0).
    io_base: u16,
    /// Размер очереди (число дескрипторов), задан устройством.
    queue_size: u16,
    /// Виртуальный адрес начала virtqueue (= таблица дескрипторов).
    vq_virt: u64,
    /// Смещение «доступного» кольца от начала virtqueue.
    avail_offset: usize,
    /// Смещение «использованного» кольца от начала virtqueue.
    used_offset: usize,
    /// Виртуальный и физический адреса страницы-буфера (заголовок/статус/данные).
    buf_virt: u64,
    buf_phys: u64,
    /// Последний виденный `used.idx` — чтобы заметить продвижение после запроса.
    last_used_idx: u16,
    /// Ёмкость диска в секторах по 512 байт.
    capacity_sectors: u64,
}

/// Округление вверх до кратного `align` (степень двойки).
const fn align_up(x: usize, align: usize) -> usize {
    (x + align - 1) & !(align - 1)
}

// Раскладка страницы-буфера запроса (всё помещается в одну 4 КиБ страницу):
const BUF_HEADER: u64 = 0; // virtio_blk_outhdr: type u32, ioprio u32, sector u64 (16 байт)
const BUF_STATUS: u64 = 16; // байт статуса
const BUF_DATA: u64 = 512; // данные сектора (512 байт)

/// Инициализирует первый диск virtio-blk: включает DMA, согласует фичи, поднимает одну
/// virtqueue и запоминает устройство. Возвращает `false`, если устройство не найдено.
///
/// Должна вызываться после поднятия кучи (использует [`pci::find`], аллоцирующий `Vec`) и
/// получает аллокатор фреймов — virtqueue требует физически непрерывной памяти.
pub fn init(phys_mem_offset: VirtAddr, frame_allocator: &mut BootInfoFrameAllocator) -> bool {
    let Some(dev) = pci::find(PCI_VENDOR_VIRTIO, PCI_DEVICE_VIRTIO_BLK) else {
        serial_println!("[virtio-blk] device not found");
        return false;
    };

    // Включаем I/O-пространство и bus-mastering (без него устройство не сможет читать
    // virtqueue по DMA) в регистре команд PCI.
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
            serial_println!("[virtio-blk] BAR0 is not an I/O BAR: {other:?}");
            return false;
        }
    };

    // SAFETY: io_base — корректная база I/O-BAR0 найденного устройства; смещения регистров —
    // из legacy virtio-pci спецификации. Память virtqueue/буфера получаем у аллокатора и
    // читаем/пишем по `phys + phys_mem_offset` (bootloader отобразил всю физпамять).
    unsafe {
        // 1) Сброс и квитирование.
        io::outb(io_base + REG_DEVICE_STATUS, 0);
        io::outb(io_base + REG_DEVICE_STATUS, STATUS_ACK);
        io::outb(io_base + REG_DEVICE_STATUS, STATUS_ACK | STATUS_DRIVER);

        // 2) Согласование фич: читаем предлагаемые устройством (шаг протокола), но для
        // базового чтения не берём ни одной — пишем 0. Чтение оставляем как явный шаг
        // handshake; не удалять.
        let _device_features = io::inl(io_base + REG_DEVICE_FEATURES);
        io::outl(io_base + REG_DRIVER_FEATURES, 0);

        // 3) Очередь 0: узнаём её размер и считаем раскладку.
        io::outw(io_base + REG_QUEUE_SELECT, 0);
        let queue_size = io::inw(io_base + REG_QUEUE_SIZE);
        if queue_size == 0 {
            serial_println!("[virtio-blk] queue 0 unavailable");
            io::outb(io_base + REG_DEVICE_STATUS, STATUS_FAILED);
            return false;
        }
        let qsize = queue_size as usize;
        // desc[qsize] | avail | (выравнивание) | used  — legacy-раскладка, align 4096.
        let avail_offset = 16 * qsize;
        let used_offset = align_up(16 * qsize + 6 + 2 * qsize, QUEUE_ALIGN);
        let vq_bytes = used_offset + 6 + 8 * qsize;
        let vq_pages = align_up(vq_bytes, QUEUE_ALIGN) / QUEUE_ALIGN;

        let Some(vq_frame) = frame_allocator.allocate_contiguous(vq_pages) else {
            serial_println!("[virtio-blk] no contiguous memory for virtqueue");
            io::outb(io_base + REG_DEVICE_STATUS, STATUS_FAILED);
            return false;
        };
        let vq_phys = vq_frame.start_address().as_u64();
        let vq_virt = (phys_mem_offset + vq_phys).as_u64();
        core::ptr::write_bytes(vq_virt as *mut u8, 0, vq_pages * QUEUE_ALIGN);
        // Просим устройство не слать прерывания (мы опрашиваем): avail.flags = NO_INTERRUPT.
        core::ptr::write_volatile(
            (vq_virt + avail_offset as u64) as *mut u16,
            VIRTQ_AVAIL_F_NO_INTERRUPT,
        );

        // Страница-буфер под заголовок/статус/данные одного запроса.
        let Some(buf_frame) = frame_allocator.allocate_contiguous(1) else {
            serial_println!("[virtio-blk] no memory for request buffer");
            io::outb(io_base + REG_DEVICE_STATUS, STATUS_FAILED);
            return false;
        };
        let buf_phys = buf_frame.start_address().as_u64();
        let buf_virt = (phys_mem_offset + buf_phys).as_u64();

        // 4) Отдаём устройству адрес очереди (в страницах) и объявляем готовность.
        io::outl(io_base + REG_QUEUE_PFN, (vq_phys >> 12) as u32);
        io::outb(
            io_base + REG_DEVICE_STATUS,
            STATUS_ACK | STATUS_DRIVER | STATUS_DRIVER_OK,
        );

        // Ёмкость диска (секторов) — из device-config: u64 двумя 32-битными чтениями.
        let cap_lo = io::inl(io_base + REG_CONFIG) as u64;
        let cap_hi = io::inl(io_base + REG_CONFIG + 4) as u64;
        let capacity_sectors = cap_lo | (cap_hi << 32);

        serial_println!(
            "[virtio-blk] ready: queue_size={queue_size}, capacity={capacity_sectors} sectors ({} KiB)",
            capacity_sectors * SECTOR_SIZE as u64 / 1024
        );

        *DEVICE.lock() = Some(VirtioBlk {
            io_base,
            queue_size,
            vq_virt,
            avail_offset,
            used_offset,
            buf_virt,
            buf_phys,
            last_used_idx: 0,
            capacity_sectors,
        });
    }
    true
}

/// Читает один 512-байтный сектор `lba` в `buf`. Синхронно: ставит запрос в очередь,
/// пинает устройство и опрашивает завершение.
pub fn read_sector(lba: u64, buf: &mut [u8; SECTOR_SIZE]) -> Result<(), BlkError> {
    let mut guard = DEVICE.lock();
    let dev = guard.as_mut().ok_or(BlkError::NotInitialized)?;
    if lba >= dev.capacity_sectors {
        return Err(BlkError::OutOfRange);
    }
    // SAFETY: virtqueue и буфер принадлежат драйверу и отображены; одновременно выполняется
    // ровно один запрос (мьютекс держится всё время), поэтому дескрипторы 0..3 можно
    // переиспользовать — прошлый запрос к этому моменту завершён.
    unsafe { dev.submit_read(lba, buf) }
}

/// Записывает один 512-байтный сектор `lba` из `buf`. Синхронно, как [`read_sector`] (тот же
/// путь virtqueue, только данные едут память → устройство).
pub fn write_sector(lba: u64, buf: &[u8; SECTOR_SIZE]) -> Result<(), BlkError> {
    let mut guard = DEVICE.lock();
    let dev = guard.as_mut().ok_or(BlkError::NotInitialized)?;
    if lba >= dev.capacity_sectors {
        return Err(BlkError::OutOfRange);
    }
    // SAFETY: как в read_sector — единственный запрос за раз под мьютексом устройства.
    unsafe { dev.submit_write(lba, buf) }
}

impl VirtioBlk {
    /// Записывает дескриптор `i`: адрес буфера, длину, флаги и индекс следующего.
    ///
    /// # Safety
    /// `vq_virt` указывает на отображённую таблицу дескрипторов нужного размера; `i` в её
    /// пределах.
    unsafe fn write_desc(&self, i: usize, addr: u64, len: u32, flags: u16, next: u16) {
        let d = self.vq_virt + (i as u64) * 16;
        core::ptr::write_volatile(d as *mut u64, addr);
        core::ptr::write_volatile((d + 8) as *mut u32, len);
        core::ptr::write_volatile((d + 12) as *mut u16, flags);
        core::ptr::write_volatile((d + 14) as *mut u16, next);
    }

    /// Заполняет заголовок-команду (тип `blk_type`, сектор `lba`) и цепочку из трёх
    /// дескрипторов: заголовок (читает устройство) → данные → статус (пишет устройство).
    /// `data_device_write` — пишет ли устройство в буфер данных: `true` для чтения (данные едут
    /// устройство → память), `false` для записи (память → устройство, устройство только читает).
    ///
    /// # Safety
    /// Вызывается под `DEVICE.lock()`; адреса virtqueue/буфера корректны и отображены.
    unsafe fn setup_chain(&self, blk_type: u32, lba: u64, data_device_write: bool) {
        core::ptr::write_volatile((self.buf_virt + BUF_HEADER) as *mut u32, blk_type);
        core::ptr::write_volatile((self.buf_virt + BUF_HEADER + 4) as *mut u32, 0); // ioprio
        core::ptr::write_volatile((self.buf_virt + BUF_HEADER + 8) as *mut u64, lba); // sector
        core::ptr::write_volatile((self.buf_virt + BUF_STATUS) as *mut u8, 0xFF); // «не записан»

        let data_flags = if data_device_write {
            VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE
        } else {
            VIRTQ_DESC_F_NEXT
        };
        self.write_desc(0, self.buf_phys + BUF_HEADER, 16, VIRTQ_DESC_F_NEXT, 1);
        self.write_desc(
            1,
            self.buf_phys + BUF_DATA,
            SECTOR_SIZE as u32,
            data_flags,
            2,
        );
        self.write_desc(2, self.buf_phys + BUF_STATUS, 1, VIRTQ_DESC_F_WRITE, 0);
    }

    /// Общая часть запроса: кладёт голову цепочки (дескриптор 0) в «доступное» кольцо, пинает
    /// устройство, опрашивает «использованное» кольцо до завершения и возвращает результат по
    /// байту статуса. Заголовок и дескрипторы должны быть уже заполнены [`Self::setup_chain`].
    ///
    /// # Safety
    /// Вызывается под `DEVICE.lock()`; virtqueue корректна и отображена.
    unsafe fn run_request(&mut self) -> Result<(), BlkError> {
        // Кладём голову цепочки (дескриптор 0) в «доступное» кольцо и двигаем idx.
        let avail = self.vq_virt + self.avail_offset as u64;
        let avail_idx_ptr = (avail + 2) as *mut u16;
        let avail_idx = core::ptr::read_volatile(avail_idx_ptr);
        let slot = (avail + 4 + (avail_idx % self.queue_size) as u64 * 2) as *mut u16;
        core::ptr::write_volatile(slot, 0);
        // Барьер: все записи дескрипторов/кольца должны быть видны устройству ДО продвижения
        // idx (иначе оно прочитает недописанную цепочку).
        fence(Ordering::SeqCst);
        core::ptr::write_volatile(avail_idx_ptr, avail_idx.wrapping_add(1));
        fence(Ordering::SeqCst);

        // Пинаем устройство: «в очереди 0 появилась работа».
        io::outw(self.io_base + REG_QUEUE_NOTIFY, 0);

        // Опрашиваем «использованное» кольцо, пока устройство не продвинет used.idx.
        let used_idx_ptr = (self.vq_virt + self.used_offset as u64 + 2) as *const u16;
        loop {
            fence(Ordering::SeqCst);
            let used_idx = core::ptr::read_volatile(used_idx_ptr);
            if used_idx != self.last_used_idx {
                self.last_used_idx = used_idx;
                break;
            }
            core::hint::spin_loop();
        }

        let status = core::ptr::read_volatile((self.buf_virt + BUF_STATUS) as *const u8);
        if status != 0 {
            return Err(BlkError::DeviceError(status));
        }
        Ok(())
    }

    /// Выполняет одно чтение сектора `lba` в `out` (см. [`read_sector`]).
    ///
    /// # Safety
    /// Вызывается под `DEVICE.lock()` (единственный запрос за раз); адреса virtqueue/буфера
    /// корректны и отображены.
    unsafe fn submit_read(
        &mut self,
        lba: u64,
        out: &mut [u8; SECTOR_SIZE],
    ) -> Result<(), BlkError> {
        // Чтение: данные пишет устройство (data_device_write = true).
        self.setup_chain(VIRTIO_BLK_T_IN, lba, true);
        self.run_request()?;
        // Копируем данные из bounce-буфера в буфер вызывающего.
        fence(Ordering::SeqCst);
        core::ptr::copy_nonoverlapping(
            (self.buf_virt + BUF_DATA) as *const u8,
            out.as_mut_ptr(),
            SECTOR_SIZE,
        );
        Ok(())
    }

    /// Выполняет одну запись сектора `lba` из `data` (см. [`write_sector`]).
    ///
    /// # Safety
    /// Вызывается под `DEVICE.lock()` (единственный запрос за раз); адреса virtqueue/буфера
    /// корректны и отображены.
    unsafe fn submit_write(&mut self, lba: u64, data: &[u8; SECTOR_SIZE]) -> Result<(), BlkError> {
        // Копируем данные вызывающего в bounce-буфер ДО постановки запроса (устройство их
        // оттуда прочитает).
        core::ptr::copy_nonoverlapping(
            data.as_ptr(),
            (self.buf_virt + BUF_DATA) as *mut u8,
            SECTOR_SIZE,
        );
        fence(Ordering::SeqCst);
        // Запись: данные читает устройство (data_device_write = false).
        self.setup_chain(VIRTIO_BLK_T_OUT, lba, false);
        self.run_request()
    }
}
