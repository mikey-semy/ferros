//! Потоки/процессы ядра с переключением контекста: кооперативное (M4d), **вытесняющее по
//! таймеру** (M4e) и **пользовательские процессы со своим адресным пространством** (M5c3).
//!
//! # Что такое «поток» здесь
//!
//! Поток — отдельный стек + сохранённый набор регистров; он может уступить в любой точке,
//! потому что мы умеем сохранить/восстановить его регистры ([`crate::arch::context`]).
//! Поток ядра работает в кольце 0 и делит адресное пространство ядра. **Пользовательский
//! процесс** (M5c3) — это поток, у которого вдобавок СВОЁ адресное пространство (PML4) и
//! СВОЙ стек ядра (rsp0, на него процессор переключается при прерывании из кольца 3); он
//! исполняется в кольце 3.
//!
//! # Переключение
//!
//! Все пути сходятся в [`switch_to_next`] (round-robin, пропускает завершённые). Помимо
//! регистров и стека, при переходе он меняет **адресное пространство** (`CR3`) и **rsp0**
//! — это делает арх-шов [`crate::arch::context::switch_task`]. Так одно ядро честно делят
//! и потоки ядра, и изолированные пользовательские процессы.
//!
//! Причины переключиться: кооперативно ([`yield_now`]), по таймеру ([`on_timer_tick`],
//! M4e) и при завершении процесса ([`exit_current`]).

use crate::arch::context;
use crate::mm::addr_space::AddressSpace;
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use spin::Mutex;
use x86_64::instructions::interrupts;
use x86_64::structures::paging::PhysFrame;

/// Размер стека одного потока ядра (16 КиБ).
const STACK_SIZE: usize = 4096 * 4;

/// Включено ли вытеснение по таймеру. Пока `false`, обработчик таймера только считает
/// тики и не трогает потоки (поведение M2–M4d). Взводится [`start_preemption`].
static PREEMPTION_ENABLED: AtomicBool = AtomicBool::new(false);

/// Раздатчик идентификаторов процессов (PID, M6f1). PID 0 закреплён за «нулевым» потоком
/// ядра; пользовательские процессы и потоки ядра получают 1, 2, … .
static NEXT_PID: AtomicU32 = AtomicU32::new(1);

/// Состояние потока в планировщике.
#[derive(PartialEq, Eq, Clone, Copy)]
enum State {
    /// Готов исполняться.
    Runnable,
    /// Завершён (`exit`), но ресурсы ещё не освобождены. Планировщик его пропускает; reaper
    /// (M6e3) позже освободит его память.
    Dead,
    /// Завершён И освобождён reaper'ом (адресное пространство, стек, дескрипторы возвращены).
    /// Остаётся «надгробием» в `Vec` планировщика (структура `Thread` крошечная); уплотнение
    /// `Vec` — позже (HARDENING).
    Reaped,
}

/// Контекст потока: сохранённый указатель стека плюс владение его памятью; для
/// пользовательских процессов — ещё адресное пространство и стек ядра.
struct Thread {
    /// Идентификатор процесса (M6f1). 0 — «нулевой» поток ядра.
    pid: u32,
    /// PID родителя (кто создал этот поток/процесс). 0 — создан ядром на старте. Пишется
    /// сейчас, читается `wait` (2a4) — отсюда `allow(dead_code)` до тех пор.
    #[allow(dead_code)]
    parent: u32,
    /// Код завершения (`exit`); `None`, пока поток жив. Пишется сейчас, читается `wait` (2a4).
    #[allow(dead_code)]
    exit_status: Option<i32>,
    /// Сохранённый `rsp` потока (в его ядровом стеке). Обновляется при переключении прочь.
    rsp: u64,
    /// Память стека (ядрового). `None` у «нулевого» потока — он на стеке ядра от загрузчика.
    /// Держит память живой; reaper (M6e3) забирает её (`take`), чтобы освободить.
    stack: Option<Box<[u8]>>,
    /// Адресное пространство (корень PML4). `None` — общее пространство ядра (поток ядра).
    cr3: Option<PhysFrame>,
    /// Вершина стека ядра этого потока — ставится в rsp0 при переключении на него (нужно
    /// для входа из кольца 3). У потоков ядра не используется.
    kernel_stack_top: u64,
    /// Состояние.
    state: State,
}

