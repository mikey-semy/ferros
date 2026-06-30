//! `net` — сетевой стек ферроса.
//!
//! По политике reuse (D13) **сам TCP/IP-стек берём готовый — `smoltcp`** (no_std, open-source): он
//! делает ARP/IPv4/UDP/DHCP. Сами пишем только драйвер NIC (`drivers::virtio_net`) и **тонкий слой
//! [`VirtioPhy`]**, реализующий трейт `smoltcp::phy::Device` поверх наших `send`/`recv` сырых кадров.
//!
//! M8c: поднимаем интерфейс и получаем IP по **DHCP** (см. [`dhcp_acquire`]).
//! M8d1: умеем **ping** — шлём ICMP echo и считаем ответы (см. [`ping`]).
//! M8d2: умеем **резолвить имена** — DNS-запрос через UDP-сокет (см. [`resolve`]).
//!
//! # Как smoltcp общается с железом
//!
//! smoltcp не знает про наш virtio. Он работает через `Device`: на каждый `poll` спрашивает «есть
//! принятый кадр?» (`receive` → `RxToken`, отдающий байты кадра) и «можно передать?» (`transmit` →
//! `TxToken`, в который smoltcp пишет кадр, а мы его шлём). Всё остальное (составить ARP/IP/UDP,
//! таймеры повторов) — внутри smoltcp.

mod dns;
pub mod socket;

