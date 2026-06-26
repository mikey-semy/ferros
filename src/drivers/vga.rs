//! Текстовый VGA-вывод: тип [`Writer`], цвета и макросы `print!` / `println!`.
//!
//! # Как это работает (для тех, кто видит ядро впервые)
//!
//! В текстовом режиме BIOS экран — это просто память по физическому адресу
//! `0xb8000`: сетка 80×25 ячеек, каждая ячейка — 2 байта `[ASCII][атрибут цвета]`.
//! Записал байты — на экране появились буквы.
//!
//! Все записи делаем через [`core::ptr::write_volatile`]: за этой памятью стоит
//! «железо» (видеоадаптер), и компилятор не должен выкидывать или переставлять
//! такие записи в погоне за оптимизацией. Это общее правило для memory-mapped I/O.
//!
//! Чтобы звать `println!` откуда угодно, держим один глобальный [`Writer`] под
//! спин-замком ([`spin::Mutex`]): на голом железе нет ОС, которая «усыпит» поток,
//! поэтому замок просто крутится в цикле, пока не освободится.

use core::fmt;
use spin::Mutex;

/// Высота стандартного текстового экрана VGA (строк).
const BUFFER_HEIGHT: usize = 25;
/// Ширина стандартного текстового экрана VGA (колонок).
const BUFFER_WIDTH: usize = 80;
/// Физический адрес начала текстового VGA-буфера.
const VGA_BUFFER_ADDR: usize = 0xb8000;

/// 16 аппаратных цветов VGA. Числовые значения — это коды, которые понимает
/// видеоадаптер, поэтому менять их нельзя.
#[allow(dead_code)] // пока используем не все цвета — это нормально
#[derive(Clone, Copy)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

/// Байт атрибута ячейки: младшие 4 бита — цвет текста, старшие 4 — цвет фона.
/// `repr(transparent)` — это просто `u8` без накладных расходов.
#[derive(Clone, Copy)]
#[repr(transparent)]
struct ColorCode(u8);

impl ColorCode {
    /// Собирает атрибут из цвета текста (`fg`) и фона (`bg`).
    const fn new(fg: Color, bg: Color) -> ColorCode {
        ColorCode(((bg as u8) << 4) | (fg as u8))
    }
}

/// Одна ячейка экрана: символ и его цвет. `repr(C)` фиксирует порядок полей
/// ровно как ждёт железо — сначала байт символа, потом байт атрибута.
#[derive(Clone, Copy)]
#[repr(C)]
struct ScreenChar {
    ascii: u8,
    color: ColorCode,
}

/// Печатает символы на экран, помня текущую колонку. Ведёт себя как терминал:
/// печатает в самую нижнюю строку, по краю переносит, заполнив экран — прокручивает.
pub struct Writer {
    column: usize,
    color: ColorCode,
}

impl Writer {
    /// Новый writer: белый текст на чёрном фоне, курсор в начале строки.
    /// `const`, чтобы можно было создать глобал без `lazy_static`.
    const fn new() -> Writer {
        Writer {
            column: 0,
            color: ColorCode::new(Color::White, Color::Black),
        }
    }

    /// Указатель на ячейку `(row, col)` внутри VGA-буфера.
    fn cell(row: usize, col: usize) -> *mut ScreenChar {
        (VGA_BUFFER_ADDR as *mut ScreenChar).wrapping_add(row * BUFFER_WIDTH + col)
    }

    /// Печатает один байт. `\n` — перенос строки; иначе кладёт символ в текущую
    /// позицию нижней строки.
    fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            byte => {
                if self.column >= BUFFER_WIDTH {
                    self.new_line();
                }
                let row = BUFFER_HEIGHT - 1; // всегда нижняя строка
                let sc = ScreenChar {
                    ascii: byte,
                    color: self.color,
                };
                // SAFETY: cell() возвращает указатель внутри VGA-буфера; пишем
                // volatile, чтобы запись точно дошла до видеопамяти.
                unsafe { core::ptr::write_volatile(Self::cell(row, self.column), sc) };
                self.column += 1;
            }
        }
    }

    /// Печатает строку. Непечатаемые/не-ASCII байты заменяет на заглушку `■`,
    /// потому что VGA-шрифт знает только однобайтовые коды.
    fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                0x20..=0x7e | b'\n' => self.write_byte(byte),
                _ => self.write_byte(0xfe),
            }
        }
    }

    /// Перенос строки: сдвигаем весь экран на строку вверх, нижнюю очищаем.
    fn new_line(&mut self) {
        for row in 1..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                // SAFETY: оба указателя внутри VGA-буфера.
                unsafe {
                    let c = core::ptr::read_volatile(Self::cell(row, col));
                    core::ptr::write_volatile(Self::cell(row - 1, col), c);
                }
            }
        }
        self.clear_row(BUFFER_HEIGHT - 1);
        self.column = 0;
    }

    /// Заполняет строку пробелами текущего цвета.
    fn clear_row(&mut self, row: usize) {
        let blank = ScreenChar {
            ascii: b' ',
            color: self.color,
        };
        for col in 0..BUFFER_WIDTH {
            // SAFETY: указатель внутри VGA-буфера.
            unsafe { core::ptr::write_volatile(Self::cell(row, col), blank) };
        }
    }
}

/// Реализация `core::fmt::Write` даёт нам бесплатно `write!`/`writeln!` и всё
/// форматирование `{}` — без стандартной библиотеки.
impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

/// Глобальный writer под спин-замком. `Mutex::new` — `const`, поэтому обычный
/// `static` без `lazy_static`.
static WRITER: Mutex<Writer> = Mutex::new(Writer::new());

/// Рабочая лошадка макросов: берёт замок и печатает форматированный текст.
/// Помечена `#[doc(hidden)]`, потому что это деталь реализации макросов.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    // Пока держим замок WRITER, выключаем прерывания: иначе обработчик прерывания
    // мог бы попытаться взять тот же замок и устроить дедлок.
    x86_64::instructions::interrupts::without_interrupts(|| {
        WRITER.lock().write_fmt(args).unwrap();
    });
}

/// Пишет сырые байты на экран (для системного вызова `write` в stdout, M5b). Непечатаемые
/// байты заменяются на заглушку `■`, как и в [`Writer::write_string`]. Замок держим под
/// выключенными прерываниями (то же правило против дедлока, что и в [`_print`]).
pub fn write_bytes(bytes: &[u8]) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut writer = WRITER.lock();
        for &byte in bytes {
            match byte {
                0x20..=0x7e | b'\n' => writer.write_byte(byte),
                _ => writer.write_byte(0xfe),
            }
        }
    });
}

/// Печать без перевода строки: `print!("x = {}", x)`.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::drivers::vga::_print(format_args!($($arg)*)));
}

/// Печать с переводом строки: `println!("привет")`.
#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}
