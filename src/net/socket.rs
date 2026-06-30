//! Постоянный сетевой стек + операции UDP-сокетов под сисколлы кольца 3 (M8d3).
//!
//! В отличие от self-contained [`super::ping`]/[`super::resolve`] (каждый поднимает свой стек на
//! один запрос и выбрасывает), сокеты кольца 3 живут долго: процесс создаёт сокет, шлёт/принимает,
//! закрывает. Поэтому держим **один общий стек** — интерфейс smoltcp + наше устройство virtio +
//! набор живущих сокетов — в `Mutex`, поднимаем его **лениво по DHCP** при первом сокете и
//! **опрашиваем по требованию** внутри каждой операции (прерываний у сети нет — весь слой polled).
//!
//! fd кольца 3 хранит [`Handle`] в этот набор (см. `syscall::files::Fd::Socket`); send/recv находят
//! по нему сокет и гоняют стек, пока датаграмма не уйдёт/не придёт.
//!
//! # Чего тут пока нет (follow-up)
//!
//! Блокирующий `recvfrom` ждёт **опросом** (с верхней границей по времени), а не через
//! планировщик (block/wake, как канал/консоль) — для демо одного процесса достаточно, но при
//! нескольких сетевых процессах это расход CPU (см. HARDENING). Фоновый опрос (для входящих без
//! активного recv и продления аренды DHCP) тоже отложен.

use super::{bring_up, dhcp_configure, now, VirtioPhy};
use crate::arch::uptime_ns;
use crate::syscall::abi;
use core::sync::atomic::{AtomicU16, Ordering};
use smoltcp::iface::{Interface, SocketSet};
use smoltcp::socket::{tcp, udp};
use smoltcp::wire::{IpAddress, IpEndpoint, Ipv4Address};
use spin::Mutex;

/// Хэндл сокета в общем наборе — то, что хранит fd кольца 3.
pub type Handle = smoltcp::iface::SocketHandle;

/// Тип сокета за дескриптором: датаграммы (UDP) или поток (TCP). fd хранит его рядом с [`Handle`],
/// чтобы send/recv шли к нужным операциям, а `SocketSet::get_mut::<T>` — к нужному типу (иначе паника).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SockKind {
    /// `SOCK_DGRAM` — UDP.
    Udp,
    /// `SOCK_STREAM` — TCP.
    Tcp,
}

/// Бюджет аптайма на ленивый подъём стека (DHCP), нс.
const INIT_TIMEOUT_NS: u64 = 5_000_000_000;
/// Макс. итераций опроса в блокирующем `udp_recvfrom`. **Важно:** к стенным часам тут привязки нет —
/// сисколлы идут с IF=0, поэтому таймер (а с ним `uptime_ns`) на время вызова стоит. Ограничиваем
/// **счётом опросов** (программный таймаут, как в `virtio_net::send`), иначе при потере ответа цикл
/// завис бы навсегда. Грубо, но конечно; точный таймер — с переходом на block/wake (см. модуль).
const RECV_MAX_SPINS: u64 = 50_000_000;
/// Низ эфемерного диапазона портов (как у Linux: 49152..=65535).
const EPHEMERAL_BASE: u16 = 49152;
/// Размер эфемерного диапазона (65536 − 49152).
const EPHEMERAL_SPAN: u16 = 16384;
/// Слотов метаданных в буфере одного UDP-сокета (макс. датаграмм «в полёте»).
const SOCK_META: usize = 8;
/// Байт payload в буфере одного UDP-сокета (под/над запросы и ответы).
const SOCK_PAYLOAD: usize = 4096;
/// Размер приёмного/передающего кольца одного TCP-сокета (поток — берём с запасом под пару сегментов).
const TCP_BUF: usize = 8192;
/// Предел опросов на установление TCP-соединения (handshake). Как [`RECV_MAX_SPINS`] — счёт, а не
/// секунды (таймер под IF=0 стоит): handshake — пара RTT, укладывается в малую долю бюджета.
const CONNECT_MAX_SPINS: u64 = 100_000_000;

