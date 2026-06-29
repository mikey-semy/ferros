/*
 * Демонстрация минимальной libc ferros (M9h): обычная C-программа со стандартным `int main()`,
 * которая выделяет память через malloc и пользуется ею. Линкуется с crt0.s + libc.c.
 *
 * Проверяет всю цепочку libc: crt0 позвал main с верным argc; malloc через brk дал страницы кучи,
 * пригодные на запись (8 КиБ — больше страницы, значит был и рост разрыва); запись на stdout
 * работает; возврат main стал кодом выхода. Самопроверяется: возвращает 0 только при совпадении
 * контрольной суммы и argc.
 */
#include "libc.h"

int main(int argc, char **argv) {
    (void)argv;

    const size_t n = 8192; /* > страницы → malloc обязан вырастить brk */
    unsigned char *buf = (unsigned char *)malloc(n);
    if (buf == 0) {
        return 1; /* нет памяти */
    }

    for (size_t i = 0; i < n; i++) {
        buf[i] = (unsigned char)(i & 0xff);
    }
    unsigned long sum = 0;
    for (size_t i = 0; i < n; i++) {
        sum += buf[i];
    }
    free(buf);

    /* sum(i & 0xff) по 8192 байтам = 32 полных блока [0..255] = 32 * 32640 = 1044480. */
    const unsigned long expected = 1044480UL;

    const char *msg = "C libc demo: malloc OK\n";
    write(1, msg, strlen(msg));

    /* argc == 0: процесс запущен без аргументов (spawn без execve) — заодно проверяем, что crt0
     * прочитал argc, а не мусор. */
    return (sum == expected && argc == 0) ? 0 : 2;
}
