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
#define O_CREAT 0100   /* создать файл, если его нет (восьмеричное, как в Linux) */
#define O_TRUNC 01000  /* обрезать до нуля при открытии */
#define O_APPEND 02000 /* писать в конец */

/* --- Сокеты (M8d3/M8e): подмножество BSD-сокетов поверх сисколлов. IPv4, UDP и TCP. --- */

typedef unsigned short sa_family_t;
typedef unsigned int socklen_t;

#define AF_INET 2     /* домен: IPv4 */
#define SOCK_STREAM 1 /* тип: поток с установлением соединения (TCP) */
#define SOCK_DGRAM 2  /* тип: датаграммы без соединения (UDP) */

/* IPv4-адрес в сетевом порядке байт. */
struct in_addr {
    unsigned int s_addr;
};

/* Адрес IPv4-эндпоинта (16 байт; раскладка как в Linux). `sin_port`/`sin_addr` — сетевой порядок. */
struct sockaddr_in {
    sa_family_t sin_family;   /* AF_INET */
    unsigned short sin_port;  /* порт (htons) */
    struct in_addr sin_addr;  /* адрес */
    unsigned char sin_zero[8];
};

/* Создать сокет: domain=AF_INET, type=SOCK_DGRAM/SOCK_STREAM, protocol=0. fd (≥0) или -errno. */
int socket(int domain, int type, int protocol);
/* Привязать сокет к локальному порту из `addr` (UDP). 0 или -errno. */
int bind(int fd, const struct sockaddr_in *addr, socklen_t addrlen);
/* Установить TCP-соединение с `addr` (блокирующе). 0 или -errno. */
int connect(int fd, const struct sockaddr_in *addr, socklen_t addrlen);
/* Отправить/принять в установленном (TCP) сокете. Возвращают число байт или -errno. */
long send(int fd, const void *buf, size_t n, int flags);
long recv(int fd, void *buf, size_t n, int flags);
/* Отправить датаграмму на `dst`. Возвращает число байт или -errno. `flags` игнорируется. */
long sendto(int fd, const void *buf, size_t n, int flags, const struct sockaddr_in *dst,
            socklen_t dstlen);
/* Принять датаграмму (блокирующе). Если `src`/`srclen` не NULL — туда адрес отправителя. */
long recvfrom(int fd, void *buf, size_t n, int flags, struct sockaddr_in *src, socklen_t *srclen);

/* Хост→сеть для 16-битного порта (x86 — little-endian, поэтому переставляем байты). */
static inline unsigned short htons(unsigned short x) {
    return (unsigned short)((x << 8) | (x >> 8));
}

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

/* --- Потоковый ввод-вывод stdio (`FILE *`) (M9n) --- */

/* Конец файла / признак ошибки для функций чтения. */
#define EOF (-1)

/* Поток поверх дескриптора. БЕЗ буферизации (каждый getc/putc = syscall) — минимально и корректно;
 * буферизацию (быстрее, меньше syscall'ов) добавим позже (см. HARDENING). Поля трогать снаружи не
 * надо — только через функции ниже. */
typedef struct {
    int fd;    /* дескриптор */
    int flags; /* внутренние биты: достигнут EOF / была ошибка */
} FILE;

/* Стандартные потоки: дескрипторы 0/1/2. */
extern FILE *stdin;
extern FILE *stdout;
extern FILE *stderr;

/* Открыть/закрыть поток. `mode`: "r"/"w"/"a" (+ "+" для чтения-записи); 'b' игнорируется.
 * `fopen` возвращает `FILE *` или NULL; `fclose` — 0 или EOF. */
FILE *fopen(const char *path, const char *mode);
int fclose(FILE *f);

/* Чтение. `fgetc`/`getc` возвращают байт (0..255) или EOF. `fgets` читает в `s` не больше `size-1`
 * символов, останавливаясь после '\n' или на EOF, и завершает строку нулём; возвращает `s` или NULL
 * (нечего читать). `fread` читает `nmemb` элементов по `size` байт; возвращает число прочитанных
 * элементов. */
int fgetc(FILE *f);
int getc(FILE *f);
char *fgets(char *s, int size, FILE *f);
size_t fread(void *ptr, size_t size, size_t nmemb, FILE *f);

/* Запись. `fputc`/`putc` пишут байт (возвращают его или EOF). `fputs` пишет строку (≥0 или EOF).
 * `fwrite` пишет `nmemb` элементов по `size` байт; возвращает число записанных элементов. `fprintf`
 * форматирует (как `printf`) и пишет в поток; возвращает число символов. `fflush` для небуферизованного
 * потока — no-op (возвращает 0). */
int fputc(int c, FILE *f);
int putc(int c, FILE *f);
int fputs(const char *s, FILE *f);
size_t fwrite(const void *ptr, size_t size, size_t nmemb, FILE *f);
int fprintf(FILE *f, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
int fflush(FILE *f);

/* Признаки состояния потока. */
int feof(FILE *f);
int ferror(FILE *f);

#endif /* FERROS_LIBC_H */
