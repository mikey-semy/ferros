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
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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

/// Сколько раз процесс блокировался в `read(0)` на пустом вводе (M7a). Наблюдаемость для
/// теста stdin: он ждёт, пока читатель реально заблокируется, и только потом подаёт ввод —
/// так детерминированно проверяется весь путь «блок → побудка».
pub static STDIN_BLOCKS: AtomicU64 = AtomicU64::new(0);

/// Состояние потока в планировщике.
#[derive(PartialEq, Eq, Clone, Copy)]
enum State {
    /// Готов исполняться.
    Runnable,
    /// Заблокирован: ждёт события (M6f4: родитель в `wait`, пока не завершится ребёнок).
    /// Планировщик его пропускает, пока кто-нибудь не переведёт обратно в `Runnable`.
    Blocked,
    /// Завершён (`exit`), статус ещё НЕ собран родителем (зомби, M6f4). Планировщик пропускает.
    /// Reaper освобождает зомби, только если он осиротел (родитель — PID 0); зомби с живым
    /// родителем ждёт, пока тот соберёт его через `wait` (это переведёт его в `Dead`).
    Zombie,
    /// Завершён и статус собран (через `wait` или как осиротевший) — ресурсы ещё не освобождены.
    /// Планировщик пропускает; reaper (M6e3) освободит его память.
    Dead,
    /// Завершён И освобождён reaper'ом (адресное пространство, стек, дескрипторы возвращены).
    /// Остаётся «надгробием» в `Vec` планировщика (структура `Thread` крошечная); уплотнение
    /// `Vec` — позже (HARDENING).
    Reaped,
}

/// Почему поток в состоянии [`State::Blocked`] — чтобы будить именно тех, кого нужно.
/// Осмысленно только при `state == Blocked`; в прочих состояниях не используется.
#[derive(PartialEq, Eq, Clone, Copy)]
enum BlockReason {
    /// Не заблокирован (или причина не важна).
    None,
    /// Ждёт завершения ребёнка в `wait4` (M6f4): будит завершение ребёнка ([`terminate`]).
    Child,
    /// Ждёт ввода в `read(0)` (M7a): будит завершённая строка ([`wake_stdin_readers`]).
    Stdin,
    /// Ждёт на канале (M7g2): читатель ждёт данные/EOF, писатель — место. Будит
    /// [`wake_pipe_waiters`] при записи/закрытии конца.
    Pipe,
}

/// Контекст потока: сохранённый указатель стека плюс владение его памятью; для
/// пользовательских процессов — ещё адресное пространство и стек ядра.
struct Thread {
    /// Идентификатор процесса (M6f1). 0 — «нулевой» поток ядра.
    pid: u32,
    /// PID родителя (кто создал этот поток/процесс). 0 — создан ядром на старте или осиротел
    /// (родитель завершился). Читается `wait` (кого будить) и reaper (осиротевшие зомби).
    parent: u32,
    /// Код завершения (`exit`); `None`, пока поток жив. Собирается родителем через `wait` (M6f4).
    exit_status: Option<i32>,
    /// Каким сигналом завершён (M6f5): `Some(sig)` — убит сигналом (`wait` сообщит `WIFSIGNALED`),
    /// `None` — обычный выход через `exit`. Пишется при завершении вместе с `exit_status`.
    term_signal: Option<u8>,
    /// Набор ожидающих сигналов (битовая маска, бит `sig-1`). `kill` ставит бит; сигналы с
    /// действием по умолчанию «завершить» применяются сразу, остальные пока лишь записываются
    /// (доставка обработчиков — позже, см. HARDENING).
    pending_signals: u64,
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
    /// Причина блокировки (осмысленна при `state == Blocked`): по ней побудчик понимает,
    /// этого ли потока касается событие (завершение ребёнка vs. ввод с консоли).
    blocked_on: BlockReason,
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
                term_signal: None,
                pending_signals: 0,
                rsp: 0,
                stack: None,
                cr3: None,
                kernel_stack_top: 0,
                state: State::Runnable,
                blocked_on: BlockReason::None,
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
        term_signal: None,
        pending_signals: 0,
        rsp,
        stack: Some(stack),
        cr3: None,
        kernel_stack_top: top as u64,
        state: State::Runnable,
        blocked_on: BlockReason::None,
    });
}