/// Счётчик для выдачи эфемерных портов по кругу — чтобы два неприсвязанных сокета на общем стеке
/// не запросили один и тот же порт (фиксированный порт был бы фут-ганом).
static EPHEMERAL_CTR: AtomicU16 = AtomicU16::new(0);

/// Следующий эфемерный локальный порт (всегда в диапазоне 49152..=65535).
fn next_ephemeral_port() -> u16 {
    EPHEMERAL_BASE + EPHEMERAL_CTR.fetch_add(1, Ordering::Relaxed) % EPHEMERAL_SPAN
}

/// Общий сетевой стек: интерфейс smoltcp, наше устройство virtio и набор живущих сокетов.
struct NetStack {
    iface: Interface,
    device: VirtioPhy,
    sockets: SocketSet<'static>,
}

static STACK: Mutex<Option<NetStack>> = Mutex::new(None);

/// Лениво поднимает общий стек (DHCP), если ещё не поднят. `true` — стек готов. Идемпотентно.
fn ensure_up(slot: &mut Option<NetStack>) -> bool {
    if slot.is_some() {
        return true;
    }
    let Some((mut device, mut iface, mut sockets)) = bring_up() else {
        return false;
    };
    let deadline = uptime_ns() + INIT_TIMEOUT_NS;
    if dhcp_configure(&mut iface, &mut device, &mut sockets, deadline).is_none() {
        return false;
    }
    *slot = Some(NetStack {
        iface,
        device,
        sockets,
    });
    true
}

/// Создаёт UDP-сокет в общем стеке (подняв его при необходимости) и возвращает хэндл.
/// `Err(ENETDOWN)` — сеть не поднялась (нет NIC / DHCP не настроился).
pub fn udp_socket() -> Result<Handle, i64> {
    let mut guard = STACK.lock();
    if !ensure_up(&mut guard) {
        return Err(abi::ENETDOWN);
    }
    let stack = guard.as_mut().unwrap();
    let rx = udp::PacketBuffer::new(
        alloc::vec![udp::PacketMetadata::EMPTY; SOCK_META],
        alloc::vec![0u8; SOCK_PAYLOAD],
    );
    let tx = udp::PacketBuffer::new(
        alloc::vec![udp::PacketMetadata::EMPTY; SOCK_META],
        alloc::vec![0u8; SOCK_PAYLOAD],
    );
    Ok(stack.sockets.add(udp::Socket::new(rx, tx)))
}

/// Удаляет сокет (любого типа) из стека — зовётся, когда закрылся последний fd на него (см.
/// `Fd::Socket`). Тихо игнорирует не поднятый стек (а до первого сокета хэндлов и не бывает).
/// `SocketSet::remove` снимает запись по хэндлу независимо от типа сокета.
pub fn close_socket(handle: Handle) {
    let mut guard = STACK.lock();
    if let Some(stack) = guard.as_mut() {
        stack.sockets.remove(handle);
    }
}

/// Привязывает сокет к локальному порту (`bind`). `Err(EINVAL)` — порт занят/сокет уже открыт.
pub fn udp_bind(handle: Handle, port: u16) -> Result<(), i64> {
    let mut guard = STACK.lock();
    let stack = guard.as_mut().ok_or(abi::ENETDOWN)?;
    stack
        .sockets
        .get_mut::<udp::Socket>(handle)
        .bind(port)
        .map_err(|_| abi::EINVAL)
}

