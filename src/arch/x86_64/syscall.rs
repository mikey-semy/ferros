//! Механизм `syscall`/`sysret` и переход ядро⇄кольцо 3 на x86_64 (M5a).
//!
//! # Что такое «кольца» и `syscall`
//!
//! Процессор x86 различает уровни привилегий — «кольца». Ядро живёт в **кольце 0**
//! (всё можно), пользовательские программы — в **кольце 3** (нельзя трогать железо и
//! чужую память). Чтобы из кольца 3 попросить ядро о услуге, есть быстрая инструкция
//! **`syscall`**: она мгновенно прыгает в ядро по адресу из MSR `LSTAR`, ставит сегменты
//! ядра из MSR `STAR` и переключает CPL в 0. Возврат — `sysret` (обратно в кольцо 3).
//! Это пришло на смену старому `int 0x80` и работает на порядок быстрее.
//!
//! # Соглашение вызова — Linux x86-64 (D8)
//!
//! Намеренно копируем Linux ABI с первого дня (см. `docs/DECISIONS.md` D8), чтобы тот же
//! слой потом носил и source-level POSIX (relibc, M9), и ABI-совместимость: номер вызова
//! в `rax`; аргументы в `rdi, rsi, rdx, r10, r8, r9`; результат в `rax` (отрицательное
//! значение — это `-errno`). `syscall` затирает `rcx` (сохранённый RIP) и `r11`
//! (сохранённый RFLAGS) — их трогаем как scratch, остальное обязаны вернуть пользователю.
//!
//! # Тонкость: стек при входе
//!
//! `syscall` НЕ меняет `rsp` — мы влетаем в ядро всё ещё на пользовательском (недоверенном)
//! стеке. Поэтому первое, что делает входной трамплин, — переключиться на стек ядра. На
//! одном ядре кладём его адрес в статик `SYSCALL_KERNEL_RSP` и грузим RIP-relative; на
//! будущее (SMP) — per-CPU через `swapgs` (в HARDENING.md).
//!
//! # Весь `unsafe` здесь (D9)
//!
//! Этот модуль — намеренный «шов» для опасного: голый asm переходов между кольцами,
//! запись MSR. Всё снабжено `// SAFETY`. Переносимый слой [`crate::syscall`] остаётся
//! безопасным Rust и зовётся отсюда.