/// Регистрирует **пользовательский процесс** как поток: `rsp` — начальный контекст в его
/// ядровом стеке (подготовлен [`context::init_user_thread_stack`]), `cr3` — корень его
/// адресного пространства, `kernel_stack_top` — вершина его ядрового стека (rsp0),
/// `kstack` — память этого стека (держим живой). Создаётся арх-слоем ([`spawn_user`]).
///
/// [`spawn_user`]: crate::arch::x86_64::syscall::spawn_user
pub fn add_user_task(rsp: u64, cr3: PhysFrame, kernel_stack_top: u64, kstack: Box<[u8]>) -> u32 {
    let parent = current_pid();
    let pid = NEXT_PID.fetch_add(1, Ordering::SeqCst);
    push_thread(Thread {
        pid,
        parent,
        exit_status: None,
        term_signal: None,
        pending_signals: 0,
        rsp,
        stack: Some(kstack),
        cr3: Some(cr3),
        kernel_stack_top,
        state: State::Runnable,
        blocked_on: BlockReason::None,
    });
    pid
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
    exit_current_inner(status, None)
}

/// Завершает **текущий** процесс как убитый сигналом `sig` (M6f5): код выхода по конвенции
/// `128+sig`, а `wait` сообщит родителю `WIFSIGNALED`. Зовётся из обработчика сбоя кольца 3
/// (`SIGSEGV`) — заменил прежний «тихий» `exit_current(139)`.
pub fn exit_current_killed(sig: u8) -> ! {
    exit_current_inner(128 + sig as i32, Some(sig))
}

/// Общая часть завершения текущего потока: фиксирует исход (`terminate`) и уходит на другой
/// готовый поток. Не возвращается. Сам поток не освобождает свой стек/АП (он на них) — это
/// позже делает [`reap`].
fn exit_current_inner(status: i32, signal: Option<u8>) -> ! {
    // Гарантируем IF=0 на всё завершение (switch_to_next переключает без сохранения IF).
    interrupts::disable();
    {
        let mut guard = SCHEDULER.lock();
        let sched = guard.as_mut().expect("scheduler not initialized");
        let cur = sched.current;
        terminate(sched, cur, status, signal);
    } // замок отпускаем здесь — switch_to_next возьмёт его снова
    switch_to_next();
    // switch_to_next ушёл в другой готовый поток; в мёртвый поток уже не вернутся.
    unreachable!("exit_current returned to a dead task");
}

/// Помечает поток `idx` завершённым (зомби) с исходом (`status` + `signal`), переусыновляет его
/// детей ядру и будит его родителя, если тот ждёт в `wait`. Общая логика обычного `exit`
/// (M6f4) и убийства сигналом (M6f5). Только меняет состояние планировщика — НЕ переключает
/// контекст (это делает вызывающий, если завершает себя). Вызывать под замком `SCHEDULER`.
fn terminate(sched: &mut Scheduler, idx: usize, status: i32, signal: Option<u8>) {
    let pid = sched.threads[idx].pid;
    sched.threads[idx].exit_status = Some(status);
    sched.threads[idx].term_signal = signal;
    // Зомби, а не сразу Dead: статус ждёт сбора родителем через `wait` (M6f4).
    sched.threads[idx].state = State::Zombie;
    // Переусыновляем детей этого процесса ядру (PID 0): осиротевших зомби соберёт reaper, а
    // живые дети, выйдя, станут зомби с parent 0 и тоже будут собраны (без `wait` от мёртвого
    // родителя они бы зависли навсегда).
    for t in sched.threads.iter_mut() {
        if t.parent == pid {
            t.parent = 0;
        }
    }
    // Будим родителя, если он заблокирован именно в `wait` — пусть соберёт статус. Родителя,
    // заблокированного по другой причине (например, на вводе `read(0)`), не трогаем: он не в
    // `wait`, ребёнок останется зомби до настоящего `wait4`.
    let parent_pid = sched.threads[idx].parent;
    if parent_pid != 0 {
        for t in sched.threads.iter_mut() {
            if t.pid == parent_pid
                && t.state == State::Blocked
                && t.blocked_on == BlockReason::Child
            {
                t.state = State::Runnable;
                break;
            }
        }
    }
}

