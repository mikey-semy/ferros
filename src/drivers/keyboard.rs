//! Async-клавиатура (M4c): ввод как поток событий, обрабатываемый задачей.
//!
//! # Зачем переносить декодирование из прерывания
//!
//! В M2c обработчик прерывания клавиатуры сам декодировал скан-код и печатал символ.
//! Это плохо: обработчик прерывания должен быть **коротким** (он выполняется с
//! выключенными прерываниями и не может ждать/аллоцировать/блокироваться). Декодер же
//! может разрастись (раскладки, Unicode, мёртвые клавиши).
//!
//! Решение: обработчик прерывания только **складывает байт в очередь и будит задачу**
//! (быстро, без аллокаций), а тяжёлую работу делает async-задача [`print_keypresses`],
//! которая спит, пока ввода нет. Так waker'ы из M4b впервые работают «по-настоящему»:
//! пробуждение приходит из обработчика прерывания.
//!
//! ```text
//!   IRQ(keyboard) -> add_scancode(byte) -> queue.push + WAKER.wake()
//!                                                          |
//!   executor:  poll(print_keypresses) <--- разбудили <----+
//!              -> ScancodeStream pops byte -> декод -> печать
//! ```

use crate::{print, println};
use core::pin::Pin;
use core::task::{Context, Poll};
use crossbeam_queue::ArrayQueue;
use futures_util::stream::{Stream, StreamExt};
use futures_util::task::AtomicWaker;
use pc_keyboard::{layouts::Us104Key, DecodedKey, HandleControl, PS2Keyboard, ScancodeSet1};
use spin::Once;

/// Ёмкость очереди скан-кодов (на всплески ввода до того, как задача разгребёт).
const QUEUE_SIZE: usize = 128;

/// Очередь скан-кодов: обработчик прерывания пишет, задача читает. Инициализируется
/// в [`init`] на этапе загрузки — **не из прерывания** (там нельзя аллоцировать).
static SCANCODE_QUEUE: Once<ArrayQueue<u8>> = Once::new();

/// Waker задачи-потребителя. Обработчик прерывания дёргает его, чтобы её разбудить.
static WAKER: AtomicWaker = AtomicWaker::new();

/// Инициализирует очередь скан-кодов. Вызывать один раз при загрузке — после кучи
/// (очередь аллоцирует буфер) и до того, как понадобится ввод.
pub fn init() {
    SCANCODE_QUEUE.call_once(|| ArrayQueue::new(QUEUE_SIZE));
}

/// Кладёт скан-код в очередь и будит задачу-потребителя.
///
/// **Вызывается из обработчика прерывания**, поэтому ничего не аллоцирует и не
/// блокируется. При переполнении очереди **роняет байт** (печатает предупреждение)
/// вместо паники — потерять символ при шторме ввода лучше, чем уронить ядро.
pub fn add_scancode(scancode: u8) {
    match SCANCODE_QUEUE.get() {
        Some(queue) => {
            if queue.push(scancode).is_err() {
                println!("WARNING: scancode queue full; dropping input");
            } else {
                WAKER.wake();
            }
        }
        None => println!("WARNING: scancode queue uninitialized; dropping input"),
    }
}

/// Поток скан-кодов — async-источник, отдающий байты из [`SCANCODE_QUEUE`].
/// Приватное поле не даёт создать его в обход [`new`](ScancodeStream::new).
pub struct ScancodeStream {
    _private: (),
}

impl ScancodeStream {
    /// Создаёт поток. Очередь должна быть уже инициализирована ([`init`]).
    pub fn new() -> Self {
        ScancodeStream { _private: () }
    }
}

impl Default for ScancodeStream {
    fn default() -> Self {
        Self::new()
    }
}

impl Stream for ScancodeStream {
    type Item = u8;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<u8>> {
        let queue = SCANCODE_QUEUE
            .get()
            .expect("scancode queue not initialized");

        // Быстрый путь: байт уже лежит в очереди — отдаём без регистрации waker'а.
        if let Some(scancode) = queue.pop() {
            return Poll::Ready(Some(scancode));
        }

        // Очередь пуста — регистрируем waker и проверяем ещё раз: байт мог прийти
        // между `pop` и `register`, и тогда пробуждение бы потерялось.
        WAKER.register(cx.waker());
        match queue.pop() {
            Some(scancode) => {
                WAKER.take(); // байт всё-таки был — waker больше не нужен
                Poll::Ready(Some(scancode))
            }
            None => Poll::Pending,
        }
    }
}

/// Задача: читает скан-коды из потока, декодирует (раскладка US, scancode set 1) и
/// печатает символы. Крутится в экзекьюторе и спит, пока ввода нет.
pub async fn print_keypresses() {
    let mut scancodes = ScancodeStream::new();
    let mut keyboard = PS2Keyboard::new(ScancodeSet1::new(), Us104Key, HandleControl::Ignore);

    while let Some(scancode) = scancodes.next().await {
        if let Ok(Some(event)) = keyboard.add_byte(scancode) {
            if let Some(key) = keyboard.process_keyevent(event) {
                match key {
                    DecodedKey::Unicode(c) => print!("{c}"),
                    DecodedKey::RawKey(k) => print!("{k:?}"),
                }
            }
        }
    }
}