use crate::arch::x86_64::gdt;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::interrupts;
use x86_64::registers::model_specific::{Efer, EferFlags, LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;
use x86_64::structures::paging::{FrameAllocator, OffsetPageTable, Page, Size4KiB};
use x86_64::VirtAddr;

/// Снимок регистров пользователя на входе в `syscall`, который строит входной трамплин
/// на стеке ядра. Порядок полей (`repr(C)`) совпадает с порядком `push` в трамплине —
/// поле по младшему адресу = последний `push`. Указатель на эту структуру передаётся в
/// [`ferros_syscall_dispatch`].
#[repr(C)]
pub struct SyscallRegs {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub r9: u64,  // arg6
    pub r8: u64,  // arg5
    pub r10: u64, // arg4
    pub rdx: u64, // arg3
    pub rsi: u64, // arg2
    pub rdi: u64, // arg1
    /// Сохранённый `r11` = RFLAGS пользователя (нужен для `sysretq`).
    pub rflags: u64,
    /// Сохранённый `rcx` = RIP возврата в пользователя (нужен для `sysretq`).
    pub rip: u64,
    /// Номер вызова на входе, результат на выходе.
    pub rax: u64,
    /// Указатель стека пользователя (мы сохраняем его при переключении на стек ядра).
    pub user_rsp: u64,
}

// Временное хранилище указателя стека пользователя на момент переключения на стек ядра,
// и сам стек ядра под `syscall`. На одном ядре syscalls не реентерабельны (входим с
// IF=0), поэтому единственного слота достаточно — per-CPU и реентерабельность в HARDENING.
//
// SAFETY-контракт: эти статики читает/пишет голый asm трамплинов как обычную u64-память.
// `AtomicU64` (внутри UnsafeCell) даёт легальную внутреннюю мутацию без `static mut`; на
// одном ядре конкурентного доступа нет.
static SYSCALL_SCRATCH: AtomicU64 = AtomicU64::new(0);
static SYSCALL_KERNEL_RSP: AtomicU64 = AtomicU64::new(0);
/// Сохранённый `rsp` ядра для возврата из кольца 3 обратно в вызвавший ядровый контекст
/// (см. [`ferros_enter_user`] / [`ferros_resume_kernel`]).
static KERNEL_RESUME_RSP: AtomicU64 = AtomicU64::new(0);

// Входной трамплин `syscall` (цель MSR LSTAR). Переключается на стек ядра, сохраняет
// регистры пользователя в SyscallRegs, зовёт диспетчер, восстанавливает регистры и
// `sysretq` обратно в кольцо 3. Синтаксис Intel (умолчание Rust).
core::arch::global_asm!(
    ".global ferros_syscall_entry",
    "ferros_syscall_entry:",
    "    mov [rip + {scratch}], rsp", // спрятать rsp пользователя
    "    mov rsp, [rip + {kstack}]",  // переключиться на стек ядра
    "    push qword ptr [rip + {scratch}]", // кадр: user_rsp
    "    push rax",
    "    push rcx", // RIP пользователя
    "    push r11", // RFLAGS пользователя
    "    push rdi",
    "    push rsi",
    "    push rdx",
    "    push r10",
    "    push r8",
    "    push r9",
    "    push rbx",
    "    push rbp",
    "    push r12",
    "    push r13",
    "    push r14",
    "    push r15", // 16 push'ей суммарно → rsp 16-выровнен перед call
    "    mov rdi, rsp", // &mut SyscallRegs
    "    call {dispatch}",
    "    pop r15",
    "    pop r14",
    "    pop r13",
    "    pop r12",
    "    pop rbp",
    "    pop rbx",
    "    pop r9",
    "    pop r8",
    "    pop r10",
    "    pop rdx",
    "    pop rsi",
    "    pop rdi",
    "    pop r11", // RFLAGS пользователя
    "    pop rcx", // RIP пользователя
    "    pop rax", // результат
    "    pop rsp", // восстановить rsp пользователя
    "    sysretq",
    scratch = sym SYSCALL_SCRATCH,
    kstack = sym SYSCALL_KERNEL_RSP,
    dispatch = sym ferros_syscall_dispatch,
);

// Переход ядро → кольцо 3 (через `iretq`) и обратный «возврат» в сохранённый контекст
// ядра (для одноразовой раскрутки из обработчика). Идея — как у переключения контекста
// в M4: сохранить callee-saved + rsp ядра, прыгнуть, а потом восстановить и `ret`.
//
// ВНИМАНИЕ: это БУТСТРАП M5a — единственный кольцо-3-заход за раз через глобальный
// `KERNEL_RESUME_RSP`. Настоящее планирование пользовательских потоков (вытеснение
// кольца 3 по таймеру через `rsp0`, много процессов) строится в M5c НЕ поверх этого.
core::arch::global_asm!(
    ".global ferros_enter_user",
    // rdi=entry, rsi=user_stack, rdx=user_cs, rcx=user_ss
    "ferros_enter_user:",
    "    push rbp",
    "    push rbx",
    "    push r12",
    "    push r13",
    "    push r14",
    "    push r15",
    "    mov [rip + {resume}], rsp", // сохранить rsp ядра для возврата
    "    push rcx",                  // iretq-кадр: SS = user_ss
    "    push rsi",                  //            RSP = user_stack
    "    push 0x2",                  //            RFLAGS (IF=0, зарезерв. бит 1)
    "    push rdx",                  //            CS = user_cs
    "    push rdi",                  //            RIP = entry
    "    iretq",
    ".global ferros_resume_kernel",
    "ferros_resume_kernel:",
    "    mov rsp, [rip + {resume}]",
    "    pop r15",
    "    pop r14",
    "    pop r13",
    "    pop r12",
    "    pop rbx",
    "    pop rbp",
    "    ret", // «возврат» к вызвавшему ferros_enter_user
    resume = sym KERNEL_RESUME_RSP,
);

// Первая пользовательская программа (M5b): крошечный позиционно-независимый код (только
// immediate'ы и `syscall`), который мы КОПИРУЕМ в пользовательскую страницу. Делает
// настоящие Linux-вызовы: `write(1, msg, len)` (печатает строку из памяти пользователя),
// затем `exit(0)` (ядро раскручивается обратно в вызвавший контекст). Хвостовой `jmp` —
// страховка: `exit` не возвращается.
core::arch::global_asm!(
    ".global ferros_user_hello_start",
    "ferros_user_hello_start:",
    "    mov eax, {sys_write}", // nr = write
    "    mov edi, 1",           // fd = stdout
    "    mov esi, {msg_va}",    // buf
    "    mov edx, {msg_len}",   // count
    "    syscall",
    "    mov eax, {sys_exit}", // nr = exit
    "    mov edi, 0",          // status = 0
    "    syscall",
    "2:  jmp 2b",
    ".global ferros_user_hello_end",
    "ferros_user_hello_end:",
    sys_write = const crate::syscall::abi::SYS_WRITE as i32,
    sys_exit = const crate::syscall::abi::SYS_EXIT as i32,
    msg_va = const USER_DATA_VA as i32,
    msg_len = const HELLO_MSG.len() as i32,
);

extern "C" {
    fn ferros_syscall_entry();
    /// Прыгает в кольцо 3 на `entry` с пользовательским стеком; «возвращается», когда
    /// пользователь сделает вызов с исходом [`crate::syscall::SyscallOutcome::LeaveUser`].
    fn ferros_enter_user(entry: u64, user_stack: u64, user_cs: u64, user_ss: u64);
    /// Восстанавливает контекст ядра, сохранённый [`ferros_enter_user`], и возвращается
    /// туда. Вызвавшему (диспетчеру) управление НЕ возвращает — отсюда тип `-> !`.
    fn ferros_resume_kernel() -> !;
    fn ferros_user_hello_start();
    fn ferros_user_hello_end();
}

/// Glue между голым трамплином и переносимым диспетчером: распаковывает [`SyscallRegs`],
/// зовёт [`crate::syscall::dispatch`] с аргументами по Linux-ABI и применяет исход.
///
/// Зовётся ТОЛЬКО из `ferros_syscall_entry`.
#[no_mangle]
extern "C" fn ferros_syscall_dispatch(regs: *mut SyscallRegs) {
    // SAFETY: трамплин только что построил валидный SyscallRegs на стеке ядра и передал
    // на него указатель в rdi.
    let regs = unsafe { &mut *regs };
    let args = [regs.rdi, regs.rsi, regs.rdx, regs.r10, regs.r8, regs.r9];
    match crate::syscall::dispatch(regs.rax, args, regs.user_rsp) {
        crate::syscall::SyscallOutcome::Return(value) => regs.rax = value as u64,
        crate::syscall::SyscallOutcome::LeaveUser => {
            // SAFETY: парный к ferros_enter_user; восстанавливает сохранённый контекст
            // ядра и возвращается в него (сюда уже не вернётся).
            unsafe { ferros_resume_kernel() }
        }
    }
}

/// Размер стека ядра под обработку `syscall` (5 страниц по 4 КиБ).
const SYSCALL_STACK_SIZE: usize = 4096 * 5;
static mut SYSCALL_STACK: [u8; SYSCALL_STACK_SIZE] = [0; SYSCALL_STACK_SIZE];

/// Настраивает `syscall`/`sysret`: включает `EFER.SCE`, прописывает селекторы колец в
/// `STAR`, точку входа в `LSTAR`, маску RFLAGS в `SFMASK` и стек ядра под syscall.
/// Вызывать один раз при старте, после [`gdt::init`] (нужны его селекторы).
pub fn init() {
    let sel = gdt::selectors();

    // SAFETY: включаем расширение syscall в long mode — штатный способ задействовать
    // инструкцию `syscall`; не трогаем прочие биты EFER.
    unsafe {
        Efer::update(|flags| flags.insert(EferFlags::SYSTEM_CALL_EXTENSIONS));
    }

    // STAR проверит раскладку GDT (kernel SS = CS+8, user CS/SS со смещением и RPL=3).
    Star::write(
        sel.user_code,
        sel.user_data,
        sel.kernel_code,
        sel.kernel_data,
    )
    .expect("invalid STAR selectors — check GDT layout");

    LStar::write(VirtAddr::new(ferros_syscall_entry as *const () as u64));

    // На входе в syscall гасим IF (прерывания) — обработчик идёт «без посторонних».
    SFMask::write(RFlags::INTERRUPT_FLAG);

    // Верхняя граница стека ядра под syscall, выровненная вниз по 16 (ABI вызова в
    // трамплине). SAFETY: &raw const не делает ссылку на static mut; память — стек.
    let top = VirtAddr::from_ptr(&raw const SYSCALL_STACK).as_u64() + SYSCALL_STACK_SIZE as u64;
    SYSCALL_KERNEL_RSP.store(top & !0xF, Ordering::SeqCst);
}

/// Виртуальный адрес страницы кода пользователя (нижняя половина — пользовательская часть
/// адресного пространства; в M5c у процессов будут свои таблицы).
pub const USER_CODE_VA: u64 = 0x4000_0000;
/// Виртуальный адрес страницы стека пользователя.
pub const USER_STACK_VA: u64 = 0x4010_0000;
/// Виртуальный адрес страницы данных пользователя (туда кладём строку для `write`).
pub const USER_DATA_VA: u64 = 0x4020_0000;
/// Строка, которую первая пользовательская программа печатает через `write` (M5b).
pub const HELLO_MSG: &[u8] = b"hello from ring 3\n";

/// Запускает первую пользовательскую программу (M5b): маппит страницы кода/стека/данных,
/// копирует туда код и строку, прыгает в кольцо 3. Программа печатает [`HELLO_MSG`] через
/// настоящий `write` и завершается `exit` — тогда управление возвращается сюда. Возвращает
/// вершину пользовательского стека (тест сверяет с ней зафиксированный `user_rsp`).
///
/// Маппинг идёт в активную (ядровую) таблицу: отдельного адресного пространства ещё нет
/// (M5c), а нижняя половина у ядра свободна. Состояние прерываний (IF) сохраняется и
/// восстанавливается: в кольцо 3 входим с IF=0 и обратно приходим с IF=0.
///
/// # Safety
/// Вызывать с корректными `mapper`/`frame_allocator` для активной таблицы и поднятой
/// кучей. `USER_CODE_VA`/`USER_STACK_VA`/`USER_DATA_VA` должны быть не отображены (иначе
/// `map_to` запаникует).
pub unsafe fn run_user_hello(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> u64 {
    for va in [USER_CODE_VA, USER_STACK_VA, USER_DATA_VA] {
        let page = Page::containing_address(VirtAddr::new(va));
        crate::mm::paging::map_user_page(page, mapper, frame_allocator);
    }

    // Копируем код программы из .text ядра в пользовательскую страницу. Он позиционно-
    // независим (immediate'ы + syscall), поэтому копирование безопасно.
    let start = ferros_user_hello_start as *const () as usize;
    let end = ferros_user_hello_end as *const () as usize;
    let len = end - start;
    // Код и строка копируются каждый в ОДНУ страницу — обязаны влезать, иначе copy ушёл
    // бы за её пределы (в следующую, неотображённую страницу → page fault).
    assert!(len <= 4096, "ring-3 program does not fit in one page");
    assert!(
        HELLO_MSG.len() <= 4096,
        "hello message does not fit in one page"
    );
    // SAFETY: src — диапазоны в ядре (.text и .rodata); dst — только что отображённые
    // пользовательские страницы (присутствуют, доступны на запись из кольца 0); длины
    // проверены выше.
    unsafe {
        core::ptr::copy_nonoverlapping(start as *const u8, USER_CODE_VA as *mut u8, len);
        core::ptr::copy_nonoverlapping(
            HELLO_MSG.as_ptr(),
            USER_DATA_VA as *mut u8,
            HELLO_MSG.len(),
        );
    }

    let user_stack_top = USER_STACK_VA + 4096;
    let sel = gdt::selectors();
    let was_enabled = interrupts::are_enabled();
    // SAFETY: страницы отображены user-accessible, селекторы кольца 3 валидны (RPL=3),
    // стек выровнен. Управление вернётся, когда программа сделает `exit`.
    unsafe {
        ferros_enter_user(
            USER_CODE_VA,
            user_stack_top,
            sel.user_code.0 as u64,
            sel.user_data.0 as u64,
        );
    }
    // Вернулись из кольца 3 с IF=0 — восстановим исходное состояние прерываний.
    if was_enabled {
        interrupts::enable();
    }
    user_stack_top
}