/// Действие по умолчанию для сигнала `sig` — завершает ли оно процесс (M6f5). Без
/// пользовательских обработчиков это и есть вся реакция. Берём Linux-таблицу: большинство
/// сигналов завершают; игнорируются по умолчанию `SIGCHLD`(17)/`SIGURG`(23)/`SIGWINCH`(28)/
/// `SIGCONT`(18); останавливающие `SIGSTOP`(19)/`SIGTSTP`(20)/`SIGTTIN`(21)/`SIGTTOU`(22) мы
/// тоже не реализуем — трактуем как «не завершать» (стоп пока не поддержан).
fn default_terminates(sig: u8) -> bool {
    !matches!(sig, 17 | 18 | 19 | 20 | 21 | 22 | 23 | 28)
}

/// Результат [`signal`].
pub enum SignalOutcome {
    /// Сигнал доставлен (или записан в ожидающие) целевому процессу.
    Delivered,
    /// Нет такого процесса (жив и с таким PID) — `kill` вернёт `-ESRCH`.
    NoSuchProcess,
}

/// Посылает сигнал `sig` процессу `target_pid` (M6f5). Записывает бит в набор ожидающих; если
/// действие сигнала по умолчанию — завершить, завершает цель немедленно (своих обработчиков у
/// нас пока нет). `sig == 0` — только проверка существования. Если цель — ТЕКУЩИЙ процесс и
/// сигнал фатальный, функция НЕ возвращается (уступает CPU, как `exit`).
pub fn signal(target_pid: u32, sig: u8) -> SignalOutcome {
    // Под замком меняем состояние; возвращаем, надо ли завершить СЕБЯ (тогда переключимся ниже,
    // уже без замка — switch_to_next берёт его сам).
    let self_terminate = interrupts::without_interrupts(|| {
        let mut guard = SCHEDULER.lock();
        let sched = guard.as_mut().expect("scheduler not initialized");
        let cur = sched.current;
        // Цель — только живой ПОЛЬЗОВАТЕЛЬСКИЙ процесс (`cr3.is_some()`): сигналы не должны
        // доставать потоки ядра (у них общий с пользователем диапазон PID, но убивать их из
        // кольца 3 нельзя). «Нулевой» поток и так отсечён проверкой `pid <= 0` в sys_kill.
        let Some(idx) = sched
            .threads
            .iter()
            .position(|t| t.pid == target_pid && t.cr3.is_some() && is_alive(t.state))
        else {
            return Err(SignalOutcome::NoSuchProcess);
        };
        if sig == 0 {
            return Ok(false); // только проверка существования
        }
        sched.threads[idx].pending_signals |= 1u64 << (sig - 1);
        if default_terminates(sig) {
            terminate(sched, idx, 128 + sig as i32, Some(sig));
            Ok(idx == cur) // себя — надо переключиться прочь
        } else {
            Ok(false) // записали в ожидающие, действия по умолчанию нет
        }
    });
    match self_terminate {
        Ok(true) => {
            // Мы только что пометили СЕБЯ зомби — уступаем CPU и не возвращаемся.
            interrupts::disable();
            switch_to_next();
            unreachable!("signal terminated self but returned");
        }
        Ok(false) => SignalOutcome::Delivered,
        Err(outcome) => outcome,
    }
}

/// Состояния потока — «живой подходящий ребёнок» для `wait`: ещё исполняется или ждёт.
fn is_alive(state: State) -> bool {
    matches!(state, State::Runnable | State::Blocked)
}

