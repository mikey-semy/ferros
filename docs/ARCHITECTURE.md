# ferros — Architecture

This document tracks the **current** design and grows with each milestone.
Status reflects **M0**.

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

## Planned structure (next milestones)

- **M1:** `vga_buffer` (a `Writer` + `print!`/`println!`), `serial` (COM1, for test
  output), panic handler that prints, QEMU exit device for tests.
- **M2:** `gdt`, `interrupts` (IDT, exception + hardware handlers), PIC.
- **M3:** `memory` (frame allocator, paging), `allocator` (kernel heap).
- **M4+:** `task`/scheduler, then userspace, drivers, filesystem, networking.

## Known future migrations

- **Bootloader:** move from `bootloader` 0.9 + `bootimage` (BIOS) to `bootloader`
  0.11 / UEFI around **M11**, when targeting real hardware. The kernel code is
  largely independent of this choice.
