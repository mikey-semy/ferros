/*
 * Проверка форматированного вывода минимальной libc (M9j): `snprintf`/`printf` со всеми
 * поддержанными спецификаторами. Линкуется с crt0 + libc.
 *
 * Форматирует строку через `snprintf`, пишет её на stdout одним `write`, и сам сверяет результат с
 * эталоном через `strcmp` — возвращает 0 только при точном совпадении. Тест проверяет код выхода 0
 * (значит формат верный) и записанную строку (по наблюдаемости записи).
 */
#include "libc.h"

int main(void) {
    char buf[128];
    int n = snprintf(buf, sizeof buf, "d=%d u=%u x=%x X=%X s=%s c=%c ld=%ld %%\n", -42, 42u, 0xdeadu,
                     0xBEEFu, "ok", 'Q', -9000000000L);
    write(1, buf, (size_t)n);

    const char *expected = "d=-42 u=42 x=dead X=BEEF s=ok c=Q ld=-9000000000 %\n";
    return (strcmp(buf, expected) == 0) ? 0 : 1;
}