/// Результат одной попытки [`wait_current`] (под замком планировщика).
enum WaitResult {
    /// Найден завершившийся ребёнок: PID, код выхода и сигнал-убийца (`Some` → завершён сигналом).
    /// Ребёнок помечен `Dead` — reaper освободит.
    Collected(u32, i32, Option<u8>),
    /// Подходящих завершившихся нет, но есть живые — текущий помечен `Blocked`, надо уступить CPU.
    WouldBlock,
    /// Подходящих детей нет вовсе — `wait` вернёт `-ECHILD`.
    NoChildren,
}

/// Одна попытка `wait` ПОД ОДНИМ замком (атомарность скан+`Blocked` исключает потерю
/// пробуждения): ищет зомби-ребёнка → забирает (метит `Dead`); иначе, если есть живые
/// подходящие дети → метит текущий `Blocked`; иначе детей нет. `want_pid > 0` — конкретный PID,
/// иначе любой ребёнок.
fn try_collect_or_block(want_pid: i64) -> WaitResult {
    interrupts::without_interrupts(|| {
        let mut guard = SCHEDULER.lock();
        let sched = guard.as_mut().expect("scheduler not initialized");
        let cur = sched.current;
        let cur_pid = sched.threads[cur].pid;
        let want = if want_pid > 0 {
            Some(want_pid as u32)
        } else {
            None
        };
        let is_mine = |t: &Thread| t.parent == cur_pid && want.is_none_or(|p| t.pid == p);

        // 1) Завершившийся (зомби) подходящий ребёнок → собираем его статус.
        let zombie = sched
            .threads
            .iter()
            .position(|t| is_mine(t) && t.state == State::Zombie);
        if let Some(i) = zombie {
            let child_pid = sched.threads[i].pid;
            let status = sched.threads[i].exit_status.unwrap_or(0);
            let signal = sched.threads[i].term_signal;
            sched.threads[i].state = State::Dead; // статус собран → reaper освободит
            return WaitResult::Collected(child_pid, status, signal);
        }

        // 2) Есть живой подходящий ребёнок → блокируемся до его завершения.
        if sched
            .threads
            .iter()
            .any(|t| is_mine(t) && is_alive(t.state))
        {
            sched.threads[cur].state = State::Blocked;
            sched.threads[cur].blocked_on = BlockReason::Child;
            WaitResult::WouldBlock
        } else {
            WaitResult::NoChildren
        }
    })
}

/// Ждёт завершения ребёнка текущего процесса (M6f4). `want_pid > 0` — конкретный PID, иначе
/// любой. Возвращает `Some((pid, код_выхода, сигнал))` собранного ребёнка (`сигнал` — `Some`,
/// если убит сигналом) или `None`, если детей нет (`-ECHILD`). Блокирует вызывающего (уступая
/// CPU), пока подходящий ребёнок не завершится.
///
/// Блокировка корректна только потому, что `syscall` теперь идёт на СВОЁМ ядровом стеке
/// процесса (M6f4): уступив CPU из середины вызова, мы не затираем чужой кадр на общем стеке.
pub fn wait_current(want_pid: i64) -> Option<(u32, i32, Option<u8>)> {
    loop {
        match try_collect_or_block(want_pid) {
            WaitResult::Collected(pid, status, signal) => return Some((pid, status, signal)),
            WaitResult::NoChildren => return None,
            WaitResult::WouldBlock => {
                // Текущий помечен `Blocked` (атомарно со сканом). Уступаем CPU; вернёмся сюда,
                // когда завершившийся ребёнок переведёт нас обратно в `Runnable`, и пересканим.
                let was_enabled = interrupts::are_enabled();
                interrupts::disable();
                switch_to_next();
                if was_enabled {
                    interrupts::enable();
                }
            }
        }
    }
}

