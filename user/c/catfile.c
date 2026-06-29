/*
 * Прикладной C на ferros (M9l): программа читает реальный файл с диска через libc и выводит его —
 * по сути ядро `cat`. Доказывает, что обычный C-код делает файловый ввод-вывод (`open`/`read`/
 * `write`/`close` поверх наших сисколлов), а не только считает в памяти. Линкуется с crt0 + libc.
 *
 * Открывает `/HELLO.TXT` (его кладёт build.rs), печатает содержимое на stdout и сам сверяет его с
 * известной фикстурой — возвращает 0 только при совпадении.
 */
#include "libc.h"

int main(void) {
    int fd = open("/HELLO.TXT", O_RDONLY);
    if (fd < 0) {
        return 1; /* не открылось */
    }

    char buf[128];
    ssize_t n = read(fd, buf, sizeof buf);
    close(fd);
    if (n <= 0) {
        return 2; /* пусто/ошибка чтения */
    }

    /* «cat»: выводим прочитанное на stdout. */
    write(1, buf, (size_t)n);

    /* Самопроверка: содержимое известно (та же строка, что в build.rs FAT_TEST_CONTENT). */
    const char *expected = "ferros M6c: hello from FAT32!\n";
    if (n != 30 || memcmp(buf, expected, 30) != 0) {
        return 3; /* прочитали не то */
    }
    return 0;
}