use crate::arch::uptime_ns;
use crate::drivers::virtio_net;
use alloc::vec::Vec;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{ChecksumCapabilities, Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::{dhcpv4, icmp, udp};
use smoltcp::time::Instant;
use smoltcp::wire::{
    EthernetAddress, Icmpv4Packet, Icmpv4Repr, IpAddress, IpCidr, IpEndpoint, Ipv4Address,
};

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

/// Выданный DHCP адрес: октеты и длина префикса. Шлюз/маршрут по умолчанию [`dhcp_configure`]
/// применяет прямо к интерфейсу (а не хранит здесь) — наружу нужен только сам адрес.
struct DhcpLease {
    addr: [u8; 4],
    prefix: u8,
}

/// Предел итераций опроса DHCP — страховка от зависания. Нужен потому, что `dhcp_configure`
/// зовётся и из сисколла `socket()` (ленивый подъём стека), а там IF=0 и таймер стоит, так что
/// `deadline` по `uptime_ns` не сработает. Счёт опросов от часов не зависит. Велик с запасом: удачный
/// DHCP укладывается в тысячи итераций, а это — лишь верхняя граница на случай «сервер не ответил».
const DHCP_MAX_SPINS: u64 = 200_000_000;

/// Запускает DHCP-клиента на уже поднятом интерфейсе и крутит `poll`, пока не получит конфигурацию
/// или не выйдет бюджет: `deadline` (абсолютный аптайм, нс — работает, когда IF=1) **или**
/// [`DHCP_MAX_SPINS`] опросов (страховка, когда часы стоят под IF=0). При успехе **применяет**
/// конфиг к интерфейсу: ставит наш IP и маршрут по умолчанию (шлюз) — это и есть «настроенная сеть»,
/// поверх которой работают ICMP/UDP-сокеты. DHCP-сокет на время добавляется в `sockets` и снимается.
fn dhcp_configure(
    iface: &mut Interface,
    device: &mut VirtioPhy,
    sockets: &mut SocketSet,
    deadline: u64,
) -> Option<DhcpLease> {
    let dhcp = sockets.add(dhcpv4::Socket::new());
    let mut spins = 0u64;
    let lease = loop {
        if uptime_ns() >= deadline || spins >= DHCP_MAX_SPINS {
            break None;
        }
        spins += 1;
        // poll прогоняет приём/передачу: smoltcp забирает наши RX-кадры и шлёт свои (DISCOVER/REQUEST).
        iface.poll(now(), device, sockets);
        if let Some(dhcpv4::Event::Configured(cfg)) = sockets.get_mut::<dhcpv4::Socket>(dhcp).poll()
        {
            // Наш адрес на интерфейс (нужен как source для исходящих пакетов).
            iface.update_ip_addrs(|addrs| {
                let _ = addrs.push(IpCidr::Ipv4(cfg.address));
            });
            // Маршрут по умолчанию (шлюз) — чтобы потом ходить за пределы подсети.
            if let Some(router) = cfg.router {
                let _ = iface.routes_mut().add_default_ipv4_route(router);
            }
            break Some(DhcpLease {
                addr: cfg.address.address().octets(),
                prefix: cfg.address.prefix_len(),
            });
        }
        core::hint::spin_loop();
    };
    sockets.remove(dhcp);
    lease
}

/// Поднимает интерфейс smoltcp поверх virtio-net: `Device` из нашего MAC, интерфейс и пустой набор
/// сокетов (на `Vec` — фича `alloc`). `None`, если драйвер сети не поднят (нет MAC). Общая стартовая
/// часть всех сетевых операций (DHCP, ping, дальше TCP) — чтобы не дублировать её в каждой.
fn bring_up() -> Option<(VirtioPhy, Interface, SocketSet<'static>)> {
    let mac = virtio_net::mac()?;
    let mut device = VirtioPhy::new();
    // Интерфейс с нашим MAC (Ethernet). random_seed=0 по умолчанию — для DHCP transaction id ок.
    // `Interface` не держит ссылку на `device` (poll берёт его заново), поэтому device возвращаем.
    let config = Config::new(EthernetAddress(mac).into());
    let iface = Interface::new(config, &mut device, now());
    Some((device, iface, SocketSet::new(Vec::new())))
}

/// Поднимает интерфейс smoltcp поверх virtio-net, запускает DHCP-клиента и крутит `poll`, пока не
/// получит адрес или не истечёт `timeout_ns` (наносекунды аптайма). Возвращает выданный IPv4-адрес
/// октетами и длину префикса. `None` — драйвер не поднят (нет MAC) или таймаут.
pub fn dhcp_acquire(timeout_ns: u64) -> Option<([u8; 4], u8)> {
    let (mut device, mut iface, mut sockets) = bring_up()?;
    let deadline = uptime_ns() + timeout_ns;
    let lease = dhcp_configure(&mut iface, &mut device, &mut sockets, deadline)?;
    Some((lease.addr, lease.prefix))
}

/// Итог серии ping: сколько echo-запросов ушло и сколько ответов вернулось.
#[derive(Debug, Clone, Copy)]
pub struct PingStats {
    /// Отправлено echo-запросов.
    pub sent: usize,
    /// Получено совпавших echo-ответов.
    pub received: usize,
}

/// Пингует `dest` (IPv4): получает IP по DHCP, шлёт `count` ICMP echo-запросов и считает ответы.
/// `timeout_ns` — общий бюджет аптайма (на DHCP и на сам обмен). `None`, если драйвер не поднят
/// или DHCP не настроился; иначе [`PingStats`] (даже если ответов 0 — это уже наблюдаемый итог).
///
/// Так выглядит классический `ping`: ICMP-сокет smoltcp шлёт **Echo Request** и принимает
/// **Echo Reply**; адресат на нашей подсети (10.0.2.x) — smoltcp сам сделает ARP к нему. SLIRP
/// QEMU отвечает на ping своего шлюза 10.0.2.2 (внутри SLIRP, без участия хоста) — поэтому тест
/// детерминирован.
pub fn ping(dest: [u8; 4], count: usize, timeout_ns: u64) -> Option<PingStats> {
    let count = count.max(1);
    let (mut device, mut iface, mut sockets) = bring_up()?;

    let deadline = uptime_ns() + timeout_ns;
    dhcp_configure(&mut iface, &mut device, &mut sockets, deadline)?;

    // ICMP-сокет: слот метаданных + с запасом байт на каждый ожидаемый пакет (echo = 8 байт
    // заголовка + 16 байт нагрузки = 24, 256 — заведомо больше). Все `count` пакетов вмещаются.
    let rx = icmp::PacketBuffer::new(
        alloc::vec![icmp::PacketMetadata::EMPTY; count],
        alloc::vec![0u8; 256 * count],
    );
    let tx = icmp::PacketBuffer::new(
        alloc::vec![icmp::PacketMetadata::EMPTY; count],
        alloc::vec![0u8; 256 * count],
    );
    let mut socket = icmp::Socket::new(rx, tx);
    // Привязываемся к ICMP-идентификатору: ядро так отбирает «свои» echo-ответы (как PID у ping(8)).
    let ident: u16 = 0x22b8;
    socket.bind(icmp::Endpoint::Ident(ident)).ok()?;
    let handle = sockets.add(socket);

    let dest_ip = IpAddress::Ipv4(Ipv4Address::new(dest[0], dest[1], dest[2], dest[3]));
    let payload = b"ferros-icmp-ping"; // 16 байт полезной нагрузки
    let checksum = ChecksumCapabilities::default(); // наше железо не считает контрольные суммы — это делает smoltcp

    let mut sent = 0usize;
    let mut received = 0usize;
    // Какой seq уже ответил — чтобы дубликат ответа не накручивал счётчик (на каждый запрос свой seq).
    let mut answered = alloc::vec![false; count];

    while received < count && uptime_ns() < deadline {
        iface.poll(now(), &mut device, &mut sockets);
        let socket = sockets.get_mut::<icmp::Socket>(handle);

        // Ставим в очередь оставшиеся запросы (seq_no = порядковый номер), пока есть место в буфере.
        // smoltcp придержит их, пока не разрешит ARP к шлюзу, и отправит на следующих poll.
        while sent < count && socket.can_send() {
            let repr = Icmpv4Repr::EchoRequest {
                ident,
                seq_no: sent as u16,
                data: payload,
            };
            let Ok(buf) = socket.send(repr.buffer_len(), dest_ip) else {
                break;
            };
            repr.emit(&mut Icmpv4Packet::new_unchecked(buf), &checksum);
            sent += 1;
        }

        // Снимаем ответы: по одному на каждый наш seq (дубликаты не считаем дважды).
        while socket.can_recv() {
            let Ok((data, _src)) = socket.recv() else {
                break;
            };
            let Ok(packet) = Icmpv4Packet::new_checked(data) else {
                continue;
            };
            if let Ok(Icmpv4Repr::EchoReply { seq_no, .. }) = Icmpv4Repr::parse(&packet, &checksum)
            {
                // smoltcp уже отфильтровал по ident (мы привязаны к Ident); проверяем seq и дубли.
                let i = seq_no as usize;
                if i < count && !answered[i] {
                    answered[i] = true;
                    received += 1;
                }
            }
        }
        core::hint::spin_loop();
    }

    Some(PingStats { sent, received })
}

/// Локальный (эфемерный) UDP-порт, с которого шлём DNS-запрос.
const DNS_LOCAL_PORT: u16 = 49152;
/// Размер буферов под DNS-сообщение. Ответ без EDNS по UDP ограничен 512 байтами (RFC 1035), но
/// берём с запасом до одного Ethernet-MTU — тогда и редкий крупный ответ не обрежется (обрезанный
/// `recv_slice` отбросил бы пакет, и резолв впустую досидел бы до таймаута).
const DNS_MSG_LEN: usize = 1500;

/// Резолвит `hostname` в IPv4 (A-запись) через DNS-сервер `dns_server` (UDP, порт 53). Поднимает
/// сеть по DHCP, шлёт один DNS-запрос и разбирает ответ. `timeout_ns` — общий бюджет аптайма (на
/// DHCP и на обмен). `None` — драйвер не поднят, DHCP не настроился, либо за отведённое время не
/// пришло ответа с A-записью.
///
/// Сам DNS (собрать запрос / разобрать ответ) — наш модуль `dns`; транспорт — UDP-сокет smoltcp.
/// Это тот же путь `bind`/`send`/`recv`, который дальше обернут сокет-сисколлы кольца 3 (M8d3): в
/// SLIRP DNS-сервер живёт на 10.0.2.3, туда и спрашиваем.
pub fn resolve(hostname: &str, dns_server: [u8; 4], timeout_ns: u64) -> Option<[u8; 4]> {
    let (mut device, mut iface, mut sockets) = bring_up()?;
    let deadline = uptime_ns() + timeout_ns;
    dhcp_configure(&mut iface, &mut device, &mut sockets, deadline)?;

    // UDP-сокет под DNS: буферы на одно сообщение (см. DNS_MSG_LEN), пары слотов метаданных хватает.
    let rx = udp::PacketBuffer::new(
        alloc::vec![udp::PacketMetadata::EMPTY; 4],
        alloc::vec![0u8; DNS_MSG_LEN],
    );
    let tx = udp::PacketBuffer::new(
        alloc::vec![udp::PacketMetadata::EMPTY; 4],
        alloc::vec![0u8; DNS_MSG_LEN],
    );
    let mut socket = udp::Socket::new(rx, tx);
    socket.bind(DNS_LOCAL_PORT).ok()?;
    let handle = sockets.add(socket);

    let server = IpEndpoint {
        addr: IpAddress::Ipv4(Ipv4Address::new(
            dns_server[0],
            dns_server[1],
            dns_server[2],
            dns_server[3],
        )),
        port: 53,
    };
    // Транзакционный id меняем от запроса к запросу (из аптайма) — чтобы устаревший ответ от
    // прошлого резолва не сошёл за наш; `parse_answer` сверяет именно его.
    let id = uptime_ns() as u16;
    let mut query = [0u8; DNS_MSG_LEN];
    let qlen = dns::build_query(id, hostname, &mut query)?;

    let mut asked = false;
    while uptime_ns() < deadline {
        iface.poll(now(), &mut device, &mut sockets);
        let socket = sockets.get_mut::<udp::Socket>(handle);

        // Один раз отправляем запрос, как только сокет готов слать (после разрешения ARP к серверу).
        if !asked && socket.can_send() && socket.send_slice(&query[..qlen], server).is_ok() {
            asked = true;
        }

        // Разбираем пришедшие датаграммы. Свой ответ — итог определённый: A-запись или «адреса нет»
        // (тогда не ждём таймаут впустую). Чужие/битые — пропускаем и читаем следующую (не бросаем
        // дренаж: обрезанный recv_slice уже снял пакет, продолжаем со следующего).
        while socket.can_recv() {
            let mut buf = [0u8; DNS_MSG_LEN];
            let Ok((len, _meta)) = socket.recv_slice(&mut buf) else {
                continue;
            };
            match dns::parse_answer(id, &buf[..len]) {
                dns::Answer::Ipv4(addr) => return Some(addr),
                dns::Answer::NoIpv4 => return None,
                dns::Answer::Ignore => {}
            }
        }
        core::hint::spin_loop();
    }
    None
}
