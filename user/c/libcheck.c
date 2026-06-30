/*
 * Проверка строковых/мемори/конвертирующих функций минимальной libc (M9k, расширено M9o/M9p):
 * `memmove`/`memcmp`/`strncmp`/`strcpy`/`strncpy`/`strchr`/`atoi`,
 * `strcat`/`strncat`/`strrchr`/`strstr`/`strtok`/`strtol`/`strtoul` (M9o), `getopt` (M9p).
 * Линкуется с crt0 + libc.
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

    /* strcat / strncat (M9o). */
    {
        char c[16];
        strcpy(c, "foo");
        strcat(c, "bar");
        if (strcmp(c, "foobar") != 0) {
            return 18;
        }
        strcpy(c, "foo");
        strncat(c, "barbaz", 3); /* допишет только "bar" + нуль */
        if (strcmp(c, "foobar") != 0) {
            return 19;
        }
    }

    /* strrchr: последнее вхождение, не найдено, завершающий '\0'. */
    {
        char path[] = "a/b/c";
        if (strrchr(path, '/') != path + 3 || strrchr(path, 'z') != 0 ||
            strrchr(path, '\0') != path + 5) {
            return 20;
        }
    }

    /* strstr: найдено, не найдено, пустая игла → начало стога. */
    {
        char hay[] = "hello world";
        if (strstr(hay, "wor") != hay + 6 || strstr(hay, "xyz") != 0 || strstr(hay, "") != hay) {
            return 21;
        }
    }

    /* strtol: знак; авто-база 0x/0; заданная база; endptr; не-число. */
    {
        char *end;
        char bad[] = "z9";
        if (strtol("-42", 0, 10) != -42) {
            return 22;
        }
        if (strtol("0x1F", &end, 0) != 31 || *end != '\0') {
            return 22;
        }
        if (strtol("  ff", &end, 16) != 255) {
            return 22;
        }
        if (strtol("0755", 0, 0) != 0755) { /* ведущий 0 → восьмеричное */
            return 22;
        }
        if (strtol(bad, &end, 10) != 0 || end != bad) { /* нет цифр → 0, endptr == nptr */
            return 22;
        }
    }

    /* strtoul: большое беззнаковое; hex. */
    if (strtoul("4294967295", 0, 10) != 4294967295UL || strtoul("0xff", 0, 16) != 255) {
        return 23;
    }

    /* strtok: разбиение, пропуск пустых полей (",,"). */
    {
        char s[] = "a,b,,c";
        char *t = strtok(s, ",");
        if (!t || strcmp(t, "a") != 0) {
            return 24;
        }
        if (!(t = strtok(0, ",")) || strcmp(t, "b") != 0) {
            return 24;
        }
        if (!(t = strtok(0, ",")) || strcmp(t, "c") != 0) {
            return 24;
        }
        if (strtok(0, ",") != 0) {
            return 24;
        }
    }

    /* getopt (M9p): кластеризация, слитный (-bval) и раздельный (-c carg) аргумент, стоп на не-опции. */
    {
        char *av[] = {"prog", "-a", "-bval", "-c", "carg", "rest", 0};
        int ac = 6;
        optind = 1;
        opterr = 0; /* без печати ошибок в stderr */
        int seen_a = 0, seen_b = 0, seen_c = 0;
        char *bval = 0, *cval = 0;
        int o;
        while ((o = getopt(ac, av, "ab:c:")) != -1) {
            switch (o) {
            case 'a':
                seen_a = 1;
                break;
            case 'b':
                seen_b = 1;
                bval = optarg;
                break;
            case 'c':
                seen_c = 1;
                cval = optarg;
                break;
            default:
                return 25;
            }
        }
        if (!seen_a || !seen_b || !seen_c) {
            return 25;
        }
        if (!bval || strcmp(bval, "val") != 0 || !cval || strcmp(cval, "carg") != 0) {
            return 25;
        }
        if (optind != 5 || strcmp(av[optind], "rest") != 0) {
            return 25;
        }
    }

    puts("libc strings ok");
    return 0;
}