/// Шлёт датаграмму `data` на `dst_ip:dst_port`. Неприсвязанный сокет авто-привязывается к
/// эфемерному порту (как POSIX `sendto`). Возвращает число отправленных байт. Стек прогоняется
/// один раз (kick): если ARP к адресату ещё не разрешён, датаграмма уйдёт на последующих опросах
/// (их делает `udp_recvfrom`) — типичный порядок send→recv это покрывает.
pub fn udp_sendto(
    handle: Handle,
    data: &[u8],
    dst_ip: [u8; 4],
    dst_port: u16,
) -> Result<usize, i64> {
    let mut guard = STACK.lock();
    let stack = guard.as_mut().ok_or(abi::ENETDOWN)?;
    let endpoint = IpEndpoint {
        addr: IpAddress::Ipv4(Ipv4Address::new(dst_ip[0], dst_ip[1], dst_ip[2], dst_ip[3])),
        port: dst_port,
    };
    {
        let socket = stack.sockets.get_mut::<udp::Socket>(handle);
        if socket.endpoint().port == 0 {
            socket
                .bind(next_ephemeral_port())
                .map_err(|_| abi::EINVAL)?;
        }
        socket
            .send_slice(data, endpoint)
            .map_err(|_| abi::EMSGSIZE)?;
    }
    stack
        .iface
        .poll(now(), &mut stack.device, &mut stack.sockets);
    Ok(data.len())
}

/// Принимает датаграмму в `buf` (блокирующе, опросом). Возвращает число записанных байт (датаграмма
/// усекается по длине `buf`, как POSIX) и адрес отправителя (октеты + порт). `Err(EAGAIN)` — за
/// [`RECV_MAX_SPINS`] опросов ничего не пришло (бюджет в опросах, а не в секундах: см. там — таймер
/// под IF=0 стоит).
pub fn udp_recvfrom(handle: Handle, buf: &mut [u8]) -> Result<(usize, [u8; 4], u16), i64> {
    let mut spins = 0u64;
    loop {
        // Короткая критическая секция: опрос + попытка снять датаграмму. Между итерациями замок
        // отпущен — другой процесс тоже может работать со стеком.
        {
            let mut guard = STACK.lock();
            let stack = guard.as_mut().ok_or(abi::ENETDOWN)?;
            stack
                .iface
                .poll(now(), &mut stack.device, &mut stack.sockets);
            let socket = stack.sockets.get_mut::<udp::Socket>(handle);
            if socket.can_recv() {
                if let Ok((payload, meta)) = socket.recv() {
                    let n = payload.len().min(buf.len());
                    buf[..n].copy_from_slice(&payload[..n]);
                    let ip = match meta.endpoint.addr {
                        IpAddress::Ipv4(a) => a.octets(),
                    };
                    return Ok((n, ip, meta.endpoint.port));
                }
            }
        }
        spins += 1;
        if spins >= RECV_MAX_SPINS {
            return Err(abi::EAGAIN);
        }
        core::hint::spin_loop();
    }
}

/// Создаёт TCP-сокет в общем стеке (подняв его при необходимости). `Err(ENETDOWN)` — нет сети.
pub fn tcp_socket() -> Result<Handle, i64> {
    let mut guard = STACK.lock();
    if !ensure_up(&mut guard) {
        return Err(abi::ENETDOWN);
    }
    let stack = guard.as_mut().unwrap();
    let rx = tcp::SocketBuffer::new(alloc::vec![0u8; TCP_BUF]);
    let tx = tcp::SocketBuffer::new(alloc::vec![0u8; TCP_BUF]);
    Ok(stack.sockets.add(tcp::Socket::new(rx, tx)))
}

/// Устанавливает TCP-соединение с `dst_ip:dst_port` (блокирующе опросом — трёхстороннее рукопожатие).
/// `Err`: `EINVAL` (плохой адрес/состояние сокета), `ECONNREFUSED` (RST / закрылось не установившись),
/// `ETIMEDOUT` (рукопожатие не уложилось в бюджет опросов), `ENETDOWN` (стек пропал).
pub fn tcp_connect(handle: Handle, dst_ip: [u8; 4], dst_port: u16) -> Result<(), i64> {
    let remote = IpEndpoint {
        addr: IpAddress::Ipv4(Ipv4Address::new(dst_ip[0], dst_ip[1], dst_ip[2], dst_ip[3])),
        port: dst_port,
    };
    let local_port = next_ephemeral_port();
    // Инициируем соединение: connect берёт контекст интерфейса (для выбора source-адреса и ISN) и
    // переводит сокет в SynSent. Реальное рукопожатие пойдёт на опросах ниже.
    {
        let mut guard = STACK.lock();
        let stack = guard.as_mut().ok_or(abi::ENETDOWN)?;
        let cx = stack.iface.context();
        stack
            .sockets
            .get_mut::<tcp::Socket>(handle)
            .connect(cx, remote, local_port)
            .map_err(|_| abi::EINVAL)?;
    }
    let mut spins = 0u64;
    loop {
        {
            let mut guard = STACK.lock();
            let stack = guard.as_mut().ok_or(abi::ENETDOWN)?;
            stack
                .iface
                .poll(now(), &mut stack.device, &mut stack.sockets);
            let socket = stack.sockets.get_mut::<tcp::Socket>(handle);
            if socket.may_send() {
                return Ok(()); // ESTABLISHED — можно слать
            }
            if !socket.is_active() {
                return Err(abi::ECONNREFUSED); // закрылось, не установившись
            }
        }
        spins += 1;
        if spins >= CONNECT_MAX_SPINS {
            return Err(abi::ETIMEDOUT);
        }
        core::hint::spin_loop();
    }
}

