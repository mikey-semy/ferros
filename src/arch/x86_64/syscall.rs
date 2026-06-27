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
use crate::mm::addr_space::AddressSpace;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::interrupts;
use x86_64::registers::control::Cr3;
use x86_64::registers::model_specific::{Efer, EferFlags, LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;
use x86_64::structures::paging::{FrameAllocator, Page, Size4KiB};
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

extern "C" {
    fn ferros_syscall_entry();
}

/// Glue между голым трамплином и переносимым диспетчером: распаковывает [`SyscallRegs`],
/// зовёт [`crate::syscall::dispatch`] с аргументами по Linux-ABI и кладёт результат в `rax`.
///
/// Зовётся ТОЛЬКО из `ferros_syscall_entry`. Для `exit` диспетчер не возвращается (поток
/// завершается планировщиком), поэтому `rax` тогда не присваивается — и `sysretq` не будет.
#[no_mangle]
extern "C" fn ferros_syscall_dispatch(regs: *mut SyscallRegs) {
    // SAFETY: трамплин только что построил валидный SyscallRegs на стеке ядра и передал
    // на него указатель в rdi.
    let regs = unsafe { &mut *regs };
    let args = [regs.rdi, regs.rsi, regs.rdx, regs.r10, regs.r8, regs.r9];
    regs.rax = crate::syscall::dispatch(regs.rax, args, regs.user_rsp) as u64;
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

/// Виртуальный адрес страницы стека пользователя. Лежит в L4-слоте 255 (`0x7F80…`) —
/// том же приватном слоте, что и сегменты ELF программы (см. `user/hello/linker.ld`), —
/// чтобы вся пользовательская память была в одном свободном у ядра слоте.
pub const USER_STACK_VA: u64 = 0x7F80_1000_0000;

/// Размер ядрового стека пользовательского процесса (16 КиБ): на нём строится начальный
/// контекст (трамплин входа) и на него (rsp0) садятся прерывания из кольца 3.
const USER_KERNEL_STACK_SIZE: usize = 4096 * 4;

/// Создаёт **пользовательский процесс** из ELF-образа и регистрирует его в планировщике как
/// поток (M5c3): заводит процессу своё адресное пространство (свой PML4, память ядра общая),
/// загружает в него сегменты ELF и отводит страницу под пользовательский стек, выделяет
/// ядровый стек (rsp0 + начальный контекст), готовит вход в кольцо 3
/// ([`crate::arch::context::init_user_thread_stack`]) и добавляет задачу в планировщик
/// ([`crate::sched::thread::add_user_task`]). Сам в кольцо 3 НЕ входит — это сделает
/// планировщик при первом переключении на эту задачу.
///
/// Загрузка сегментов идёт при ВРЕМЕННО активном адресном пространстве процесса (так запись
/// содержимого по пользовательским адресам видна CPU); на это время гасим прерывания, после —
/// возвращаем активным пространство ядра.
///
/// # Safety
/// `phys_offset` — корректный оффсет физпамяти; `frame_allocator` валиден; куча поднята.
/// VA сегментов ELF / `USER_STACK_VA` должны попадать в слот, свободный у ядра.
///
/// # Panics
/// Если ELF не загрузился ([`crate::syscall::elf::load`] вернул ошибку).
pub unsafe fn spawn_user(
    elf_bytes: &[u8],
    phys_offset: VirtAddr,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    // Создаём адресное пространство и грузим в него ELF. На время активации чужого
    // пространства — без прерываний.
    let was_enabled = interrupts::are_enabled();
    interrupts::disable();

    let (kernel_pml4, cr3_flags) = Cr3::read();
    // SAFETY: phys_offset корректен; новое пространство содержит все отображения ядра.
    let aspace = unsafe { AddressSpace::new_sharing_kernel(phys_offset, frame_allocator) };
    // SAFETY: в пространстве процесса отображено ядро, поэтому код ядра продолжает работать.
    unsafe { Cr3::write(aspace.pml4_frame(), cr3_flags) };

    let entry = {
        // SAFETY: единственный живой маппер на это пространство в пределах блока.
        let mut pmapper = unsafe { aspace.mapper(phys_offset) };
        let entry = crate::syscall::elf::load(elf_bytes, &mut pmapper, frame_allocator)
            .expect("failed to load user ELF");
        let stack_page = Page::containing_address(VirtAddr::new(USER_STACK_VA));
        crate::mm::paging::map_user_page(stack_page, &mut pmapper, frame_allocator);
        entry
    };

    // Возвращаем активным пространство ядра.
    // SAFETY: kernel_pml4 — сохранённый корень таблиц ядра.
    unsafe { Cr3::write(kernel_pml4, cr3_flags) };
    if was_enabled {
        interrupts::enable();
    }

    let user_stack_top = USER_STACK_VA + 4096;

    // Ядровый стек процесса (куча — уже в пространстве ядра). Вершина выровнена вниз по 16.
    let mut kstack = alloc::vec![0u8; USER_KERNEL_STACK_SIZE].into_boxed_slice();
    let ktop = (kstack.as_mut_ptr() as usize + kstack.len()) & !0xF;

    let sel = gdt::selectors();
    // SAFETY: `ktop` — вершина свежего выровненного ядрового стека; `entry`/`user_stack_top`
    // отображены user-accessible в пространстве процесса; селекторы — кольца 3 (RPL=3).
    let rsp = unsafe {
        crate::arch::context::init_user_thread_stack(
            ktop as *mut u8,
            entry,
            user_stack_top,
            sel.user_code.0 as u64,
            sel.user_data.0 as u64,
        )
    };

    crate::sched::thread::add_user_task(rsp, aspace.pml4_frame(), ktop as u64, kstack);
}
