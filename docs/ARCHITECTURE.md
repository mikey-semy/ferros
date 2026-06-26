# ferros — Architecture

This document tracks the **current** design and grows with each milestone.
Status: **M3 done; M4 (multitasking) next.** Code-level conventions live in
[CONVENTIONS.md](CONVENTIONS.md); deferred hardening in [HARDENING.md](HARDENING.md).

## Target & build model

- **ISA:** x86_64, **monolithic** kernel.
- **Custom target:** [`x86_64-ferros.json`](../x86_64-ferros.json) — `os: none`,
  `panic-strategy: abort`, red zone disabled, SIMD off + soft-float (no FPU/SSE
  state to manage in the kernel yet), `rust-lld` linker.
- **`no_std` + `no_main`:** no standard library and no Rust runtime — we run on bare
  metal. The standard `core`/`alloc` are rebuilt for our target via `build-std`
  (hence the `rust-src` component); there are no prebuilt std binaries for this target.
- **Image:** `bootimage` links the compiled kernel with the `bootloader` 0.9 crate
  into a bootable BIOS disk image; `cargo run` boots it in QEMU.

## Boot flow (M0)

```
BIOS  →  bootloader 0.9 (sets up long mode, paging, hands off)
      →  _start  (our entry point, C ABI, no_mangle)
      →  write "ferros booting..." to the VGA text buffer at 0xb8000
      →  hlt loop (idle until an interrupt)
```

- VGA text mode: linear buffer at physical `0xb8000`, 80×25 cells, each cell = 2
  bytes `[ascii][attribute]`. We write bytes directly with `write_volatile` so the
  compiler can't elide or reorder the MMIO writes.

## Source layout

Organized by subsystem, with all CPU/platform code isolated under `arch/` — the
portability seam. Names follow kernel convention; the rules are in
[CONVENTIONS.md](CONVENTIONS.md).

```
src/
├─ main.rs              thin entry point (_start) + panic handler
├─ lib.rs               kernel crate root: init(), test harness, QEMU exit
├─ arch/                CPU/platform-specific code (the only place that is)
│  ├─ mod.rs            cfg-routed arch::init()
│  └─ x86_64/
│     ├─ gdt.rs         GDT/TSS + IST stack (M2b)
│     ├─ interrupts.rs  IDT; breakpoint + double fault; PIC timer & keyboard (M2a/c)
│     └─ context.rs     kernel-thread context switch (M4d)
├─ drivers/             device drivers
│  ├─ vga.rs            VGA text + print!/println! (M1a)
│  ├─ serial.rs         16550 UART on COM1 + serial_println! (M1b)
│  └─ keyboard.rs       async keyboard — scancode stream (M4c)
├─ mm/                  memory management — M3 (paging.rs, frame.rs, heap.rs)
├─ sched/               tasks & threads — M4 (async executor + kernel-thread scheduler)
├─ syscall/             syscalls & userspace — M5
├─ fs/                  filesystems & VFS — M6
├─ net/                 networking — M8
└─ util/                shared no_std helpers
```

`mm` and `sched` are populated; `syscall`/`fs`/`net` are still placeholders (doc-only
`mod.rs`), filled in at their milestones. Integration tests live in `tests/` and link
the kernel crate.

## Known future migrations

- **Bootloader:** move from `bootloader` 0.9 + `bootimage` (BIOS) to `bootloader`
  0.11 / UEFI around **M11**, when targeting real hardware. The kernel code is
  largely independent of this choice.
