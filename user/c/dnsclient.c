/*
 * dnsclient (M8d3): прикладной C резолвит имя по DNS через СОКЕТ-сисколлы ferros (UDP).
 *
 * Это смыкает сеть с north-star целью: обычная C-программа в кольце 3 делает
 * socket()/sendto()/recvfrom() (POSIX-сокеты) — ровно как сетевой Linux-софт. Собираем DNS-запрос
 * A-записи для `dns.google`, шлём его UDP на резолвер SLIRP 10.0.2.3:53, разбираем ответ и
 * печатаем адрес. Успех (выход 0) — только если вернулся один из стабильных anycast-адресов Google
 * (8.8.8.8 / 8.8.4.4), иначе выход 1. Строку с результатом пишем на fd 2 (serial) — видно в логе.
 */
#include "dns.h"
#include "libc.h"

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
    int qn = dns_build(0x1234, "dns.google", q, sizeof q);
    if (qn < 0) {
        fail("[dnsclient] query build failed\n");
    }
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