/// Шлёт **весь** `data` в установленный TCP-сокет (блокирующе опросом — ставит в очередь, прогоняет
/// стек, повторяет, пока всё не уйдёт в передающее кольцо). Возвращает число отправленных байт —
/// обычно `data.len`; меньше только если вышел бюджет опросов ([`RECV_MAX_SPINS`]). `Err(ENOTCONN)` —
/// соединение не установлено / передающая половина закрыта (и ничего не успели отправить).
///
/// Почему «блокирующе, а не один `send_slice`»: при полном передающем кольце `send_slice` вернул бы
/// `0`, и наивный пользовательский цикл `while sent < len { sent += send() }` крутился бы впустую.
pub fn tcp_send(handle: Handle, data: &[u8]) -> Result<usize, i64> {
    let mut sent = 0usize;
    let mut spins = 0u64;
    loop {
        {
            let mut guard = STACK.lock();
            let stack = guard.as_mut().ok_or(abi::ENETDOWN)?;
            {
                let socket = stack.sockets.get_mut::<tcp::Socket>(handle);
                if !socket.may_send() {
                    // Не установлено / передающая половина закрыта.
                    return if sent > 0 {
                        Ok(sent)
                    } else {
                        Err(abi::ENOTCONN)
                    };
                }
                if socket.can_send() {
                    match socket.send_slice(&data[sent..]) {
                        Ok(n) => sent += n,
                        Err(_) => {
                            return if sent > 0 {
                                Ok(sent)
                            } else {
                                Err(abi::ENOTCONN)
                            }
                        }
                    }
                }
            }
            // Прогоняем стек: отправляем поставленное в очередь и продвигаем окно по ACK.
            stack
                .iface
                .poll(now(), &mut stack.device, &mut stack.sockets);
        }
        if sent >= data.len() {
            return Ok(sent);
        }
        spins += 1;
        if spins >= RECV_MAX_SPINS {
            return Ok(sent); // частично за бюджет опросов — вызывающий дошлёт остаток
        }
        core::hint::spin_loop();
    }
}

/// Принимает из TCP-сокета в `buf` (блокирующе опросом). Возвращает число прочитанных байт; `0` —
/// пир закрыл соединение (EOF). `Err(EAGAIN)` — за бюджет опросов ничего не пришло.
pub fn tcp_recv(handle: Handle, buf: &mut [u8]) -> Result<usize, i64> {
    let mut spins = 0u64;
    loop {
        {
            let mut guard = STACK.lock();
            let stack = guard.as_mut().ok_or(abi::ENETDOWN)?;
            stack
                .iface
                .poll(now(), &mut stack.device, &mut stack.sockets);
            let socket = stack.sockets.get_mut::<tcp::Socket>(handle);
            if socket.can_recv() {
                let n = socket.recv_slice(buf).unwrap_or(0);
                return Ok(n);
            }
            if !socket.may_recv() {
                return Ok(0); // пир закрыл, данных больше нет — EOF
            }
        }
        spins += 1;
        if spins >= RECV_MAX_SPINS {
            return Err(abi::EAGAIN);
        }
        core::hint::spin_loop();
    }
}
