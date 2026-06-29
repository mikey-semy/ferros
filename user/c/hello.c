/*
 * Первая программа ferros на C (M9g): доказывает, что обычный C-код, собранный clang'ом,
 * запускается в кольце 3 и делает Linux-сисколлы. Это фундамент под порт libc — всё, что даёт
 * libc, лежит поверх такого пути.
 *
 * Свободностоящая (freestanding): без libc и crt0. Свой `_start`, свои тонкие обёртки над
 * инструкцией `syscall` (соглашение Linux x86-64: номер в rax; аргументы rdi/rsi/rdx; rcx/r11
 * затираются). Собирается build.rs'ом: clang + lld + наш user/hello/linker.ld (база
 * 0x7F80_0000_0000, ENTRY(_start)); модель кода `large` — иначе 32-битные релокации не достают до
 * высокого пользовательского адреса.
 */

typedef unsigned long size_t;

/* write(fd, buf, len) — сисколл 1. */
static long sys_write(long fd, const void *buf, size_t len) {
    long ret;
    __asm__ volatile("syscall"
                     : "=a"(ret)
                     : "a"(1L), "D"(fd), "S"(buf), "d"(len)
                     : "rcx", "r11", "memory");
    return ret;
}

/* exit(code) — сисколл 60, не возвращается. */
__attribute__((noreturn)) static void sys_exit(long code) {
    __asm__ volatile("syscall" : : "a"(60L), "D"(code));
    __builtin_unreachable();
}

/* Длина C-строки (своя — libc нет). */
static size_t str_len(const char *s) {
    size_t n = 0;
    while (s[n]) {
        n++;
    }
    return n;
}

/* Точка входа: ядро передаёт управление сюда (см. linker.ld ENTRY(_start)). */
void _start(void) {
    static const char msg[] = "Hello from C on ferros!\n";
    sys_write(1, msg, str_len(msg));
    sys_exit(0);
}
