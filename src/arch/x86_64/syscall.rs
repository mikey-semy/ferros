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
    // Эпилог восстановления и возврата в кольцо 3 — отдельная метка: на него же «приземляется»
    // первое переключение на РЕБЁНКА fork (M6f3). Ребёнок стартует с подделанной копией
    // SyscallRegs (rax=0) на своём ядровом стеке и проходит ровно этот же путь, поэтому
    // восстанавливает ВЕСЬ регистровый контекст родителя — без дублирования логики.
    ".global ferros_syscall_return",
    "ferros_syscall_return:",
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
    /// Эпилог `syscall` (восстановление SyscallRegs + `sysretq`); цель первого запуска ребёнка
    /// fork — см. [`init_fork_child_stack`].
    fn ferros_syscall_return();
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
    // `execve` (и далее `fork`) переписывают сохранённое состояние возврата пользователя
    // (rip/rsp/rflags/регистры), поэтому обрабатываются здесь, где доступен весь SyscallRegs;
    // остальные системные вызовы идут в переносимый диспетчер и лишь возвращают i64 в rax.
    if regs.rax == crate::syscall::abi::SYS_EXECVE {
        exec(regs, regs.rdi);
        return;
    }
    if regs.rax == crate::syscall::abi::SYS_FORK {
        fork(regs);
        return;
    }
    let args = [regs.rdi, regs.rsi, regs.rdx, regs.r10, regs.r8, regs.r9];
    regs.rax = crate::syscall::dispatch(regs.rax, args, regs.user_rsp) as u64;
}

