//! Минимальный DNS: собрать запрос **A-записи** и вынуть из ответа первый IPv4 (M8d2).
//!
//! Это ровно столько DNS, сколько нужно, чтобы превратить имя в адрес через UDP-сокет: одна
//! функция собирает запрос, другая разбирает ответ. Полноценный резолвер (CNAME-цепочки, несколько
//! серверов, повторы, кэш) — отдельный уровень, и по политике reuse его взяли бы готовым; здесь же
//! формат на проводе простой и понятный, поэтому пишем сами (заодно видно, как DNS устроен).
//!
//! # Формат (RFC 1035, очень кратко)
//!
//! Сообщение = **заголовок (12 байт)** + секции. Заголовок: `ID`, флаги, и счётчики записей в
//! каждой секции (`QDCOUNT`/`ANCOUNT`/…). Дальше идёт секция вопросов, затем ответов. **Имя**
//! кодируется метками «длина + байты», конец — нулевой байт; в ответах имя часто **сжато** — вместо
//! меток стоит 2-байтовый указатель (старшие два бита первого байта = 1) на имя выше по сообщению.
//! Запись-ответ: имя, `TYPE`(2), `CLASS`(2), `TTL`(4), `RDLENGTH`(2), `RDATA`. Для A-записи
//! `TYPE=1`, `RDLENGTH=4`, `RDATA` — это и есть IPv4.

/// Тип записи **A** (IPv4-адрес).
const QTYPE_A: u16 = 1;
/// Класс **IN** (Internet).
const QCLASS_IN: u16 = 1;

/// Дописывает `bytes` в `out` начиная с `*pos`, сдвигая `*pos`. `None` — не влезло.
fn write(out: &mut [u8], pos: &mut usize, bytes: &[u8]) -> Option<()> {
    let end = pos.checked_add(bytes.len())?;
    if end > out.len() {
        return None;
    }
    out[*pos..end].copy_from_slice(bytes);
    *pos = end;
    Some(())
}

/// Кодирует DNS-запрос A-записи для `hostname` с идентификатором `id` в буфер `out`.
/// Возвращает длину запроса в байтах или `None` (имя некорректно / буфер мал).
pub fn build_query(id: u16, hostname: &str, out: &mut [u8]) -> Option<usize> {
    let mut pos = 0usize;
    // Заголовок (12 байт). ID запроса:
    write(out, &mut pos, &id.to_be_bytes())?;
    // Флаги 0x0100 — стандартный запрос с RD («нужна рекурсия»):
    write(out, &mut pos, &[0x01, 0x00])?;
    // Счётчики секций: QDCOUNT=1 (один вопрос), AN/NS/AR=0 (прочие секции пусты):
    write(out, &mut pos, &[0x00, 0x01])?;
    write(out, &mut pos, &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00])?;
    // QNAME: каждая метка = длина + байты; завершается нулём («корень»). Хвостовую точку FQDN
    // (`example.com.`) принимаем — это та же запись с явным корнем; внутренние пустые метки
    // (двойная точка, ведущая точка) остаются ошибкой.
    let hostname = hostname.strip_suffix('.').unwrap_or(hostname);
    for label in hostname.split('.') {
        if label.is_empty() || label.len() > 63 {
            return None; // пустая метка (напр. двойная точка / хвостовая) или слишком длинная
        }
        write(out, &mut pos, &[label.len() as u8])?;
        write(out, &mut pos, label.as_bytes())?;
    }
    write(out, &mut pos, &[0x00])?; // конец имени
    write(out, &mut pos, &QTYPE_A.to_be_bytes())?;
    write(out, &mut pos, &QCLASS_IN.to_be_bytes())?;
    Some(pos)
}

