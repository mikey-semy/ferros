//! `net` — сетевой стек ферроса.
//!
//! По политике reuse (D13) **сам TCP/IP-стек берём готовый — `smoltcp`** (no_std, open-source): он
//! делает ARP/IPv4/UDP/DHCP. Сами пишем только драйвер NIC (`drivers::virtio_net`) и **тонкий слой
//! [`VirtioPhy`]**, реализующий трейт `smoltcp::phy::Device` поверх наших `send`/`recv` сырых кадров.
//!
//! M8c: поднимаем интерфейс и получаем IP по **DHCP** (см. [`dhcp_acquire`]). Ping/TCP — дальше.
//!
//! # Как smoltcp общается с железом
//!
//! smoltcp не знает про наш virtio. Он работает через `Device`: на каждый `poll` спрашивает «есть
//! принятый кадр?» (`receive` → `RxToken`, отдающий байты кадра) и «можно передать?» (`transmit` →
//! `TxToken`, в который smoltcp пишет кадр, а мы его шлём). Всё остальное (составить ARP/IP/UDP,
//! таймеры повторов) — внутри smoltcp.

use crate::arch::uptime_ns;
use crate::drivers::virtio_net;
use alloc::vec::Vec;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::dhcpv4;
use smoltcp::time::Instant;
use smoltcp::wire::EthernetAddress;

/// Максимальный размер Ethernet-кадра (без FCS) — MTU линии для smoltcp.
const MTU: usize = 1514;

/// Текущий момент времени для smoltcp — из аптайма ядра (PIT, [`uptime_ns`]).
fn now() -> Instant {
    Instant::from_micros((uptime_ns() / 1000) as i64)
}

/// Адаптер драйвера virtio-net к `smoltcp::phy::Device`: приём/передача сырых кадров.
///
/// Держит один приёмный буфер: `receive` опрашивает драйвер и, если кадр пришёл, кладёт его сюда и
/// отдаёт smoltcp через [`VirtioRxToken`]. Передача ([`VirtioTxToken`]) буфера в структуре не
/// требует — кадр живёт только на время `consume`.
pub struct VirtioPhy {
    rx: [u8; MTU + 2],
}

impl VirtioPhy {
    /// Новый адаптер (драйвер virtio-net должен быть уже поднят).
    pub fn new() -> Self {
        Self { rx: [0u8; MTU + 2] }
    }
}

impl Default for VirtioPhy {
    fn default() -> Self {
        Self::new()
    }
}

impl Device for VirtioPhy {
    type RxToken<'a> = VirtioRxToken<'a>;
    type TxToken<'a> = VirtioTxToken;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        // Опрашиваем драйвер: пришёл ли кадр? Если да — он уже в self.rx, длиной len.
        let len = virtio_net::recv(&mut self.rx)?;
        Some((VirtioRxToken(&self.rx[..len]), VirtioTxToken))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        // Передать можем всегда (драйвер шлёт синхронно).
        Some(VirtioTxToken)
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = MTU;
        caps.max_burst_size = Some(1); // принимаем/шлём по одному кадру за раз
        caps
    }
}

/// Принятый кадр: smoltcp читает его в `consume`.
pub struct VirtioRxToken<'a>(&'a [u8]);

impl RxToken for VirtioRxToken<'_> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(self.0)
    }
}

/// Разрешение на передачу: smoltcp пишет кадр в `consume`, мы его шлём.
pub struct VirtioTxToken;

impl TxToken for VirtioTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = [0u8; MTU + 2];
        let result = f(&mut buf[..len]); // smoltcp заполняет кадр
        virtio_net::send(&buf[..len]); // и мы его передаём
        result
    }
}

/// Поднимает интерфейс smoltcp поверх virtio-net, запускает DHCP-клиента и крутит `poll`, пока не
/// получит адрес или не истечёт `timeout_ns` (наносекунды аптайма). Возвращает выданный IPv4-адрес
/// октетами и длину префикса. `None` — драйвер не поднят (нет MAC) или таймаут.
pub fn dhcp_acquire(timeout_ns: u64) -> Option<([u8; 4], u8)> {
    let mac = virtio_net::mac()?;
    let mut device = VirtioPhy::new();
    // Интерфейс с нашим MAC (Ethernet). random_seed=0 по умолчанию — для DHCP transaction id ок.
    let config = Config::new(EthernetAddress(mac).into());
    let mut iface = Interface::new(config, &mut device, now());
    // Набор сокетов (на Vec — фича `alloc`); один DHCP-клиент.
    let mut sockets = SocketSet::new(Vec::new());
    let dhcp = sockets.add(dhcpv4::Socket::new());

    let deadline = uptime_ns() + timeout_ns;
    while uptime_ns() < deadline {
        // poll прогоняет приём/передачу: smoltcp забирает наши RX-кадры и шлёт свои (DISCOVER/REQUEST).
        iface.poll(now(), &mut device, &mut sockets);
        if let Some(dhcpv4::Event::Configured(cfg)) = sockets.get_mut::<dhcpv4::Socket>(dhcp).poll()
        {
            return Some((cfg.address.address().octets(), cfg.address.prefix_len()));
        }
        core::hint::spin_loop();
    }
    None
}
