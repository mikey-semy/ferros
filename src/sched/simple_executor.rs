//! Простейший кооперативный экзекьютор (M4a).
//!
//! Держит FIFO-очередь задач и крутит цикл: достаёт задачу, опрашивает её. `Ready` —
//! выбрасываем; `Pending` — кладём в конец очереди. Используется «пустой» (dummy)
//! waker: он ничего не делает, поэтому экзекьютор просто опрашивает задачи по кругу,
//! пока очередь не опустеет. Это неэффективно (CPU не засыпает) и годится только для
//! знакомства — настоящие waker'ы и сон на `hlt` будут в M4b.

use super::Task;
use alloc::collections::VecDeque;
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

/// Простейший экзекьютор: одна FIFO-очередь задач.
pub struct SimpleExecutor {
    task_queue: VecDeque<Task>,
}

impl SimpleExecutor {
    /// Пустой экзекьютор.
    pub fn new() -> SimpleExecutor {
        SimpleExecutor {
            task_queue: VecDeque::new(),
        }
    }

    /// Ставит задачу в очередь на исполнение.
    pub fn spawn(&mut self, task: Task) {
        self.task_queue.push_back(task);
    }

    /// Гоняет задачи, пока очередь не опустеет (все завершились).
    pub fn run(&mut self) {
        while let Some(mut task) = self.task_queue.pop_front() {
            let waker = dummy_waker();
            let mut context = Context::from_waker(&waker);
            match task.poll(&mut context) {
                Poll::Ready(()) => {}                             // завершилась — забываем
                Poll::Pending => self.task_queue.push_back(task), // ещё не готова — в конец
            }
        }
    }
}

impl Default for SimpleExecutor {
    fn default() -> Self {
        Self::new()
    }
}

/// «Пустой» waker: все операции — no-op. Нужен лишь потому, что `poll` требует
/// `Context` с waker'ом, а наш простой экзекьютор и так опрашивает все задачи в цикле.
fn dummy_raw_waker() -> RawWaker {
    fn no_op(_: *const ()) {}
    fn clone(_: *const ()) -> RawWaker {
        dummy_raw_waker()
    }
    let vtable = &RawWakerVTable::new(clone, no_op, no_op, no_op);
    RawWaker::new(core::ptr::null(), vtable)
}

fn dummy_waker() -> Waker {
    // SAFETY: vtable из `dummy_raw_waker` — корректные no-op функции, а указатель
    // данных (null) ими не разыменовывается, так что контракт `RawWaker` соблюдён.
    unsafe { Waker::from_raw(dummy_raw_waker()) }
}
