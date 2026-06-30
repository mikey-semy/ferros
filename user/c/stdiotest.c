/*
 * stdiotest (M9n): прикладной C использует потоковый ввод-вывод stdio (`FILE *`).
 *
 * Доказывает слой `FILE *` над сисколлами: пишем файл через `fopen("w")`/`fputs`/`fprintf`/`fputc`/
 * `fclose`, затем читаем обратно через `fopen("r")`/`fgets`/`fgetc`/`feof`/`fclose` и сверяем
 * содержимое — round-trip через stdio, как у обычной C-программы. Файл кладём в каталог `/SUB`
 * (там есть место; корень FAT не растёт). Успех = всё совпало → выход 0; иначе сообщение на
 * `stderr` (fd 2, виден в serial) и выход 1.
 */
#include "libc.h"

#define PATH "/SUB/STDIO.TXT"

static void fail(const char *msg) {
    fputs(msg, stderr);
    exit(1);
}

int main(void) {
    /* --- Запись через FILE*. --- */
    FILE *w = fopen(PATH, "w");
    if (!w) {
        fail("[stdio] fopen(w) failed\n");
    }
    if (fputs("hello stdio\n", w) < 0) {
        fail("[stdio] fputs failed\n");
    }
    fprintf(w, "answer=%d\n", 42);
    fputc('Z', w);
    fputc('\n', w);
    if (fclose(w) != 0) {
        fail("[stdio] fclose(w) failed\n");
    }

    /* --- Чтение обратно и сверка. --- */
    FILE *r = fopen(PATH, "r");
    if (!r) {
        fail("[stdio] fopen(r) failed\n");
    }
    char line[64];
    if (!fgets(line, sizeof line, r) || strcmp(line, "hello stdio\n") != 0) {
        fail("[stdio] line 1 mismatch\n");
    }
    if (!fgets(line, sizeof line, r) || strcmp(line, "answer=42\n") != 0) {
        fail("[stdio] line 2 mismatch\n");
    }
    /* Третью строку ("Z\n") читаем посимвольно через fgetc, затем ждём EOF. */
    if (fgetc(r) != 'Z' || fgetc(r) != '\n') {
        fail("[stdio] line 3 (fgetc) mismatch\n");
    }
    if (fgetc(r) != EOF || !feof(r)) {
        fail("[stdio] expected EOF after line 3\n");
    }
    fclose(r);

    fprintf(stderr, "[stdio] round-trip ok via FILE* (%s)\n", PATH);
    return 0;
}
