//! Эффективный кооперативный экзекьютор с настоящими waker'ами (M4b).
//!
//! # Чем лучше простого
//!
//! [`super::simple_executor::SimpleExecutor`] крутил busy-loop: опрашивал ВСЕ задачи
//! по кругу, даже не готовые, и не давал CPU спать. Здесь иначе:
//!
//! - задачи лежат в таблице `tasks` по их [`TaskId`];
//! - есть общая очередь `task_queue` с id **разбуждённых** задач — тех, кого имеет
//!   смысл опросить;
//! - у каждой задачи свой [`Waker`]. Когда future задачи готов продвинуться (например,
//!   пришёл байт от клавиатуры), он зовёт `waker.wake()`, и тот кладёт `TaskId` задачи
//!   в `task_queue`. Экзекьютор опрашивает **только** задачи из этой очереди;
//! - когда очередь пуста — экзекьютор **спит на `hlt`** до следующего прерывания.
//!   Вот она, забота о быстродействии: CPU не жжётся впустую.
//!
//! Очередь — лок-фри [`ArrayQueue`] под [`Arc`], потому что её разделяют экзекьютор и
//! waker'ы (а waker могут дёрнуть из обработчика прерывания).

use super::{Task, TaskId};
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::task::Wake;
use core::task::{Context, Poll, Waker};
use crossbeam_queue::ArrayQueue;

/// Эффективный экзекьютор: таблица задач + очередь разбуждённых + кэш waker'ов.
pub struct Executor {
    /// Все живые задачи по их id.
    tasks: BTreeMap<TaskId, Task>,
    /// id «разбуждённых» задач, готовых к опросу. `Arc`, потому что очередь
    /// разделяют экзекьютор и waker'ы задач.
    task_queue: Arc<ArrayQueue<TaskId>>,
    /// Кэш waker'ов по id — чтобы не пересоздавать waker на каждый `poll`.
    waker_cache: BTreeMap<TaskId, Waker>,
}

impl Executor {
    /// Пустой экзекьютор. Очередь разбуждённых — фиксированного размера (лок-фри).
    pub fn new() -> Self {
        Executor {
            tasks: BTreeMap::new(),
            task_queue: Arc::new(ArrayQueue::new(100)),
            waker_cache: BTreeMap::new(),
        }
    }

    /// Добавляет задачу и сразу ставит её id в очередь готовых (для первого опроса).
    pub fn spawn(&mut self, task: Task) {
        let task_id = task.id;
        if self.tasks.insert(task_id, task).is_some() {
            panic!("task with the same ID is already spawned");
        }
        self.task_queue.push(task_id).expect("task_queue is full");
    }

    /// Один проход: опрашивает все задачи из очереди готовых, пока та не опустеет.
    /// `Ready` — удаляем задачу и её waker; `Pending` — оставляем (разбудит свой
    /// waker). Не блокирует — это «насос», который зовёт [`run`](Self::run).
    pub fn run_ready_tasks(&mut self) {
        // Разбиваем `self` на поля, чтобы заёмщик пропустил одновременный доступ
        // к разным полям (иначе borrow checker ругался бы).
        let Self {
            tasks,
            task_queue,
            waker_cache,
        } = self;

        while let Some(task_id) = task_queue.pop() {
            let task = match tasks.get_mut(&task_id) {
                Some(task) => task,
                None => continue, // задача уже завершилась — пропускаем
            };
            let waker = waker_cache
                .entry(task_id)
                .or_insert_with(|| TaskWaker::new_waker(task_id, task_queue.clone()));
            let mut context = Context::from_waker(waker);
            match task.poll(&mut context) {
                Poll::Ready(()) => {
                    // Задача завершилась — убираем её и кэшированный waker.
                    tasks.remove(&task_id);
                    waker_cache.remove(&task_id);
                }
                Poll::Pending => {}
            }
        }
    }

    /// Если готовых задач нет — засыпаем на `hlt` до прерывания, чтобы не жечь CPU.
    ///
    /// «Пусто?» и `hlt` делаем с выключенными прерываниями, иначе между проверкой и
    /// `hlt` могло бы прийти пробуждение, заполнить очередь, а мы бы всё равно уснули
    /// и проспали его. [`enable_and_hlt`] атомарно делает `sti; hlt`: прерывание после
    /// `sti` сработает уже после `hlt`, пробуждение не теряется.
    ///
    /// [`enable_and_hlt`]: x86_64::instructions::interrupts::enable_and_hlt
    fn sleep_if_idle(&self) {
        use x86_64::instructions::interrupts::{self, enable_and_hlt};

        interrupts::disable();
        if self.task_queue.is_empty() {
            enable_and_hlt();
        } else {
            interrupts::enable();
        }
    }

    /// Главный цикл ядра после старта: гонять готовые задачи, затем спать, если делать
    /// нечего. Не возвращается.
    pub fn run(&mut self) -> ! {
        loop {
            self.run_ready_tasks();
            self.sleep_if_idle();
        }
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

/// Waker конкретной задачи: разбудить = положить её [`TaskId`] в общую очередь
/// готовых, откуда экзекьютор её заберёт на следующем проходе.
struct TaskWaker {
    task_id: TaskId,
    task_queue: Arc<ArrayQueue<TaskId>>,
}

impl TaskWaker {
    /// Собирает [`Waker`] из нового `TaskWaker` через `Arc` и [`alloc::task::Wake`].
    /// Имя не `new` намеренно (`clippy::new_ret_no_self`): возвращаем `Waker`, не `Self`.
    fn new_waker(task_id: TaskId, task_queue: Arc<ArrayQueue<TaskId>>) -> Waker {
        Waker::from(Arc::new(TaskWaker {
            task_id,
            task_queue,
        }))
    }

    fn wake_task(&self) {
        self.task_queue
            .push(self.task_id)
            .expect("task_queue is full");
    }
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_task();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.wake_task();
    }
}
