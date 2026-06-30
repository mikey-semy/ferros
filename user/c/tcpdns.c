/*
 * tcpdns (M8e): прикладной C резолвит имя по DNS поверх TCP через СОКЕТ-сисколлы ferros.
 *
 * Доказывает потоковые сокеты в кольце 3: socket(SOCK_STREAM) -> connect -> send -> recv — ровно
 * как сетевой Linux-софт (TCP-клиент). Соединяемся с публичным DNS Google 8.8.8.8:53 по TCP,
 * шлём DNS-запрос A-записи для `dns.google` (с 2-байтовым префиксом длины — кадрирование DNS-over-TCP,
 * RFC 1035 §4.2.2), читаем ответ (TCP может прийти кусками — накапливаем) и разбираем адрес. Успех
 * (выход 0) — только при стабильных anycast-адресах Google (8.8.8.8 / 8.8.4.4). Нужен исходящий TCP
 * хоста (как и DNS-зависимость UDP-демо).
 */
#include "dns.h"
#include "libc.h"

/* Сообщение об ошибке на fd 2 (serial) + код выхода. */
static void fail(const char *msg) {
    write(2, msg, strlen(msg));
    exit(1);
}

int main(void) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        fail("[tcpdns] socket failed\n");
    }

    /* Публичный DNS Google по TCP: 8.8.8.8:53. */
    struct sockaddr_in dst;
    memset(&dst, 0, sizeof dst);
    dst.sin_family = AF_INET;
    dst.sin_port = htons(53);
    unsigned char *a = (unsigned char *)&dst.sin_addr;
    a[0] = 8;
    a[1] = 8;
    a[2] = 8;
    a[3] = 8;

    if (connect(fd, &dst, sizeof dst) < 0) {
        fail("[tcpdns] connect failed\n");
    }

    /* DNS-over-TCP: перед сообщением — 2 байта его длины. */
    unsigned char q[256];
    int qn = dns_build(0x1234, "dns.google", q, sizeof q);
    if (qn < 0) {
        fail("[tcpdns] query build failed\n");
    }
    unsigned char framed[258];
    framed[0] = (unsigned char)(qn >> 8);
    framed[1] = (unsigned char)qn;
    memcpy(framed + 2, q, (size_t)qn);

    int total = qn + 2, sent = 0;
    while (sent < total) {
        long w = send(fd, framed + sent, (size_t)(total - sent), 0);
        if (w <= 0) {
            fail("[tcpdns] send failed\n");
        }
        sent += (int)w;
    }

    /* Ответ: сначала 2 байта длины, затем столько байт. TCP — поток, читаем кусками до полноты. */
    unsigned char r[1024];
    int have = 0;
    int want = -1; /* полная длина (2 + длина сообщения), пока неизвестна */
    while (1) {
        if (have >= 2 && want < 0) {
            want = 2 + ((r[0] << 8) | r[1]);
            if (want > (int)sizeof r) {
                fail("[tcpdns] response too large\n");
            }
        }
        if (want >= 0 && have >= want) {
            break;
        }
        long got = recv(fd, r + have, sizeof r - (size_t)have, 0);
        if (got < 0) {
            fail("[tcpdns] recv failed\n");
        }
        if (got == 0) {
            break; /* пир закрыл соединение */
        }
        have += (int)got;
    }
    if (want < 0 || have < want) {
        fail("[tcpdns] short response\n");
    }

    unsigned char ip[4];
    if (!dns_first_a(r + 2, want - 2, ip)) { /* пропускаем 2-байтовый префикс длины */
        fail("[tcpdns] no A record\n");
    }

    char line[64];
    int m = snprintf(line, sizeof line, "[tcpdns] dns.google -> %d.%d.%d.%d\n", ip[0], ip[1], ip[2],
                     ip[3]);
    write(2, line, (size_t)m);
    close(fd);

    int ok = (ip[0] == 8 && ip[1] == 8 && ip[2] == 8 && ip[3] == 8) ||
             (ip[0] == 8 && ip[1] == 8 && ip[2] == 4 && ip[3] == 4);
    return ok ? 0 : 1;
}