/// Блокирует ТЕКУЩИЙ процесс в ожидании ввода с консоли (`read(0)`, M7a): метит его
/// `Blocked` с причиной [`BlockReason::Stdin`] и уступает CPU. Возвращается, когда
/// [`wake_stdin_readers`] переведёт его обратно в `Runnable` (появилась завершённая строка) —
/// тогда вызывающий перечитывает буфер консоли.
///
/// # Безопасность вызова
/// Звать **с выключенными прерываниями** (как и положено вокруг [`switch_to_next`]), причём
/// атомарно с проверкой «ввод пуст»: иначе строка могла бы прийти между проверкой и
/// блокировкой, и побудка бы потерялась. Корректность блокирующего syscall'а — на собственном
/// ядровом стеке процесса (как у `wait`, M6f4): уступив из середины вызова, мы не затрём чужой
/// кадр.
pub fn block_current_on_stdin() {
    {
        let mut guard = SCHEDULER.lock();
        let sched = guard.as_mut().expect("scheduler not initialized");
        let cur = sched.current;
        sched.threads[cur].state = State::Blocked;
        sched.threads[cur].blocked_on = BlockReason::Stdin;
    }
    STDIN_BLOCKS.fetch_add(1, Ordering::SeqCst);
    // Уступаем CPU; вернёмся сюда, когда побудка переведёт нас в `Runnable`.
    switch_to_next();
}

/// Будит все процессы, заблокированные в `read(0)` (M7a): переводит их из `Blocked`/`Stdin`
/// обратно в `Runnable`. Зовётся консолью при завершении строки (Enter). Разбуженные сами
/// перечитают буфер; кому ввода не досталось — заблокируются снова (ложная побудка безопасна).
pub fn wake_stdin_readers() {
    interrupts::without_interrupts(|| {
        let mut guard = SCHEDULER.lock();
        if let Some(sched) = guard.as_mut() {
            for t in sched.threads.iter_mut() {
                if t.state == State::Blocked && t.blocked_on == BlockReason::Stdin {
                    t.state = State::Runnable;
                }
            }
        }
    });
}

/// Блокирует ТЕКУЩИЙ процесс на канале (M7g2): метит `Blocked`/[`BlockReason::Pipe`] и уступает
/// CPU. Возвращается, когда [`wake_pipe_waiters`] переведёт его в `Runnable` (другой конец что-то
/// записал или закрылся) — вызывающий тогда перечитывает буфер канала.
///
/// # Безопасность вызова
/// Звать с выключенными прерываниями, атомарно с проверкой состояния канала и БЕЗ удержания замка
/// буфера канала (иначе writer не сможет его взять) — ровно как [`block_current_on_stdin`].
pub fn block_current_on_pipe() {
    {
        let mut guard = SCHEDULER.lock();
        let sched = guard.as_mut().expect("scheduler not initialized");
        let cur = sched.current;
        sched.threads[cur].state = State::Blocked;
        sched.threads[cur].blocked_on = BlockReason::Pipe;
    }
    switch_to_next();
}

/// Будит все процессы, заблокированные на канале (M7g2). Зовётся при записи в канал, закрытии его
/// конца (`Drop`) или исчерпании читателей. Разбуженные перечитают свой канал; кому делать нечего —
/// заблокируются снова (ложная побудка безопасна).
pub fn wake_pipe_waiters() {
    interrupts::without_interrupts(|| {
        let mut guard = SCHEDULER.lock();
        if let Some(sched) = guard.as_mut() {
            for t in sched.threads.iter_mut() {
                if t.state == State::Blocked && t.blocked_on == BlockReason::Pipe {
                    t.state = State::Runnable;
                }
            }
        }
    });
}

/// Освобождает ресурсы завершённых потоков: их адресное пространство, стек ядра и таблицу
/// дескрипторов. Это собранные родителем (`Dead`) и осиротевшие зомби (`Zombie` с родителем
/// PID 0 — их статус уже некому собрать через `wait`). Вызывать из **безопасного контекста**
/// (главный цикл на «нулевом» потоке, в адресном пространстве ядра, IF=1) — не с мёртвого
/// стека, который освобождаем.
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
                // Освобождаем собранные (`Dead`) и осиротевшие зомби (родитель — PID 0: их
                // статус уже некому собирать через `wait`). Зомби с живым родителем не трогаем —
                // ждём, пока тот соберёт его (`wait` переведёт в `Dead`).
                let collectable =
                    t.state == State::Dead || (t.state == State::Zombie && t.parent == 0);
                if i != cur && collectable {
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