/// Простой round-robin планировщик.
struct Scheduler {
    threads: Vec<Thread>,
    current: usize,
    /// Адресное пространство ядра — на него возвращаемся при переключении на поток ядра.
    kernel_cr3: PhysFrame,
}

static SCHEDULER: Mutex<Option<Scheduler>> = Mutex::new(None);

/// Инициализирует планировщик «нулевым» потоком — текущим контекстом ядра. Его `rsp`
/// заполнится при первом переключении прочь. Вызывать один раз, после инициализации кучи.
pub fn init() {
    let kernel_cr3 = context::current_address_space();
    // Без прерываний: замок `SCHEDULER` берёт и обработчик таймера (правило дедлока,
    // CONVENTIONS.md §4), поэтому держим его только с IF=0.
    interrupts::without_interrupts(|| {
        let mut guard = SCHEDULER.lock();
        *guard = Some(Scheduler {
            threads: vec![Thread {
                pid: 0, // «нулевой» поток ядра — PID 0
                parent: 0,
                exit_status: None,
                rsp: 0,
                stack: None,
                cr3: None,
                kernel_stack_top: 0,
                state: State::Runnable,
            }],
            current: 0,
            kernel_cr3,
        });
    });
}

/// Создаёт **поток ядра** (кольцо 0, общее адресное пространство) с точкой входа `entry`
/// (она не должна возвращаться — её тип `-> !`).
///
/// # Panics
/// Если планировщик не инициализирован ([`init`]).
pub fn spawn(entry: extern "C" fn() -> !) {
    let mut stack = vec![0u8; STACK_SIZE].into_boxed_slice();

    // Вершина стека (старший адрес), выровненная вниз по 16 байт.
    let top = stack.as_mut_ptr() as usize + stack.len();
    let top = (top & !0xF) as *mut u8;

    // SAFETY: `top` — вершина только что выделенного выровненного стека достаточного
    // размера; `entry` имеет тип `-> !` и не вернётся.
    let rsp = unsafe { context::init_thread_stack(top, entry) };

    let parent = current_pid();
    push_thread(Thread {
        pid: NEXT_PID.fetch_add(1, Ordering::SeqCst),
        parent,
        exit_status: None,
        rsp,
        stack: Some(stack),
        cr3: None,
        kernel_stack_top: top as u64,
        state: State::Runnable,
    });
}

/// Регистрирует **пользовательский процесс** как поток: `rsp` — начальный контекст в его
/// ядровом стеке (подготовлен [`context::init_user_thread_stack`]), `cr3` — корень его
/// адресного пространства, `kernel_stack_top` — вершина его ядрового стека (rsp0),
/// `kstack` — память этого стека (держим живой). Создаётся арх-слоем ([`spawn_user`]).
///
/// [`spawn_user`]: crate::arch::x86_64::syscall::spawn_user
pub fn add_user_task(rsp: u64, cr3: PhysFrame, kernel_stack_top: u64, kstack: Box<[u8]>) {
    let parent = current_pid();
    push_thread(Thread {
        pid: NEXT_PID.fetch_add(1, Ordering::SeqCst),
        parent,
        exit_status: None,
        rsp,
        stack: Some(kstack),
        cr3: Some(cr3),
        kernel_stack_top,
        state: State::Runnable,
    });
}

/// Меняет адресное пространство (CR3) ТЕКУЩЕГО потока на `new` и возвращает старое — для
/// `execve` (M6f2), который заменяет образ процесса, оставляя его поток/стек ядра прежними.
///
/// # Panics
/// Если текущий поток — поток ядра (без своего адресного пространства).
pub fn exec_replace_cr3(new: PhysFrame) -> PhysFrame {
    interrupts::without_interrupts(|| {
        let mut guard = SCHEDULER.lock();
        let sched = guard.as_mut().expect("scheduler not initialized");
        let cur = sched.current;
        let old = sched.threads[cur]
            .cr3
            .expect("exec on a kernel thread (no address space)");
        sched.threads[cur].cr3 = Some(new);
        old
    })
}

