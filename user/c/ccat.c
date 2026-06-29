/*
 * `ccat` (M9m): утилита `cat` на C поверх минимальной libc — настоящий shell-coreutil. Лежит на
 * диске в `/bin`, shell находит и запускает её через fork/execve с аргументами. Берёт имя файла из
 * `argv[1]`, выводит его содержимое на stdout (циклом до EOF — работает с файлами больше буфера).
 *
 * Доказывает, что прикладной C-код работает как обычная утилита окружения (argv + файловый I/O +
 * перенаправление shell'а), а не только как встроенная тест-программа.
 */
#include "libc.h"

int main(int argc, char **argv) {
    if (argc < 2) {
        write(2, "usage: ccat FILE\n", 17);
        return 1;
    }

    int fd = open(argv[1], O_RDONLY);
    if (fd < 0) {
        write(2, "ccat: cannot open file\n", 23);
        return 1;
    }

    char buf[256];
    ssize_t n;
    while ((n = read(fd, buf, sizeof buf)) > 0) {
        write(1, buf, (size_t)n);
    }
    close(fd);

    return (n < 0) ? 1 : 0; /* n == 0 — нормальный EOF */
}
