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
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use x86_64::instructions::interrupts;
use x86_64::structures::paging::PhysFrame;

/// Размер стека одного потока ядра (16 КиБ).
const STACK_SIZE: usize = 4096 * 4;

/// Включено ли вытеснение по таймеру. Пока `false`, обработчик таймера только считает
/// тики и не трогает потоки (поведение M2–M4d). Взводится [`start_preemption`].
static PREEMPTION_ENABLED: AtomicBool = AtomicBool::new(false);

/// Состояние потока в планировщике.
#[derive(PartialEq, Eq, Clone, Copy)]
enum State {
    /// Готов исполняться.
    Runnable,
    /// Завершён (`exit`); планировщик его пропускает. Память пока не освобождаем (зомби) —
    /// реапинг в M5c3b (см. `docs/HARDENING.md`).
    Dead,
}

/// Контекст потока: сохранённый указатель стека плюс владение его памятью; для
/// пользовательских процессов — ещё адресное пространство и стек ядра.
struct Thread {
    /// Сохранённый `rsp` потока (в его ядровом стеке). Обновляется при переключении прочь.
    rsp: u64,
    /// Память стека (ядрового). `None` у «нулевого» потока — он на стеке ядра от загрузчика.
    /// Поле держит память живой; напрямую не читается.
    _stack: Option<Box<[u8]>>,
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
                rsp: 0,
                _stack: None,
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

    push_thread(Thread {
        rsp,
        _stack: Some(stack),
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
    push_thread(Thread {
        rsp,
        _stack: Some(kstack),
        cr3: Some(cr3),
        kernel_stack_top,
        state: State::Runnable,
    });
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
/// переключается на другой готовый поток. Не возвращается. Память завершённого потока пока
/// не освобождаем (зомби) — реапинг в M5c3b. Зовётся из обработчика `exit` (M5c3).
///
/// # Panics
/// Если нет ни одного другого готового потока (так быть не должно — «нулевой» поток ядра
/// всегда `Runnable`).
pub fn exit_current() -> ! {
    interrupts::disable();
    interrupts::without_interrupts(|| {
        let mut guard = SCHEDULER.lock();
        let sched = guard.as_mut().expect("scheduler not initialized");
        let cur = sched.current;
        sched.threads[cur].state = State::Dead;
    });
    switch_to_next();
    // switch_to_next ушёл в другой готовый поток; в мёртвый поток уже не вернутся.
    unreachable!("exit_current returned to a dead task");
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
