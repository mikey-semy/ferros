/*
 * wc (M9r): настоящая утилита подсчёта строк/слов/байт на C поверх libc ferros.
 *
 * Это первая «серьёзная» прикладная программа, упражняющая libc разом: флаги через getopt (M9p),
 * чтение файла через FILE* (M9n), форматный вывод через printf. Лежит на диске в /bin, shell сам её
 * находит и запускает с аргументами — как обычный coreutil.
 *
 * Usage: wc [-l] [-w] [-c] [file]
 *   -l строки, -w слова, -c байты; без флагов — все три (как настоящий wc). Без файла — stdin.
 * Печатает выбранные счётчики (через пробел) и имя файла.
 */
#include "libc.h"

int main(int argc, char **argv) {
    int want_l = 0, want_w = 0, want_c = 0;

    optind = 1;
    int o;
    while ((o = getopt(argc, argv, "lwc")) != -1) {
        switch (o) {
        case 'l':
            want_l = 1;
            break;
        case 'w':
            want_w = 1;
            break;
        case 'c':
            want_c = 1;
            break;
        default: /* '?' — неизвестная опция (getopt уже напечатал ошибку) */
            fputs("usage: wc [-lwc] [file]\n", stderr);
            return 1;
        }
    }
    /* Без флагов — считаем всё, как настоящий wc. */
    if (!want_l && !want_w && !want_c) {
        want_l = want_w = want_c = 1;
    }

    const char *path = (optind < argc) ? argv[optind] : 0;
    FILE *f = path ? fopen(path, "r") : stdin;
    if (!f) {
        fprintf(stderr, "wc: cannot open %s\n", path);
        return 1;
    }

    long lines = 0, words = 0, bytes = 0;
    int in_word = 0;
    int c;
    while ((c = fgetc(f)) != EOF) {
        bytes++;
        if (c == '\n') {
            lines++;
        }
        if (c == ' ' || c == '\t' || c == '\n') {
            in_word = 0;
        } else if (!in_word) {
            in_word = 1;
            words++;
        }
    }
    if (path) {
        fclose(f);
    }

    /* Печатаем только запрошенные счётчики через одиночный пробел, затем имя файла. */
    int n = 0;
    if (want_l) {
        if (n++) {
            printf(" ");
        }
        printf("%ld", lines);
    }
    if (want_w) {
        if (n++) {
            printf(" ");
        }
        printf("%ld", words);
    }
    if (want_c) {
        if (n++) {
            printf(" ");
        }
        printf("%ld", bytes);
    }
    if (path) {
        printf(" %s", path);
    }
    printf("\n");
    return 0;
}
