//! Вывод в последовательный порт COM1 — наш «провод наружу»: отладка и тесты.
//!
//! # Зачем
//!
//! Serial-порт — простой канал «байт за байтом» к хосту. Даже если VGA сломается,
//! лог уйдёт сюда; а в тестах (M1c) ядро внутри QEMU будет писать результат в
//! serial, а мы — читать его снаружи, без всякого GUI.
//!
//! # Как
//!
//! За «serial-портом» на ПК стоит чип **UART 16550**. Общаемся с ним через
//! **port I/O** — инструкции `in`/`out`, которые читают/пишут отдельное от памяти
//! адресное пространство «портов». Здесь мы разговариваем с чипом сами (без
//! крейта-обёртки), чтобы было видно, как это устроено: регистры UART — это
//! смещения от базового порта COM1 (`0x3F8`).

use core::fmt;
use spin::{LazyLock, Mutex};

/// Базовый порт COM1 на стандартном ПК.
const COM1_BASE: u16 = 0x3F8;

/// Записать байт в порт ввода-вывода x86 (инструкция `out dx, al`).
///
/// # Safety
/// Прямое обращение к железу: вызывающий отвечает за корректность номера порта.
unsafe fn outb(port: u16, value: u8) {
    core::arch::asm!(
        "out dx, al",
        in("dx") port,
        in("al") value,
        options(nomem, nostack, preserves_flags),
    );
}

/// Прочитать байт из порта ввода-вывода x86 (инструкция `in al, dx`).
///
/// # Safety
/// Прямое обращение к железу: вызывающий отвечает за корректность номера порта.
unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    core::arch::asm!(
        "in al, dx",
        out("al") value,
        in("dx") port,
        options(nomem, nostack, preserves_flags),
    );
    value
}

/// Драйвер одного UART 16550, заданного базовым портом ввода-вывода.
struct SerialPort {
    base: u16,
}

impl SerialPort {
    const fn new(base: u16) -> SerialPort {
        SerialPort { base }
    }

    /// Инициализация по стандартной последовательности 16550: 38400 бод, формат
    /// 8N1, включённый FIFO. Регистры — это смещения от базового порта.
    fn init(&mut self) {
        // SAFETY: `base` — корректный базовый порт UART, последовательность
        // обращений к регистрам стандартна для чипа 16550.
        unsafe {
            outb(self.base + 1, 0x00); // выключить прерывания UART
            outb(self.base + 3, 0x80); // DLAB=1: следующие два байта — делитель скорости
            outb(self.base, 0x03); // делитель, младший байт (3 → 38400 бод)
            outb(self.base + 1, 0x00); // делитель, старший байт
            outb(self.base + 3, 0x03); // DLAB=0 + формат 8N1 (8 бит, без чётности, 1 стоп)
            outb(self.base + 2, 0xC7); // включить и очистить FIFO, порог 14 байт
            outb(self.base + 4, 0x0B); // выводы DTR/RTS + OUT2
        }
    }

    /// Регистр статуса линии (LSR, смещение +5). Бит 5 = «буфер передатчика пуст».
    fn is_transmit_empty(&self) -> bool {
        // SAFETY: чтение LSR корректного порта не имеет побочных эффектов.
        unsafe { inb(self.base + 5) & 0x20 != 0 }
    }

    /// Отправить байт: ждём готовности передатчика, затем пишем в регистр данных.
    fn send(&mut self, byte: u8) {
        while !self.is_transmit_empty() {
            core::hint::spin_loop();
        }
        // SAFETY: регистр данных — смещение 0 корректного базового порта.
        unsafe { outb(self.base, byte) };
    }
}

/// Реализация `core::fmt::Write` даёт `write_fmt` и форматирование `{}` бесплатно.
impl fmt::Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            self.send(byte);
        }
        Ok(())
    }
}

/// Глобальный COM1, лениво проинициализированный при первом обращении.
/// `SerialPort::init` делает реальный I/O, поэтому это не `const` — используем
/// `spin::LazyLock`, который выполнит инициализацию при первом `serial_println!`.
static SERIAL1: LazyLock<Mutex<SerialPort>> = LazyLock::new(|| {
    let mut port = SerialPort::new(COM1_BASE);
    port.init();
    Mutex::new(port)
});

/// Рабочая лошадка макросов: берёт замок и пишет форматированный текст в COM1.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    // Тот же приём против дедлока, что и в vga: на время вывода в COM1
    // выключаем прерывания.
    x86_64::instructions::interrupts::without_interrupts(|| {
        SERIAL1.lock().write_fmt(args).expect("serial write failed");
    });
}

/// Печать в serial без перевода строки.
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => ($crate::drivers::serial::_print(format_args!($($arg)*)));
}

/// Печать в serial с переводом строки.
#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($($arg:tt)*) => ($crate::serial_print!("{}\n", format_args!($($arg)*)));
}