/// `execve(path, argv, envp)` (M6f2): заменяет образ текущего процесса программой, прочитанной
/// **с диска** (FAT). `argv`/`envp` пока игнорируем. При успехе не возвращается «как вызов» —
/// переписывает сохранённое состояние так, что `sysretq` уходит в точку входа новой программы;
/// при ошибке кладёт `-errno` в `rax`, а старый образ процесса остаётся нетронутым.
fn exec(regs: &mut SyscallRegs, path_ptr: u64) {
    use crate::syscall::abi;

    // 1) Путь — из памяти СТАРОГО (ещё активного) процесса.
    let path = match crate::syscall::uaccess::read_user_cstr(path_ptr) {
        Ok(p) => p,
        Err(errno) => {
            regs.rax = (-errno) as u64;
            return;
        }
    };
    let path = match core::str::from_utf8(&path) {
        Ok(s) => s,
        Err(_) => {
            regs.rax = (-abi::ENOENT) as u64;
            return;
        }
    };

    // 2) Байты программы — из файловой системы.
    let bytes = match crate::fs::open(path) {
        Ok(b) => b,
        Err(_) => {
            regs.rax = (-abi::ENOENT) as u64;
            return;
        }
    };

    // 2b) Копируем argv/envp из памяти СТАРОГО (ещё активного) процесса в кучу ядра — позже
    //     запишем их на стек нового образа. Старый адрес становится недоступен после смены CR3.
    let argv = match read_user_str_array(regs.rsi) {
        Ok(v) => v,
        Err(errno) => {
            regs.rax = (-errno) as u64;
            return;
        }
    };
    let envp = match read_user_str_array(regs.rdx) {
        Ok(v) => v,
        Err(errno) => {
            regs.rax = (-errno) as u64;
            return;
        }
    };

    let phys_offset = crate::mm::paging::phys_mem_offset();
    let (old_pml4, cr3_flags) = Cr3::read();
    // Копируем ИМЕННО ядровую таблицу: активна сейчас таблица текущего процесса, её
    // пользовательский слот в новое пространство тащить нельзя (см. new_sharing_kernel).
    let kernel_pml4 = crate::sched::thread::kernel_cr3();

    // 3) Строим новое адресное пространство и грузим в него ELF, ВРЕМЕННО активируя его, и
    //    возвращаем активным старое — чтобы ошибка загрузки оставила процесс неизменным.
    let built = crate::mm::frame::with_global(|fa| {
        // SAFETY: phys_offset корректен; копируем ядровую таблицу (без чужого user-слота).
        let aspace = unsafe { AddressSpace::new_sharing_kernel(phys_offset, kernel_pml4, fa) };
        // SAFETY: в новом пространстве отображено ядро, поэтому код ядра продолжает работать.
        unsafe { Cr3::write(aspace.pml4_frame(), cr3_flags) };
        // Грузим ELF, отображаем страницу стека и строим на ней начальный стек (argc/argv/
        // envp) — всё пока активно пространство нового образа. `Err(errno)` различает битый ELF
        // (`ENOEXEC`) и не влезший на стек argv (`E2BIG`).
        let loaded: Result<(u64, u64), i64> = {
            // SAFETY: единственный живой маппер на это пространство в пределах блока.
            let mut m = unsafe { aspace.mapper(phys_offset) };
            match crate::syscall::elf::load(&bytes, &mut m, fa) {
                Ok(entry) => {
                    let stack_page = Page::containing_address(VirtAddr::new(USER_STACK_VA));
                    crate::mm::paging::map_user_page(stack_page, &mut m, fa);
                    // SAFETY: CR3 = новое пространство, страница стека отображена user+writable.
                    match unsafe {
                        build_user_stack(USER_STACK_VA + 4096, USER_STACK_VA, &argv, &envp)
                    } {
                        Ok(rsp) => Ok((entry, rsp)),
                        Err(()) => Err(abi::E2BIG),
                    }
                }
                Err(_) => Err(abi::ENOEXEC),
            }
        };
        // SAFETY: возвращаем активным старое пространство (на случай ошибки — оно цело).
        unsafe { Cr3::write(old_pml4, cr3_flags) };
        match loaded {
            Ok((entry, rsp)) => Ok((aspace.pml4_frame(), entry, rsp)),
            Err(errno) => {
                // SAFETY: свежесобранное (сейчас неактивное) пространство бросаем — освобождаем.
                unsafe { aspace.destroy(phys_offset, fa) };
                Err(errno)
            }
        }
    });
    let (new_pml4, entry, user_rsp) = match built {
        Some(Ok(x)) => x,
        Some(Err(errno)) => {
            regs.rax = (-errno) as u64;
            return;
        }
        None => {
            // Глобальный аллокатор не установлен (execve до загрузки ядра) — не должно быть.
            regs.rax = (-abi::ENOMEM) as u64;
            return;
        }
    };

    // 4) Коммит: переключаем текущий поток на новое пространство, освобождаем старое,
    //    переносим таблицу дескрипторов (fd переживают exec).
    let old_pml4 = crate::sched::thread::exec_replace_cr3(new_pml4);
    // SAFETY: активируем новое пространство — `sysretq` ниже разрешает rip/rsp уже в нём.
    unsafe { Cr3::write(new_pml4, cr3_flags) };
    // SAFETY: старое пространство теперь неактивно (мы на новом) — разбираем его.
    crate::mm::frame::with_global(|fa| unsafe {
        AddressSpace::from_pml4_frame(old_pml4).destroy(phys_offset, fa);
    });
    crate::syscall::files::rekey_process(
        old_pml4.start_address().as_u64(),
        new_pml4.start_address().as_u64(),
    );

    // 5) Переписываем сохранённое состояние пользователя: чистый старт новой программы.
    //    `user_rsp` указывает на `argc` построенного начального стека (System V ABI); регистры
    //    зануляем — аргументы программа берёт со стека, не из них.
    regs.rip = entry;
    regs.user_rsp = user_rsp;
    regs.rflags = 0x202; // IF=1 + зарезервированный бит
    regs.rax = 0;
    regs.rdi = 0;
    regs.rsi = 0;
    regs.rdx = 0;
    regs.r10 = 0;
    regs.r8 = 0;
    regs.r9 = 0;
    regs.rbx = 0;
    regs.rbp = 0;
    regs.r12 = 0;
    regs.r13 = 0;
    regs.r14 = 0;
    regs.r15 = 0;
}