/// Перешагивает DNS-имя начиная с `pos`; возвращает позицию сразу после него. Сжатое имя
/// (указатель `0xC0…`) занимает 2 байта и на этом кончается — саму цель указателя не разбираем
/// (для пропуска не нужно). `None` — выход за пределы буфера.
fn skip_name(msg: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let len = *msg.get(pos)?;
        if len & 0xc0 == 0xc0 {
            msg.get(pos + 1)?; // второй байт указателя должен существовать
            return Some(pos + 2);
        }
        if len == 0 {
            return Some(pos + 1); // корень — конец имени
        }
        pos = pos.checked_add(1 + len as usize)?;
    }
}

/// Итог разбора одной DNS-датаграммы (см. [`parse_answer`]).
pub enum Answer {
    /// Это ответ на наш запрос и в нём есть A-запись — вот IPv4.
    Ipv4([u8; 4]),
    /// Это ответ на наш запрос (`id` совпал), но A-записи нет (`RCODE != 0`, только CNAME/AAAA,
    /// пустой ответ, или пакет битый): спрашивать дальше бессмысленно — итог определённый.
    NoIpv4,
    /// Датаграмма не на наш запрос (чужой `id` / слишком короткая) — игнорируем, ждём свою.
    Ignore,
}

/// Разбирает DNS-датаграмму `msg` как ответ на наш запрос с идентификатором `id`. Возвращает
/// [`Answer`]: совпал `id` → итог **определённый** ([`Answer::Ipv4`] или [`Answer::NoIpv4`], сколько
/// бы ни ждали, лучше не станет); не совпал → [`Answer::Ignore`] (продолжаем ждать). Так вызывающий
/// не крутит таймаут впустую на определённом «адреса нет».
pub fn parse_answer(id: u16, msg: &[u8]) -> Answer {
    if msg.len() < 12 || u16::from_be_bytes([msg[0], msg[1]]) != id {
        return Answer::Ignore; // слишком короткая / чужой id
    }
    // id совпал — это ответ на наш запрос; дальше итог определён (адрес либо его отсутствие).
    match find_a(msg) {
        Some(addr) => Answer::Ipv4(addr),
        None => Answer::NoIpv4,
    }
}

/// Ищет в ответе `msg` первую A-запись (`TYPE=A`, `CLASS=IN`, `RDLENGTH=4`) и возвращает её IPv4.
/// `None`, если сервер вернул ошибку (`RCODE != 0`), A-записи нет, или пакет битый. Разбор
/// полностью **bounds-safe**: любой выход за буфер даёт `None`, а не панику (пакет из сети —
/// доверять ему нельзя).
fn find_a(msg: &[u8]) -> Option<[u8; 4]> {
    if msg[3] & 0x0f != 0 {
        return None; // RCODE != 0 — сервер вернул ошибку
    }
    let qdcount = u16::from_be_bytes([msg[4], msg[5]]);
    let ancount = u16::from_be_bytes([msg[6], msg[7]]);

    let mut pos = 12usize;
    // Пропускаем секцию вопросов: на каждый вопрос QNAME + QTYPE(2) + QCLASS(2).
    for _ in 0..qdcount {
        pos = skip_name(msg, pos)?;
        pos = pos.checked_add(4)?;
    }
    // Идём по ответам, возвращаем первую A-запись класса IN.
    for _ in 0..ancount {
        pos = skip_name(msg, pos)?;
        let rtype = u16::from_be_bytes([*msg.get(pos)?, *msg.get(pos + 1)?]);
        let rclass = u16::from_be_bytes([*msg.get(pos + 2)?, *msg.get(pos + 3)?]);
        let rdlen = u16::from_be_bytes([*msg.get(pos + 8)?, *msg.get(pos + 9)?]) as usize;
        let rdata = pos.checked_add(10)?;
        let rend = rdata.checked_add(rdlen)?;
        if rend > msg.len() {
            return None;
        }
        if rtype == QTYPE_A && rclass == QCLASS_IN && rdlen == 4 {
            return Some([msg[rdata], msg[rdata + 1], msg[rdata + 2], msg[rdata + 3]]);
        }
        pos = rend; // не наша A-запись — перешагиваем RDATA и смотрим следующую
    }
    None
}