/// Корень таблиц страниц ядра (общий для всех адресных пространств). Нужен `execve`/`fork`,
/// чтобы строить новое пространство, копируя именно ядровые L4-записи (а не активные —
/// активной может быть таблица пользовательского процесса).
///
/// # Panics
/// Если планировщик не инициализирован.
pub fn kernel_cr3() -> PhysFrame {
    interrupts::without_interrupts(|| {
        SCHEDULER
            .lock()
            .as_ref()
            .expect("scheduler not initialized")
            .kernel_cr3
    })
}

/// PID текущего процесса/потока. 0, если планировщик ещё не инициализирован.
pub fn current_pid() -> u32 {
    interrupts::without_interrupts(|| {
        SCHEDULER
            .lock()
            .as_ref()
            .map_or(0, |s| s.threads[s.current].pid)
    })
}

/// Добавляет поток в планировщик (с выключенными прерываниями — тот же замок берёт таймер).
fn push_thread(thread: Thread) {
    interrupts::without_interrupts(|| {
        let mut guard = SCHEDULER.lock();
        let sched = guard.as_mut().expect("scheduler not initialized");
        sched.threads.push(thread);
    });
}

/// Включает вытеснение по таймеру: с этого момента каждый тик может переключить потоки.
pub fn start_preemption() {
    PREEMPTION_ENABLED.store(true, Ordering::SeqCst);
}

/// Выключает вытеснение по таймеру (потоки остаются, но таймер их больше не переключает).
pub fn stop_preemption() {
    PREEMPTION_ENABLED.store(false, Ordering::SeqCst);
}

/// Завершает **текущий** поток: помечает его `Dead` (планировщик больше его не выберет) и
/// переключается на другой готовый поток. Не возвращается. Сам поток не может освободить свой
/// же стек/адресное пространство (он на них исполняется) — это позже делает [`reap`] на
/// другом потоке. Зовётся из обработчика `exit` (M5c3).
///
/// # Panics
/// Если нет ни одного другого готового потока (так быть не должно — «нулевой» поток ядра
/// всегда `Runnable`).
pub fn exit_current(status: i32) -> ! {
    // Гарантируем IF=0 на всё завершение (switch_to_next переключает без сохранения IF).
    interrupts::disable();
    {
        let mut guard = SCHEDULER.lock();
        let sched = guard.as_mut().expect("scheduler not initialized");
        let cur = sched.current;
        sched.threads[cur].exit_status = Some(status);
        sched.threads[cur].state = State::Dead;
    } // замок отпускаем здесь — switch_to_next возьмёт его снова
    switch_to_next();
    // switch_to_next ушёл в другой готовый поток; в мёртвый поток уже не вернутся.
    unreachable!("exit_current returned to a dead task");
}

/// Освобождает ресурсы завершённых (`Dead`) потоков: их адресное пространство, стек ядра и
/// таблицу дескрипторов. Вызывать из **безопасного контекста** (главный цикл на «нулевом»
/// потоке, в адресном пространстве ядра, IF=1) — не с мёртвого стека, который освобождаем.
///
/// Не уплотняет `Vec`: обработанный поток помечается `Reaped` и остаётся «надгробием» (его
/// структура крошечная) — так не нужно двигать индекс `current`/переключение. Большие
/// ресурсы (фреймы АП, стек, дескрипторы) при этом возвращаются.
///
/// Делать нечего, пока не установлен глобальный аллокатор ([`crate::mm::frame::install`]) —
/// иначе нечем освобождать (и потеряли бы `cr3`); тогда просто выходим.
pub fn reap() {
    if !crate::mm::frame::global_installed() {
        return;
    }
    let phys_offset = crate::mm::paging::phys_mem_offset();

    // 1) Под замком собираем «трупы»: забираем cr3 и стек, помечаем Reaped (не трогаем
    //    текущий поток). Освобождаем ПОСЛЕ снятия замка (порядок: SCHEDULER → FRAME_ALLOC).
    /// Изъятые из завершённого потока ресурсы: его адресное пространство (PML4) и ядровый стек.
    type Corpse = (Option<PhysFrame>, Option<Box<[u8]>>);
    let mut corpses: Vec<Corpse> = Vec::new();
    interrupts::without_interrupts(|| {
        let mut guard = SCHEDULER.lock();
        if let Some(sched) = guard.as_mut() {
            let cur = sched.current;
            for (i, t) in sched.threads.iter_mut().enumerate() {
                if i != cur && t.state == State::Dead {
                    corpses.push((t.cr3.take(), t.stack.take()));
                    t.state = State::Reaped;
                }
            }
        }
    });

    // 2) Освобождаем ресурсы трупов (замок планировщика уже снят).
    for (cr3, stack) in corpses {
        if let Some(frame) = cr3 {
            // SAFETY: труп помечен Dead→Reaped, это не текущий поток и не активное адресное
            // пространство (reaper идёт на «нулевом» потоке в АП ядра), живого маппера на
            // него нет — значит его приватное АП можно разобрать.
            crate::mm::frame::with_global(|fa| unsafe {
                AddressSpace::from_pml4_frame(frame).destroy(phys_offset, fa);
            });
            crate::syscall::files::forget_process(frame.start_address().as_u64());
        }
        drop(stack); // освобождаем Box ядрового стека (в кучу)
    }
}

