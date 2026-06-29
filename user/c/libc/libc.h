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

/* Куча поверх brk: bump-аллокатор (free пока без переиспользования — см. HARDENING). */
void *malloc(size_t n);
void free(void *p);

/* Базовые операции с памятью и строками. */
void *memset(void *dst, int c, size_t n);
void *memcpy(void *dst, const void *src, size_t n);
size_t strlen(const char *s);

/* Минимальный вывод символов/строк в stdout. */
int putchar(int c);
int puts(const char *s);

#endif /* FERROS_LIBC_H */
