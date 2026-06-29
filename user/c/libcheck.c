/*
 * Проверка строковых/мемори/конвертирующих функций минимальной libc (M9k): `memmove`/`memcmp`/
 * `strncmp`/`strcpy`/`strncpy`/`strchr`/`atoi`. Линкуется с crt0 + libc.
 *
 * Самопроверяется: каждая группа возвращает свой ненулевой код при ошибке, 0 — если всё сошлось.
 */
#include "libc.h"

int main(void) {
    char buf[32];

    /* memmove с перекрытием dst > src (копирование назад). */
    {
        char m[] = "ABCDEF";
        memmove(m + 1, m, 5); /* → "AABCDE" */
        if (strcmp(m, "AABCDE") != 0) {
            return 10;
        }
    }
    /* memmove с перекрытием dst < src (копирование вперёд). */
    {
        char m[] = "ABCDEF";
        memmove(m, m + 1, 5); /* → "BCDEFF" */
        if (strcmp(m, "BCDEFF") != 0) {
            return 11;
        }
    }

    /* memcmp: равные и различающиеся. */
    if (memcmp("abc", "abc", 3) != 0 || memcmp("abc", "abd", 3) >= 0) {
        return 12;
    }

    /* strncmp: первые n совпадают / расходятся. */
    if (strncmp("hello", "help", 3) != 0 || strncmp("hello", "help", 4) >= 0) {
        return 13;
    }

    /* strcpy. */
    strcpy(buf, "copy me");
    if (strcmp(buf, "copy me") != 0) {
        return 14;
    }

    /* strncpy: добивает нулями, если src короче n. */
    char nb[6];
    strncpy(nb, "ab", 6);
    if (nb[0] != 'a' || nb[1] != 'b' || nb[2] != '\0' || nb[5] != '\0') {
        return 15;
    }

    /* strchr: найдено / не найдено / завершающий '\0'. */
    {
        char hello[] = "hello";
        char *s = hello;
        if (strchr(s, 'e') != s + 1 || strchr(s, 'z') != 0 || strchr(s, '\0') != s + 5) {
            return 16;
        }
    }

    /* atoi: знак, ведущие пробелы, мусор-хвост, не-число. */
    if (atoi("-123") != -123 || atoi("  42") != 42 || atoi("+7") != 7 || atoi("abc") != 0 ||
        atoi("12x") != 12) {
        return 17;
    }

    puts("libc strings ok");
    return 0;
}
