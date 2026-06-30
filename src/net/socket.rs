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
use smoltcp::socket::udp;
use smoltcp::wire::{IpAddress, IpEndpoint, Ipv4Address};
use spin::Mutex;

/// Хэндл сокета в общем наборе — то, что хранит fd кольца 3.
pub type Handle = smoltcp::iface::SocketHandle;

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
/// Слотов метаданных в буфере одного сокета (макс. датаграмм «в полёте»).
const SOCK_META: usize = 8;
/// Байт payload в буфере одного сокета (под/над запросы и ответы).
const SOCK_PAYLOAD: usize = 4096;

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

/// Удаляет сокет из стека — зовётся, когда закрылся последний fd на него (см. `Fd::Socket`).
/// Тихо игнорирует не поднятый стек (а до первого сокета хэндлов и не бывает).
pub fn udp_close(handle: Handle) {
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