/// Уступает CPU следующему готовому потоку (round-robin) — **кооперативно**. Переключение
/// с выключенными прерываниями (атомарно), исходный IF восстанавливается по возврате.
pub fn yield_now() {
    let was_enabled = interrupts::are_enabled();
    interrupts::disable();

    switch_to_next();

    if was_enabled {
        interrupts::enable();
    }
}

/// Точка входа вытеснения: её зовёт обработчик таймера на каждом тике. Если вытеснение
/// включено, переключает на следующий готовый поток; иначе — ничего не делает.
///
/// # Safety
/// Вызывать **только из контекста прерывания с IF=0** (как делает обработчик таймера):
/// [`switch_to_next`] переключает контекст/`CR3`/rsp0 без сохранения IF — при IF=1 таймер
/// мог бы реентрантно влезть в середину переключения (UB). IF вернёт `iretq` обработчика.
pub unsafe fn on_timer_tick() {
    if PREEMPTION_ENABLED.load(Ordering::SeqCst) {
        switch_to_next();
    }
}

/// Переключает на следующий **готовый** поток (round-robin, пропуская `Dead`). Возвращает
/// `true`, если переключение произошло, `false` — если переключаться не на кого.
///
/// Вызывать **с выключенными прерываниями**. Замок планировщика берём, считаем параметры
/// перехода и **отпускаем замок до переключения** — держать его через переключение нельзя:
/// мы уйдём в другой поток, `guard.drop()` не выполнится, и замок завис бы навсегда.
fn switch_to_next() -> bool {
    let switch = {
        let mut guard = SCHEDULER.lock();
        match guard.as_mut() {
            Some(sched) => {
                let len = sched.threads.len();
                let cur = sched.current;
                // Первый готовый поток строго после current (по кругу).
                let mut next = None;
                for i in 1..len {
                    let c = (cur + i) % len;
                    if sched.threads[c].state == State::Runnable {
                        next = Some(c);
                        break;
                    }
                }
                match next {
                    Some(n) => {
                        sched.current = n;
                        let old_rsp: *mut u64 = &mut sched.threads[cur].rsp;
                        let new_rsp = sched.threads[n].rsp;
                        let next_cr3 = sched.threads[n].cr3.unwrap_or(sched.kernel_cr3);
                        let next_rsp0 = sched.threads[n].kernel_stack_top;
                        Some((old_rsp, new_rsp, next_cr3, next_rsp0))
                    }
                    None => {
                        // Других готовых нет. Если текущий жив — просто не переключаемся;
                        // если мёртв — переключаться не на кого, это фатально.
                        assert!(
                            sched.threads[cur].state == State::Runnable,
                            "no runnable task to switch to"
                        );
                        None
                    }
                }
            }
            None => None,
        }
    };

    match switch {
        Some((old_rsp, new_rsp, next_cr3, next_rsp0)) => {
            // SAFETY: прерывания выключены; указатели — из валидных контекстов потоков;
            // память ядра отображена в каждом адресном пространстве, поэтому переключение
            // (стеки/таблицы ядра) корректно и через смену CR3. Между отпусканием замка и
            // записью `*old_rsp` (начало switch_context) никакой код Vec не перелокует —
            // IF=0.
            unsafe { context::switch_task(old_rsp, new_rsp, next_cr3, next_rsp0) };
            true
        }
        None => false,
    }
}