/// `fork()` (M6f3): создаёт ребёнка — копию текущего процесса. У ребёнка СВОЁ адресное
/// пространство (полная копия пользовательских страниц родителя), СВОЯ копия таблицы
/// дескрипторов и СВОЙ ядровый стек. Ребёнок «возвращается» из этого же `fork` с `rax = 0`;
/// родитель получает PID ребёнка. При ошибке (нет глобального аллокатора) — `-errno` родителю.
///
/// CR3 здесь не трогаем: копирование адресного пространства идёт через отображение физпамяти
/// (`phys_offset`), а ребёнок начнёт исполняться в своём пространстве позже — планировщик
/// переключит CR3 при первом переходе на его задачу.
fn fork(regs: &mut SyscallRegs) {
    use crate::syscall::abi;

    let phys_offset = crate::mm::paging::phys_mem_offset();
    let (parent_pml4, _) = Cr3::read();
    let kernel_pml4 = crate::sched::thread::kernel_cr3();

    // 1) Адресное пространство ребёнка = копия родительского (содержимое страниц копируется).
    let child = crate::mm::frame::with_global(|fa| {
        // SAFETY: parent_pml4 — активное (вызывающее) пространство; копируем его приватное
        // поддерево в новое, копируя именно ядровую таблицу для общих с ядром записей.
        unsafe { AddressSpace::fork_from(parent_pml4, phys_offset, kernel_pml4, fa) }
    });
    let child_pml4 = match child {
        Some(a) => a.pml4_frame(),
        None => {
            // Глобальный аллокатор не установлен (fork до загрузки ядра) — не должно быть.
            regs.rax = (-abi::ENOMEM) as u64;
            return;
        }
    };

    // 2) Таблица дескрипторов ребёнка = копия родительской (открытые файлы переживают fork).
    crate::syscall::files::fork_fds(
        parent_pml4.start_address().as_u64(),
        child_pml4.start_address().as_u64(),
    );

    // 3) Ядровый стек ребёнка: первое переключение уведёт в кольцо 3 в ту же точку, откуда
    //    родитель звал `fork`, со ВСЕМ его регистровым контекстом, но с rax=0.
    let mut kstack = alloc::vec![0u8; USER_KERNEL_STACK_SIZE].into_boxed_slice();
    let ktop = (kstack.as_mut_ptr() as usize + kstack.len()) & !0xF;
    // SAFETY: `ktop` — вершина свежего выровненного ядрового стека; сохранённые в `regs`
    // rip/user_rsp указывают в отображённую кольцо-3 память пространства РЕБЁНКА (копию роди-
    // тельской — те же VA, своё содержимое).
    let rsp = unsafe { init_fork_child_stack(ktop as *mut u8, regs) };

    // 4) Регистрируем ребёнка (parent = текущий PID) и возвращаем его PID родителю.
    let child_pid = crate::sched::thread::add_user_task(rsp, child_pml4, ktop as u64, kstack);
    regs.rax = child_pid as u64;
}

