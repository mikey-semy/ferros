//! Кооперативные потоки ядра с переключением контекста (M4d).
//!
//! # Чем поток отличается от async-задачи
//!
//! Async-[`Task`](super::Task) — это future: он добровольно уступает в `.await`, и у
//! него нет собственного стека (состояние компилятор зашивает в стейт-машину).
//! **Поток** же — это отдельный стек + сохранённый набор регистров; он может уступить
//! в любой точке кода, потому что мы умеем сохранить/восстановить его регистры
//! ([`crate::arch::context`]). Это фундамент для **вытесняющей** многозадачности (M4e):
//! там переключение будет дёргать таймер, а не сам код.
//!
//! Здесь — **кооперативная** версия: поток уступает CPU явным вызовом [`yield_now`].
//! Планировщик простой round-robin. «Нулевой» поток — текущий контекст ядра (его стек
//! завёл загрузчик); остальные получают свежий стек в куче.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::instructions::interrupts;

/// Размер стека одного потока ядра (16 КиБ).
const STACK_SIZE: usize = 4096 * 4;

/// Контекст потока: сохранённый указатель стека плюс владение его памятью.
struct Thread {
    /// Сохранённый `rsp` потока. Пока поток исполняется, значение неактуально;
    /// обновляется в момент переключения прочь.
    rsp: u64,
    /// Память стека. `None` у «нулевого» потока — он работает на стеке ядра от
    /// загрузчика. Поле держит память живой; напрямую не читается.
    _stack: Option<Box<[u8]>>,
}

/// Простой round-robin планировщик потоков ядра.
struct Scheduler {
    threads: Vec<Thread>,
    current: usize,
}

static SCHEDULER: Mutex<Option<Scheduler>> = Mutex::new(None);

/// Инициализирует планировщик «нулевым» потоком — текущим контекстом ядра. Его `rsp`
/// заполнится при первом переключении прочь. Вызывать один раз, после инициализации
/// кучи (потоки выделяют стеки в куче).
pub fn init() {
    let mut guard = SCHEDULER.lock();
    *guard = Some(Scheduler {
        threads: vec![Thread {
            rsp: 0,
            _stack: None,
        }],
        current: 0,
    });
}

/// Создаёт поток с точкой входа `entry` (она не должна возвращаться — её тип `-> !`).
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
    let rsp = unsafe { crate::arch::context::init_thread_stack(top, entry) };

    let mut guard = SCHEDULER.lock();
    let sched = guard.as_mut().expect("scheduler not initialized");
    sched.threads.push(Thread {
        rsp,
        _stack: Some(stack),
    });
}

/// Уступает CPU следующему потоку (round-robin). Если поток всего один — ничего не
/// делает. Переключение выполняется с выключенными прерываниями (атомарно).
pub fn yield_now() {
    let was_enabled = interrupts::are_enabled();
    interrupts::disable();

    // Считаем указатели на контексты под замком, затем ОТПУСКАЕМ замок — держать его
    // через переключение нельзя: мы уйдём в другой поток, и `guard.drop()` не выполнится,
    // оставив замок захваченным навсегда.
    let switch = {
        let mut guard = SCHEDULER.lock();
        match guard.as_mut() {
            Some(sched) if sched.threads.len() >= 2 => {
                let cur = sched.current;
                let next = (cur + 1) % sched.threads.len();
                sched.current = next;
                let old_rsp: *mut u64 = &mut sched.threads[cur].rsp;
                let new_rsp: u64 = sched.threads[next].rsp;
                Some((old_rsp, new_rsp))
            }
            _ => None,
        }
    };

    if let Some((old_rsp, new_rsp)) = switch {
        // SAFETY: указатели — из валидных контекстов потоков. `*old_rsp` пишется в
        // самом начале switch_context (до прыжка), поэтому его не задевает возможная
        // перелокация Vec в другом потоке. Прерывания выключены.
        unsafe { crate::arch::context::switch_context(old_rsp, new_rsp) };
    }

    if was_enabled {
        interrupts::enable();
    }
}
