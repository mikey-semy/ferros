//! `sched` — кооперативная многозадачность на `async`/`await` (M4).
//!
//! # Идея кооперативных задач
//!
//! «Задача» — исполняемая единица работы, обёрнутая в Rust-future. В отличие от
//! потоков ОС, кооперативные задачи переключаются только **добровольно** — в точках
//! `.await`, где future возвращает `Poll::Pending`. Никаких прерываний и сохранения
//! регистров: всё в обычном коде, поэтому переключение почти бесплатное. «Экзекьютор»
//! (планировщик) по очереди опрашивает (`poll`) задачи.
//!
//! - [`Task`] — обёртка над `Pin<Box<dyn Future>>` (вот где пригодилась куча из M3),
//!   с уникальным [`TaskId`].
//! - [`simple_executor::SimpleExecutor`] (M4a) — учебный busy-poll экзекьютор с
//!   «пустым» waker'ом.
//! - [`executor::Executor`] (M4b) — эффективный: настоящие waker'ы (задача сама кладёт
//!   свой [`TaskId`] в очередь разбуждённых при готовности), опрос только готовых
//!   задач и сон на `hlt`, пока делать нечего.
//! - [`thread`] (M4d/M4e) — потоки ядра с переключением контекста: отдельный стек на
//!   поток, кооперативный [`thread::yield_now`] (M4d) и **вытеснение по таймеру** (M4e),
//!   когда переключение инициирует обработчик таймера, а не сам код.
//!
//! Кооперативные async-задачи (executor) и вытесняемые потоки сосуществуют: executor
//! живёт на «нулевом» потоке, а таймер делит CPU между ним и остальными потоками.

pub mod executor;
pub mod simple_executor;
pub mod thread;

use alloc::boxed::Box;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicU64, Ordering};
use core::task::{Context, Poll};

/// Уникальный идентификатор задачи. Нужен эффективному экзекьютору (M4b), чтобы
/// хранить задачи в таблице по ключу и адресно их будить.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskId(u64);

impl TaskId {
    /// Выдаёт новый уникальный id из глобального атомарного счётчика.
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        TaskId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// Одна кооперативная задача — future без результата, размещённый на куче и
/// «закреплённый» в памяти ([`Pin`]): future после старта нельзя двигать, потому что
/// он может хранить ссылки сам на себя.
pub struct Task {
    id: TaskId,
    future: Pin<Box<dyn Future<Output = ()>>>,
}

impl Task {
    /// Оборачивает любой `'static` future в задачу со свежим [`TaskId`].
    pub fn new(future: impl Future<Output = ()> + 'static) -> Task {
        Task {
            id: TaskId::new(),
            future: Box::pin(future),
        }
    }

    /// Опрашивает задачу один раз. `Ready` — задача завершилась; `Pending` — уступила
    /// управление и ждёт пробуждения через waker из `Context`.
    fn poll(&mut self, context: &mut Context) -> Poll<()> {
        self.future.as_mut().poll(context)
    }
}