/// Готовит ядровый стек ребёнка `fork` (M6f3): первое переключение `switch_context` снимет 6
/// нулевых callee-saved и `ret`-нёт в эпилог [`ferros_syscall_return`], а тот восстановит
/// ПОДДЕЛАННУЮ копию [`SyscallRegs`] родителя (с `rax = 0`) и `sysretq`-нёт в кольцо 3: ребёнок
/// «возвращается» из того же `fork`, что и родитель, получая 0 и весь его регистровый контекст.
/// Возвращает начальный `rsp` (в ядровом стеке ребёнка).
///
/// Раскладка (от старших адресов к младшим): сверху — копия `SyscallRegs` в ТОМ ЖЕ порядке,
/// что кладёт входной трамплин (`user_rsp` старшим … `r15` младшим); под ней — кадр
/// `switch_context` (`[ferros_syscall_return][rbp=0][rbx=0][r12=0][r13=0][r14=0][r15=0]`).
///
/// # Safety
/// `kstack_top` — вершина свежего, выровненного по 16 байт ядрового стека (≥ 23 слов).
/// Сохранённые в `parent` `rip`/`user_rsp` указывают в отображённую кольцо-3 память РЕБЁНКА.
unsafe fn init_fork_child_stack(kstack_top: *mut u8, parent: &SyscallRegs) -> u64 {
    let mut sp = kstack_top as *mut u64;
    let mut push = |value: u64| {
        // SAFETY: sp идёт вниз по выделенному ядровому стеку достаточного размера.
        unsafe {
            sp = sp.sub(1);
            sp.write(value);
        }
    };

    // Копия SyscallRegs в порядке push входного трамплина (user_rsp старшим … r15 младшим).
    // Единственное отличие от родителя — rax = 0 (значение, которое fork() вернёт ребёнку).
    push(parent.user_rsp);
    push(0); // rax = 0 (ребёнок)
    push(parent.rip);
    push(parent.rflags);
    push(parent.rdi);
    push(parent.rsi);
    push(parent.rdx);
    push(parent.r10);
    push(parent.r8);
    push(parent.r9);
    push(parent.rbx);
    push(parent.rbp);
    push(parent.r12);
    push(parent.r13);
    push(parent.r14);
    push(parent.r15);

    // Кадр switch_context: адрес возврата (ret → эпилог), затем 6 нулевых callee-saved —
    // их switch_context снимет (pop r15,r14,r13,r12,rbx,rbp), после чего ret уйдёт в эпилог.
    push(ferros_syscall_return as *const () as u64);
    push(0); // rbp
    push(0); // rbx
    push(0); // r12
    push(0); // r13
    push(0); // r14
    push(0); // r15

    sp as u64
}

/// Резервный стек ядра под `syscall` — действует лишь ДО первого переключения на
/// пользовательскую задачу. С M6f4 планировщик при каждом таком переключении подменяет
/// [`SYSCALL_KERNEL_RSP`] на СОБСТВЕННЫЙ ядровый стек процесса (см. [`set_syscall_stack`]),
/// поэтому `syscall` идёт на стеке процесса. До первого пользовательского кода syscalls не
/// бывает (потоки ядра в кольце 0 их не делают), так что этот буфер по сути не используется.
const SYSCALL_STACK_SIZE: usize = 4096 * 5;
static mut SYSCALL_STACK: [u8; SYSCALL_STACK_SIZE] = [0; SYSCALL_STACK_SIZE];

/// Ставит ядровый стек под `syscall` равным `top` — собственному ядровому стеку текущего
/// процесса. Зовётся планировщиком ([`crate::arch::context::switch_task`]) при каждом
/// переключении на задачу со своим стеком (M6f4). Зачем: блокирующий вызов (`wait`) уступает
/// CPU из середины обработки syscall; если бы все процессы делили один стек, следующий вызов
/// затёр бы кадр заблокированного. На своём стке кадр процесса переживает блокировку.
pub fn set_syscall_stack(top: u64) {
    SYSCALL_KERNEL_RSP.store(top & !0xF, Ordering::SeqCst);
}

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

// Гарантия на этапе компиляции: [`build_user_stack`] выравнивает стек вниз `sp &= !0xF` БЕЗ
// повторной проверки нижней границы — это безопасно ровно потому, что сам пол страницы стека
// (`USER_STACK_VA`) выровнен по 16: выравнивание может дойти до пола, но не уйти под него.
const _: () = assert!(USER_STACK_VA.is_multiple_of(16));

