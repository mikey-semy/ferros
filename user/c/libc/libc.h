/*
 * Заголовок минимальной libc ferros (M9h). Объявляет горстку функций, которых хватает реальной
 * C-программе: ввод-вывод поверх сисколлов, куча (malloc/free через brk) и базовые mem/str.
 * Это НЕ полная libc — ровно столько, чтобы `int main()` + malloc + строки работали (см. HARDENING).
 */
#ifndef FERROS_LIBC_H
#define FERROS_LIBC_H

typedef unsigned long size_t;
typedef long ssize_t;

/* Завершить процесс с кодом (сисколл exit). Не возвращается. */
void exit(int code) __attribute__((noreturn));

/* Ввод-вывод по дескриптору (сисколлы write/read). Возвращают число байт или -errno. */
ssize_t write(int fd, const void *buf, size_t n);
ssize_t read(int fd, void *buf, size_t n);

/* Открыть/закрыть файл (сисколлы open/close). `open` возвращает дескриптор (≥0) или -errno. */
int open(const char *path, int flags);
int close(int fd);

/* Флаги доступа open(2) (значения как в Linux). */
#define O_RDONLY 0
#define O_WRONLY 1
#define O_RDWR 2

/* Куча поверх brk: bump-аллокатор (free пока без переиспользования — см. HARDENING). */
void *malloc(size_t n);
void free(void *p);

/* Базовые операции с памятью и строками. */
void *memset(void *dst, int c, size_t n);
void *memcpy(void *dst, const void *src, size_t n);
void *memmove(void *dst, const void *src, size_t n); /* безопасна при перекрытии */
int memcmp(const void *a, const void *b, size_t n);
size_t strlen(const char *s);
int strcmp(const char *a, const char *b);
int strncmp(const char *a, const char *b, size_t n);
char *strcpy(char *dst, const char *src);
char *strncpy(char *dst, const char *src, size_t n);
char *strchr(const char *s, int c); /* находит и завершающий '\0' */
int atoi(const char *s);

/* Минимальный вывод символов/строк в stdout. */
int putchar(int c);
int puts(const char *s);

/* Форматированный вывод (M9j). Подмножество спецификаторов: `%d %i %u %x %X %p %s %c %%` плюс
 * модификатор длины `l` (`%ld` и т.п.). Без ширины/точности/флагов и без плавающей точки (это
 * «действительно сложное» — наращиваем по мере надобности; см. HARDENING). `printf` форматирует во
 * внутренний буфер и пишет его ОДНИМ `write` на stdout; `snprintf` — в буфер пользователя с обрезкой
 * по `cap` (всегда дописывает нуль, если `cap > 0`). Возвращают число символов БЕЗ нуля (как в C —
 * сколько было бы записано без обрезки). */
int snprintf(char *out, size_t cap, const char *fmt, ...) __attribute__((format(printf, 3, 4)));
int printf(const char *fmt, ...) __attribute__((format(printf, 1, 2)));

#endif /* FERROS_LIBC_H */
