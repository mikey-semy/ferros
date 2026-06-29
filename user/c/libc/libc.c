/*
 * Минимальная libc ferros (M9h): реализация функций из libc.h поверх Linux-сисколлов ferros.
 * Линкуется с crt0.s в C-программы со стандартным `int main()`. Это фундамент: дальше libc
 * наращивается инкрементально (printf, больше string/stdio, …) либо заменяется портом relibc.
 */
#include "libc.h"

/* --- Тонкие обёртки над инструкцией `syscall` (Linux x86-64: nr в rax; rdi/rsi/rdx; rcx/r11 — scratch). --- */

static long sc1(long nr, long a) {
    long ret;
    __asm__ volatile("syscall" : "=a"(ret) : "a"(nr), "D"(a) : "rcx", "r11", "memory");
    return ret;
}

static long sc3(long nr, long a, long b, long c) {
    long ret;
    __asm__ volatile("syscall"
                     : "=a"(ret)
                     : "a"(nr), "D"(a), "S"(b), "d"(c)
                     : "rcx", "r11", "memory");
    return ret;
}

/* --- Процесс и ввод-вывод --- */

__attribute__((noreturn)) void exit(int code) {
    sc1(60, code); /* SYS_exit */
    __builtin_unreachable();
}

ssize_t write(int fd, const void *buf, size_t n) {
    return sc3(1, fd, (long)buf, (long)n); /* SYS_write */
}

ssize_t read(int fd, void *buf, size_t n) {
    return sc3(0, fd, (long)buf, (long)n); /* SYS_read */
}

/* --- Куча: bump-аллокатор поверх brk (сисколл 12). --- */

static char *heap_cur; /* текущий конец занятой кучи (следующее malloc отдаёт отсюда) */
static char *heap_lim; /* до какого адреса куча уже отображена (brk) */

void *malloc(size_t n) {
    n = (n + 15) & ~(size_t)15; /* выравниваем по 16 байт */
    if (heap_cur == 0) {        /* первая аллокация: текущий разрыв = база кучи */
        char *base = (char *)sc1(12, 0);
        heap_cur = base;
        heap_lim = base;
    }
    if ((size_t)(heap_lim - heap_cur) < n) {
        /* не хватает отображённого — растим разрыв до heap_cur + n. brk возвращает новый разрыв. */
        char *want = heap_cur + n;
        char *got = (char *)sc1(12, (long)want);
        if (got < want) {
            return 0; /* нет памяти */
        }
        heap_lim = got;
    }
    char *p = heap_cur;
    heap_cur += n;
    return p;
}

void free(void *p) {
    (void)p; /* bump-аллокатор не переиспользует освобождённое (минимально; см. HARDENING) */
}

/* --- Память и строки --- */

void *memset(void *dst, int c, size_t n) {
    unsigned char *p = (unsigned char *)dst;
    while (n--) {
        *p++ = (unsigned char)c;
    }
    return dst;
}

void *memcpy(void *dst, const void *src, size_t n) {
    unsigned char *d = (unsigned char *)dst;
    const unsigned char *s = (const unsigned char *)src;
    while (n--) {
        *d++ = *s++;
    }
    return dst;
}

size_t strlen(const char *s) {
    size_t n = 0;
    while (s[n]) {
        n++;
    }
    return n;
}

int putchar(int c) {
    char ch = (char)c;
    return (int)write(1, &ch, 1);
}

int puts(const char *s) {
    write(1, s, strlen(s));
    return (int)write(1, "\n", 1);
}