/// Размер ядрового стека пользовательского процесса (20 КиБ): на нём строится начальный
/// контекст (трамплин входа), на него (rsp0) садятся прерывания из кольца 3, и с M6f4 на нём же
/// исполняется `syscall` этого процесса (раньше — общий стек; 5 страниц как у того общего).
const USER_KERNEL_STACK_SIZE: usize = 4096 * 5;

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
    // SAFETY: phys_offset корректен; копируем именно ядровую таблицу (она сейчас активна).
    let aspace =
        unsafe { AddressSpace::new_sharing_kernel(phys_offset, kernel_pml4, frame_allocator) };
    // SAFETY: в пространстве процесса отображено ядро, поэтому код ядра продолжает работать.
    unsafe { Cr3::write(aspace.pml4_frame(), cr3_flags) };

    let (entry, user_stack_top) = {
        // SAFETY: единственный живой маппер на это пространство в пределах блока.
        let mut pmapper = unsafe { aspace.mapper(phys_offset) };
        let entry = crate::syscall::elf::load(elf_bytes, &mut pmapper, frame_allocator)
            .expect("failed to load user ELF");
        let stack_page = Page::containing_address(VirtAddr::new(USER_STACK_VA));
        crate::mm::paging::map_user_page(stack_page, &mut pmapper, frame_allocator);
        // Начальный процесс не получает аргументов: строим пустой System V-стек (argc = 0),
        // чтобы раскладка стека была валидной и единообразной с `execve`. CR3 = это пространство.
        // SAFETY: страница стека только что отображена user+writable в активной таблице.
        let user_stack_top = unsafe {
            build_user_stack(USER_STACK_VA + 4096, USER_STACK_VA, &[], &[])
                .expect("initial user stack does not fit")
        };
        (entry, user_stack_top)
    };

    // Возвращаем активным пространство ядра.
    // SAFETY: kernel_pml4 — сохранённый корень таблиц ядра.
    unsafe { Cr3::write(kernel_pml4, cr3_flags) };
    if was_enabled {
        interrupts::enable();
    }

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

/// Верхняя граница числа элементов `argv`/`envp`, читаемых из памяти пользователя (M7b) —
/// чтобы битый/злонамеренный массив без `NULL` не зациклил чтение.
const MAX_STR_ARRAY: usize = 128;

/// Читает из ТЕКУЩЕГО (старого) адресного пространства `NULL`-терминированный массив
/// пользовательских указателей на C-строки (`argv`/`envp` для `execve`, M7b) во владелые копии
/// ядра. Останавливается на нулевом указателе или [`MAX_STR_ARRAY`]. `array_ptr == 0` (нет
/// массива) → пустой результат. Возвращает положительный `errno` при плохом указателе/строке
/// (вызывающий вернёт `-errno`), `E2BIG` — если элементов слишком много.
fn read_user_str_array(array_ptr: u64) -> Result<alloc::vec::Vec<alloc::vec::Vec<u8>>, i64> {
    use crate::syscall::uaccess;
    let mut out = alloc::vec::Vec::new();
    if array_ptr == 0 {
        return Ok(out);
    }
    for i in 0..MAX_STR_ARRAY {
        // Указатель-элемент — это 8 байт в памяти пользователя (uaccess сам проверит диапазон).
        let slot = array_ptr + (i as u64) * 8;
        let ptr = uaccess::with_user_bytes(slot, 8, |b| {
            u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
        })?;
        if ptr == 0 {
            return Ok(out); // конец массива
        }
        out.push(uaccess::read_user_cstr(ptr)?);
    }
    Err(crate::syscall::abi::E2BIG)
}

