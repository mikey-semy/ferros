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

void *memmove(void *dst, const void *src, size_t n) {
    unsigned char *d = (unsigned char *)dst;
    const unsigned char *s = (const unsigned char *)src;
    if (d < s) {
        while (n--) { /* копируем вперёд: dst раньше src — переписывать слева направо безопасно */
            *d++ = *s++;
        }
    } else if (d > s) {
        d += n; /* копируем назад: dst позже src — иначе затёрли бы ещё не прочитанное */
        s += n;
        while (n--) {
            *--d = *--s;
        }
    }
    return dst;
}

int memcmp(const void *a, const void *b, size_t n) {
    const unsigned char *p = (const unsigned char *)a;
    const unsigned char *q = (const unsigned char *)b;
    while (n--) {
        if (*p != *q) {
            return (int)*p - (int)*q;
        }
        p++;
        q++;
    }
    return 0;
}

int strcmp(const char *a, const char *b) {
    while (*a && *a == *b) {
        a++;
        b++;
    }
    return (int)(unsigned char)*a - (int)(unsigned char)*b;
}

int strncmp(const char *a, const char *b, size_t n) {
    while (n > 0 && *a != '\0' && *a == *b) {
        a++;
        b++;
        n--;
    }
    if (n == 0) {
        return 0; /* сравнили первые n символов — равны */
    }
    return (int)(unsigned char)*a - (int)(unsigned char)*b;
}

char *strcpy(char *dst, const char *src) {
    char *r = dst;
    while ((*dst++ = *src++) != '\0') {
        /* копируем включая завершающий нуль */
    }
    return r;
}

char *strncpy(char *dst, const char *src, size_t n) {
    size_t i = 0;
    for (; i < n && src[i] != '\0'; i++) {
        dst[i] = src[i];
    }
    for (; i < n; i++) {
        dst[i] = '\0'; /* добиваем нулями (как в C; терминатора может и не быть, если src длиннее n) */
    }
    return dst;
}

char *strchr(const char *s, int c) {
    char ch = (char)c;
    while (*s != '\0') {
        if (*s == ch) {
            return (char *)s;
        }
        s++;
    }
    return (ch == '\0') ? (char *)s : 0; /* strchr находит и завершающий '\0' */
}

int atoi(const char *s) {
    while (*s == ' ' || *s == '\t' || *s == '\n') {
        s++; /* пропускаем ведущие пробелы */
    }
    int sign = 1;
    if (*s == '-') {
        sign = -1;
        s++;
    } else if (*s == '+') {
        s++;
    }
    int v = 0;
    while (*s >= '0' && *s <= '9') {
        v = v * 10 + (*s - '0'); /* переполнение — UB, как у стандартного atoi */
        s++;
    }
    return sign * v;
}

int putchar(int c) {
    char ch = (char)c;
    return (int)write(1, &ch, 1);
}

int puts(const char *s) {
    write(1, s, strlen(s));
    return (int)write(1, "\n", 1);
}

/* --- Форматированный вывод (M9j): минимальный printf-движок --- */

#include <stdarg.h>

/* Кладёт символ в буфер вывода с учётом ёмкости `cap` (оставляя место под завершающий нуль) и
 * считает ПОЛНУЮ длину в `*pos` (даже если не влезло — как C-printf возвращает «сколько было бы»). */
static void put_ch(char *out, size_t cap, size_t *pos, char c) {
    if (*pos + 1 < cap) {
        out[*pos] = c;
    }
    (*pos)++;
}

static void put_str(char *out, size_t cap, size_t *pos, const char *s) {
    while (*s) {
        put_ch(out, cap, pos, *s++);
    }
}

/* Беззнаковое `v` в системе счисления `base` (10 или 16); `upper` — заглавный hex. */
static void put_uint(char *out, size_t cap, size_t *pos, unsigned long v, unsigned base, int upper) {
    char tmp[20]; /* до 20 цифр десятичного u64 */
    const char *digits = upper ? "0123456789ABCDEF" : "0123456789abcdef";
    int i = 0;
    if (v == 0) {
        tmp[i++] = '0';
    }
    while (v != 0) {
        tmp[i++] = digits[v % base];
        v /= base;
    }
    while (i > 0) {
        put_ch(out, cap, pos, tmp[--i]); /* цифры были в обратном порядке */
    }
}

int vsnprintf(char *out, size_t cap, const char *fmt, va_list ap) {
    size_t pos = 0;
    for (const char *p = fmt; *p != '\0'; p++) {
        if (*p != '%') {
            put_ch(out, cap, &pos, *p);
            continue;
        }
        p++;
        int is_long = 0;
        if (*p == 'l') { /* модификатор длины (одиночный l) */
            is_long = 1;
            p++;
        }
        switch (*p) {
        case 'd':
        case 'i': {
            long v = is_long ? va_arg(ap, long) : (long)va_arg(ap, int);
            unsigned long mag;
            if (v < 0) {
                put_ch(out, cap, &pos, '-');
                mag = 0UL - (unsigned long)v; /* корректно и для LONG_MIN */
            } else {
                mag = (unsigned long)v;
            }
            put_uint(out, cap, &pos, mag, 10, 0);
            break;
        }
        case 'u': {
            unsigned long v = is_long ? va_arg(ap, unsigned long) : (unsigned long)va_arg(ap, unsigned int);
            put_uint(out, cap, &pos, v, 10, 0);
            break;
        }
        case 'x':
        case 'X': {
            unsigned long v = is_long ? va_arg(ap, unsigned long) : (unsigned long)va_arg(ap, unsigned int);
            put_uint(out, cap, &pos, v, 16, *p == 'X');
            break;
        }
        case 'p': {
            unsigned long v = (unsigned long)va_arg(ap, void *);
            put_str(out, cap, &pos, "0x");
            put_uint(out, cap, &pos, v, 16, 0);
            break;
        }
        case 's': {
            const char *s = va_arg(ap, const char *);
            put_str(out, cap, &pos, s != 0 ? s : "(null)");
            break;
        }
        case 'c':
            put_ch(out, cap, &pos, (char)va_arg(ap, int));
            break;
        case '%':
            put_ch(out, cap, &pos, '%');
            break;
        case '\0':
            p--; /* висячий '%' в конце — выйдем по условию цикла */
            break;
        default: /* неизвестный спецификатор — печатаем как есть */
            put_ch(out, cap, &pos, '%');
            put_ch(out, cap, &pos, *p);
            break;
        }
    }
    if (cap > 0) {
        out[pos < cap ? pos : cap - 1] = '\0';
    }
    return (int)pos;
}

int snprintf(char *out, size_t cap, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(out, cap, fmt, ap);
    va_end(ap);
    return n;
}

int printf(const char *fmt, ...) {
    char buf[256];
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(buf, sizeof buf, fmt, ap);
    va_end(ap);
    /* Пишем ОДНИМ write (так тест ловит весь вывод по LAST_WRITE; и это естественная буферизация).
     * Если строка длиннее буфера — пишем сколько влезло (обрезано). */
    int w = (n < (int)sizeof buf) ? n : (int)(sizeof buf) - 1;
    write(1, buf, (size_t)w);
    return n;
}
