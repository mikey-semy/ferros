/*
 * httpget (M8f): прикладной C делает HTTP-запрос из кольца 3 — настоящий сетевой клиент.
 *
 * Как обычный Linux-софт: открываем TCP к веб-серверу на порт 80, шлём `GET / HTTP/1.0` и читаем
 * ответ до закрытия соединения (`Connection: close` — сервер закроет сам, наш recv упрётся в EOF).
 * Адресат — публичный anycast Cloudflare **1.1.1.1:80** (стабилен и достижим тем же исходящим TCP,
 * что проверил M8e к 8.8.8.8:53; на :80 он отдаёт HTTP-редирект). Успех (выход 0) — ответ начинается
 * с `HTTP/1.` (значит TCP-соединение и обмен HTTP отработали из кольца 3). Статусную строку пишем на
 * fd 2 (serial) — видно в логе. Нужен исходящий TCP хоста (как и прочие сетевые демо).
 *
 * Резолв имени по DNS из кольца 3 уже показан отдельно (`dnsclient`/`tcpdns`); тут адрес фиксирован,
 * чтобы тест не зависел от CDN-маршрутизации произвольного хоста.
 */
#include "libc.h"

/* Сообщение об ошибке на fd 2 (serial) + код выхода. */
static void fail(const char *msg) {
    write(2, msg, strlen(msg));
    exit(1);
}

int main(void) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        fail("[httpget] socket failed\n");
    }

    /* Cloudflare 1.1.1.1:80 — стабильный anycast веб-сервер. */
    struct sockaddr_in srv;
    memset(&srv, 0, sizeof srv);
    srv.sin_family = AF_INET;
    srv.sin_port = htons(80);
    unsigned char *s = (unsigned char *)&srv.sin_addr;
    s[0] = 1;
    s[1] = 1;
    s[2] = 1;
    s[3] = 1;
    if (connect(fd, &srv, sizeof srv) < 0) {
        fail("[httpget] connect failed\n");
    }

    /* HTTP/1.0 с `Connection: close`, чтобы сервер закрыл соединение после ответа (наш recv → EOF). */
    const char *req = "GET / HTTP/1.0\r\nHost: 1.1.1.1\r\nConnection: close\r\n\r\n";
    int total = (int)strlen(req), sent = 0;
    while (sent < total) {
        long w = send(fd, req + sent, (size_t)(total - sent), 0);
        if (w <= 0) {
            fail("[httpget] send failed\n");
        }
        sent += (int)w;
    }

    /* Читаем ответ до EOF (или заполнения буфера) — нам важно его начало (статусная строка). */
    char resp[2048];
    int have = 0;
    while (have < (int)sizeof resp - 1) {
        long got = recv(fd, resp + have, sizeof resp - 1 - (size_t)have, 0);
        if (got < 0) {
            fail("[httpget] recv failed\n");
        }
        if (got == 0) {
            break; /* сервер закрыл соединение */
        }
        have += (int)got;
    }
    resp[have] = '\0';
    close(fd);

    /* Логируем статусную строку (до первого CR/LF). */
    int eol = 0;
    while (eol < have && resp[eol] != '\r' && resp[eol] != '\n') {
        eol++;
    }
    write(2, "[httpget] ", 10);
    write(2, resp, (size_t)eol);
    write(2, "\n", 1);

    /* Доказательство: пришёл валидный HTTP-ответ (TCP-соединение и обмен из кольца 3 отработали). */
    if (have < 8 || strncmp(resp, "HTTP/1.", 7) != 0) {
        fail("[httpget] not an HTTP response\n");
    }
    return 0;
}