/// Строит начальный стек процесса по System V AMD64 ABI на вершине его страницы стека (M7b).
/// Вызывать, когда АКТИВНО адресное пространство процесса (CR3 на него) и страница стека
/// `[floor, top)` уже отображена present+user+writable: пишем прямо по пользовательским VA
/// (кольцо 0 может).
///
/// Раскладка на входе в `_start` (от младших адресов к старшим), `rsp` указывает на `argc`:
/// `[argc][argv0…][NULL][envp0…][NULL][auxv: AT_NULL=0,0]`; строки лежат выше, и всё выровнено
/// так, что `rsp % 16 == 0` (требование ABI к точке входа процесса). `argv`/`envp` — владелые
/// копии строк ядра (без терминирующего нуля — дописываем его). Возвращает `rsp` либо `Err(())`,
/// если содержимое не помещается в страницу (вызывающий трактует как `E2BIG`).
///
/// # Safety
/// CR3 = адресное пространство процесса; страница `[floor, top)` отображена user+writable.
unsafe fn build_user_stack(
    top: u64,
    floor: u64,
    argv: &[alloc::vec::Vec<u8>],
    envp: &[alloc::vec::Vec<u8>],
) -> Result<u64, ()> {
    let mut sp = top;

    // Пишет строку (с дописанным нулём) вниз от вершины, возвращает её VA. Инвариант: sp ≥ floor.
    let put_str = |sp: &mut u64, s: &[u8]| -> Result<u64, ()> {
        let need = s.len() as u64 + 1; // +1 под завершающий нуль
        if *sp - floor < need {
            return Err(());
        }
        *sp -= need;
        // SAFETY: [*sp, *sp+need) внутри отображённой страницы стека; пишем байты строки и нуль.
        unsafe {
            core::ptr::copy_nonoverlapping(s.as_ptr(), *sp as *mut u8, s.len());
            (*sp as *mut u8).add(s.len()).write(0);
        }
        Ok(*sp)
    };
    // Кладёт 8-байтное слово вниз. Инвариант: sp ≥ floor.
    let push = |sp: &mut u64, v: u64| -> Result<(), ()> {
        if *sp - floor < 8 {
            return Err(());
        }
        *sp -= 8;
        // SAFETY: *sp внутри отображённой страницы, выровнен по 8 (sp двигаем кратно 8/выровняв).
        unsafe { (*sp as *mut u64).write(v) };
        Ok(())
    };

    // 1) Строки argv, затем envp — наверх страницы, запоминаем их пользовательские адреса.
    let mut arg_ptrs = alloc::vec::Vec::with_capacity(argv.len());
    for s in argv {
        arg_ptrs.push(put_str(&mut sp, s)?);
    }
    let mut env_ptrs = alloc::vec::Vec::with_capacity(envp.len());
    for s in envp {
        env_ptrs.push(put_str(&mut sp, s)?);
    }

    // 2) Граница под массивы указателей — выровнять вниз по 16.
    sp &= !0xF;

    // 3) Падинг, чтобы итоговый argc лёг на 16: каждый слот 8 байт, нужно ЧЁТНОЕ число слотов.
    //    Слоты (младший→старший): argc(1) + argv(n) + NULL(1) + envp(m) + NULL(1) + auxv AT_NULL(2).
    let slots = 1 + arg_ptrs.len() + 1 + env_ptrs.len() + 1 + 2;
    if slots % 2 == 1 {
        push(&mut sp, 0)?; // лишний слот выше auxv (программа его не читает)
    }

    // 4) Пишем массивы ВНИЗ (последний push ложится по младшему адресу = argc). auxv → envp → argv.
    push(&mut sp, 0)?; // auxv: a_val (AT_NULL)
    push(&mut sp, 0)?; // auxv: a_type = AT_NULL (конец auxv)
    push(&mut sp, 0)?; // конец envp
    for &p in env_ptrs.iter().rev() {
        push(&mut sp, p)?;
    }
    push(&mut sp, 0)?; // конец argv
    for &p in arg_ptrs.iter().rev() {
        push(&mut sp, p)?;
    }
    push(&mut sp, arg_ptrs.len() as u64)?; // argc

    Ok(sp) // указывает на argc, выровнен по 16
}
