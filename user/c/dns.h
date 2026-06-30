/*
 * Минимальный DNS на проводе для демо-клиентов (M8d3/M8e): собрать запрос A-записи и вынуть из
 * ответа первый IPv4. Общий для UDP- и TCP-демо — формат сообщения один; различаются только
 * транспорт и, у TCP, 2-байтовый префикс длины (его демо добавляет/снимает сами, см. RFC 1035 §4.2.2).
 * Функции `static` — чтобы делиться через заголовок без отдельного объектника.
 */
#ifndef FERROS_DNS_H
#define FERROS_DNS_H

/* Собрать DNS-запрос A-записи для `name` с идентификатором `id` в `out` (вместимость `cap` байт).
   Возвращает длину запроса или -1 (имя некорректно / не влезло в `cap`). */
static int dns_build(unsigned short id, const char *name, unsigned char *out, int cap) {
    int n = 0;
    if (cap < 12) {
        return -1; /* не помещается даже заголовок */
    }
    out[n++] = (unsigned char)(id >> 8);
    out[n++] = (unsigned char)id;
    out[n++] = 0x01; /* флаги: RD (нужна рекурсия) */
    out[n++] = 0x00;
    out[n++] = 0x00;
    out[n++] = 0x01; /* QDCOUNT = 1 */
    out[n++] = 0x00;
    out[n++] = 0x00; /* ANCOUNT */
    out[n++] = 0x00;
    out[n++] = 0x00; /* NSCOUNT */
    out[n++] = 0x00;
    out[n++] = 0x00; /* ARCOUNT */
    const char *p = name;
    while (*p) {
        const char *dot = p;
        while (*dot && *dot != '.') {
            dot++;
        }
        int len = (int)(dot - p);
        if (len == 0 || len > 63) {
            return -1; /* пустая (двойная/ведущая точка) или слишком длинная метка */
        }
        if (n + 1 + len > cap) {
            return -1; /* метка не влезает */
        }
        out[n++] = (unsigned char)len;
        for (int i = 0; i < len; i++) {
            out[n++] = (unsigned char)p[i];
        }
        p = (*dot == '.') ? dot + 1 : dot;
    }
    if (n + 5 > cap) {
        return -1; /* корень + QTYPE + QCLASS не влезают */
    }
    out[n++] = 0x00; /* корень */
    out[n++] = 0x00;
    out[n++] = 0x01; /* QTYPE = A */
    out[n++] = 0x00;
    out[n++] = 0x01; /* QCLASS = IN */
    return n;
}

/* Перешагнуть DNS-имя с позиции `pos` (учитывая сжатие). Возвращает позицию после или -1. */
static int dns_skip_name(const unsigned char *msg, int len, int pos) {
    while (pos < len) {
        unsigned char c = msg[pos];
        if ((c & 0xc0) == 0xc0) {
            return pos + 2 <= len ? pos + 2 : -1;
        }
        if (c == 0) {
            return pos + 1;
        }
        pos += 1 + c;
    }
    return -1;
}

/* Найти первую A-запись (TYPE=1, CLASS=1, RDLENGTH=4); положить IPv4 в ip[4]. 1 — нашли, 0 — нет. */
static int dns_first_a(const unsigned char *msg, int len, unsigned char ip[4]) {
    if (len < 12) {
        return 0;
    }
    int qd = (msg[4] << 8) | msg[5];
    int an = (msg[6] << 8) | msg[7];
    int pos = 12;
    for (int i = 0; i < qd; i++) {
        pos = dns_skip_name(msg, len, pos);
        if (pos < 0) {
            return 0;
        }
        pos += 4; /* QTYPE + QCLASS */
    }
    for (int i = 0; i < an; i++) {
        pos = dns_skip_name(msg, len, pos);
        if (pos < 0 || pos + 10 > len) {
            return 0;
        }
        int type = (msg[pos] << 8) | msg[pos + 1];
        int cls = (msg[pos + 2] << 8) | msg[pos + 3];
        int rdlen = (msg[pos + 8] << 8) | msg[pos + 9];
        int rdata = pos + 10;
        if (rdata + rdlen > len) {
            return 0;
        }
        if (type == 1 && cls == 1 && rdlen == 4) {
            ip[0] = msg[rdata];
            ip[1] = msg[rdata + 1];
            ip[2] = msg[rdata + 2];
            ip[3] = msg[rdata + 3];
            return 1;
        }
        pos = rdata + rdlen;
    }
    return 0;
}

#endif /* FERROS_DNS_H */
