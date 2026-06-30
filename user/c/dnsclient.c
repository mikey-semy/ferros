/*
 * dnsclient (M8d3): прикладной C резолвит имя по DNS через СОКЕТ-сисколлы ferros.
 *
 * Это смыкает сеть с north-star целью: обычная C-программа в кольце 3 делает
 * socket()/sendto()/recvfrom() (POSIX-сокеты) — ровно как сетевой Linux-софт. Собираем DNS-запрос
 * A-записи для `dns.google`, шлём его UDP на резолвер SLIRP 10.0.2.3:53, разбираем ответ и
 * печатаем адрес. Успех (выход 0) — только если вернулся один из стабильных anycast-адресов Google
 * (8.8.8.8 / 8.8.4.4), иначе выход 1. Строку с результатом пишем на fd 2 (serial) — видно в логе.
 */
#include "libc.h"

/* Собрать DNS-запрос A-записи для `name` с идентификатором `id` в `out`. Возвращает длину. */
static int dns_build(unsigned short id, const char *name, unsigned char *out) {
    int n = 0;
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
    /* QNAME: метки «длина + байты», конец — нулевой байт. */
    const char *p = name;
    while (*p) {
        const char *dot = p;
        while (*dot && *dot != '.') {
            dot++;
        }
        int len = (int)(dot - p);
        out[n++] = (unsigned char)len;
        for (int i = 0; i < len; i++) {
            out[n++] = (unsigned char)p[i];
        }
        p = (*dot == '.') ? dot + 1 : dot;
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

/* Найти первую A-запись (TYPE=1, CLASS=1, RDLENGTH=4) и положить IPv4 в ip[4]. 1 — нашли, 0 — нет. */
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

/* Сообщение об ошибке на fd 2 (serial) + код выхода. */
static void fail(const char *msg) {
    write(2, msg, strlen(msg));
    exit(1);
}

int main(void) {
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) {
        fail("[dnsclient] socket failed\n");
    }

    /* Адресат: DNS-сервер SLIRP 10.0.2.3:53. */
    struct sockaddr_in dst;
    memset(&dst, 0, sizeof dst);
    dst.sin_family = AF_INET;
    dst.sin_port = htons(53);
    unsigned char *a = (unsigned char *)&dst.sin_addr;
    a[0] = 10;
    a[1] = 0;
    a[2] = 2;
    a[3] = 3;

    unsigned char q[256];
    int qn = dns_build(0x1234, "dns.google", q);
    if (sendto(fd, q, (size_t)qn, 0, &dst, sizeof dst) < 0) {
        fail("[dnsclient] sendto failed\n");
    }

    unsigned char r[512];
    long rn = recvfrom(fd, r, sizeof r, 0, 0, 0);
    if (rn < 0) {
        fail("[dnsclient] recvfrom failed\n");
    }

    unsigned char ip[4];
    if (!dns_first_a(r, (int)rn, ip)) {
        fail("[dnsclient] no A record\n");
    }

    char line[64];
    int m = snprintf(line, sizeof line, "[dnsclient] dns.google -> %d.%d.%d.%d\n", ip[0], ip[1],
                     ip[2], ip[3]);
    write(2, line, (size_t)m);
    close(fd);

    /* dns.google — стабильные anycast-адреса Google: успех только при них (сильная проверка). */
    int ok = (ip[0] == 8 && ip[1] == 8 && ip[2] == 8 && ip[3] == 8) ||
             (ip[0] == 8 && ip[1] == 8 && ip[2] == 4 && ip[3] == 4);
    return ok ? 0 : 1;
}
