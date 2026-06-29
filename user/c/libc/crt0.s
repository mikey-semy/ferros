# crt0 минимальной libc ferros (M9h): точка входа C-программы со стандартным `int main()`.
#
# Ядро передаёт управление сюда с начальным стеком System V (как и для Rust-программ):
#   rsp → argc | argv[0] … argv[argc-1] | NULL | envp[0] … | NULL | auxv…
# Раскладываем argc/argv/envp по регистрам C-ABI (rdi/rsi/rdx) и зовём main; его возврат (eax)
# передаём в exit(). На входе ядро гарантирует rsp, выровненный по 16 — после `call` (он кладёт
# адрес возврата) main получает rsp ≡ 8 (mod 16), как требует ABI.

.intel_syntax noprefix
.global _start
_start:
    mov  rdi, [rsp]               # argc
    lea  rsi, [rsp + 8]           # argv = &стек[1]
    lea  rdx, [rsp + rdi*8 + 16]  # envp = argv + (argc+1) = rsp + 8 + (argc+1)*8
    xor  rbp, rbp                 # конец цепочки кадров (frame pointer = 0)
    call main
    mov  edi, eax                 # код возврата main → аргумент exit
    call exit
1:  hlt                           # exit не возвращается; страховка от «провала» сюда
    jmp  1b
