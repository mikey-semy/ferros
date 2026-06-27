//! GDT (Global Descriptor Table) + TSS: сегменты ядра и пользователя, аварийный стек
//! для double fault и стек ядра для входа из кольца 3 (rsp0).
//!
//! # Зачем (double fault)
//!
//! Если стек ядра переполнится, попытка процессора сохранить кадр исключения на
//! сломанном стеке вызовет ещё одно исключение → double fault → (если и он на
//! сломанном стеке) → triple fault → перезагрузка машины. Поэтому обработчик double
//! fault работает на **другом, заведомо хорошем стеке** из таблицы IST в **TSS**.
//!
//! # Зачем (кольца и SYSCALL) — M5
//!
//! Пользовательский код работает в **кольце 3** (CPL=3), ядро — в кольце 0. Для каждого
//! уровня нужен свой сегмент кода/данных в GDT. Инструкции `syscall`/`sysret` берут
//! селекторы колец из MSR **STAR**, и аппаратно требуют строгий порядок четырёх
//! сегментов: ядро `SS = CS + 8`, пользователь `SS = sysret_base + 8`, `CS = base + 16`.
//! Поэтому раскладка фиксирована: `kernel_code, kernel_data, user_data, user_code`.
//!
//! # rsp0 — на каждый процесс свой (M5c3)
//!
//! Когда из кольца 3 прилетает прерывание/исключение, процессор переключается на стек
//! ядра из `TSS.privilege_stack_table[0]` (rsp0). При вытесняющей многозадачности у
//! каждого пользовательского потока — свой стек ядра, поэтому rsp0 **меняется на каждом
//! переключении** ([`set_kernel_stack`]). Значит TSS должен быть изменяемым в рантайме —
//! держим его в [`UnsafeCell`] (одно ядро, пишем только с выключенными прерываниями).

use core::cell::UnsafeCell;
use spin::LazyLock;
use x86_64::instructions::segmentation::{Segment, CS, SS};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

/// Индекс IST-стека, который мы выделяем под обработчик double fault.
pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

/// Размер статического стека ядра (5 страниц по 4 КиБ).
const KSTACK_SIZE: usize = 4096 * 5;

/// Аварийный стек для double fault (IST).
static mut DF_STACK: [u8; KSTACK_SIZE] = [0; KSTACK_SIZE];
/// Стек ядра по умолчанию для входа из кольца 3 (rsp0) — пока не запущен пользовательский
/// поток со своим стеком. Процессору отдаём верхнюю границу.
static mut PRIV_STACK: [u8; KSTACK_SIZE] = [0; KSTACK_SIZE];

/// Верхняя граница (старший адрес) статического стека `stack`.
fn stack_top(stack: *const [u8; KSTACK_SIZE]) -> VirtAddr {
    // Берём адрес через `&raw const`, чтобы не создавать ссылку на `static mut`.
    // Стек растёт вниз → отдаём верхнюю границу.
    VirtAddr::from_ptr(stack) + KSTACK_SIZE as u64
}

/// Обёртка над TSS с внутренней изменяемостью: rsp0 надо менять в рантайме на каждом
/// переключении потоков (M5c3), а `LazyLock`/обычный `static` это не дают.
struct MutableTss(UnsafeCell<TaskStateSegment>);
// SAFETY: одно ядро; к TSS обращаемся либо при инициализации, либо из переключения
// контекста с выключенными прерываниями — конкурентного доступа нет.
unsafe impl Sync for MutableTss {}

/// Глобальный TSS. `TaskStateSegment::new()` — `const`, поэтому статик; поля (IST, rsp0)
/// заполняем в [`init`] (адреса стеков — рантайм-значения).
static TSS: MutableTss = MutableTss(UnsafeCell::new(TaskStateSegment::new()));

/// Меняет rsp0 (стек ядра для входа из кольца 3) — вызывается при переключении на
/// пользовательский поток, чтобы прерывание из кольца 3 село на стек ИМЕННО этого потока.
///
/// # Safety
/// Вызывать только с выключенными прерываниями (как делает переключение контекста): иначе
/// прерывание могло бы прочитать rsp0 в момент записи.
pub unsafe fn set_kernel_stack(rsp0: VirtAddr) {
    // SAFETY: одно ядро, прерывания выключены — эксклюзивный доступ к полю TSS.
    unsafe { (*TSS.0.get()).privilege_stack_table[0] = rsp0 };
}

/// Селекторы сегментов, нужные после загрузки GDT и для настройки `syscall` (M5).
pub struct Selectors {
    /// Сегмент кода ядра (кольцо 0).
    pub kernel_code: SegmentSelector,
    /// Сегмент данных ядра (кольцо 0); используется как SS ядра.
    pub kernel_data: SegmentSelector,
    /// Сегмент данных пользователя (кольцо 3); используется как SS пользователя.
    pub user_data: SegmentSelector,
    /// Сегмент кода пользователя (кольцо 3).
    pub user_code: SegmentSelector,
    /// Селектор TSS.
    pub tss: SegmentSelector,
}

/// GDT + селекторы. Порядок сегментов фиксирован требованием `syscall`/`sysret`:
/// `kernel_code, kernel_data, user_data, user_code`, затем TSS.
static GDT: LazyLock<(GlobalDescriptorTable, Selectors)> = LazyLock::new(|| {
    let mut gdt = GlobalDescriptorTable::new();
    let kernel_code = gdt.append(Descriptor::kernel_code_segment());
    let kernel_data = gdt.append(Descriptor::kernel_data_segment());
    let user_data = gdt.append(Descriptor::user_data_segment());
    let user_code = gdt.append(Descriptor::user_code_segment());
    // SAFETY: дескриптору TSS нужен только адрес TSS (база+лимит), а не его содержимое;
    // живой ссылки мы не держим — она нужна лишь на момент построения дескриптора.
    let tss = gdt.append(Descriptor::tss_segment(unsafe { &*TSS.0.get() }));
    (
        gdt,
        Selectors {
            kernel_code,
            kernel_data,
            user_data,
            user_code,
            tss,
        },
    )
});

/// Селекторы сегментов GDT (для настройки `syscall` и перехода в кольцо 3).
pub fn selectors() -> &'static Selectors {
    &GDT.1
}

/// Загружает GDT, перезагружает `CS`/`SS` на сегменты ядра и активирует TSS (`ltr`).
/// Вызывать до загрузки IDT (та ссылается на IST-индекс из TSS).
pub fn init() {
    // Заполняем TSS до его активации: IST-стек для double fault и стек rsp0 по умолчанию.
    // SAFETY: одно ядро, прерывания ещё выключены (до `sti` в arch::init); эксклюзивно.
    unsafe {
        let tss = &mut *TSS.0.get();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = stack_top(&raw const DF_STACK);
        tss.privilege_stack_table[0] = stack_top(&raw const PRIV_STACK);
    }

    GDT.0.load();
    // SAFETY: селекторы получены из только что загруженной GDT и валидны. `SS`
    // обязательно переустановить: после смены раскладки GDT старый селектор стека от
    // загрузчика указывал бы на другой дескриптор.
    unsafe {
        CS::set_reg(GDT.1.kernel_code);
        SS::set_reg(GDT.1.kernel_data);
        load_tss(GDT.1.tss);
    }
}
