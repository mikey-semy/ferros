# ferros — Roadmap

Each milestone is a complete, observable result ("it now does X") and ends with a
merged PR. We detail a milestone only when we reach it. Current status: **M3 in
progress** (M3a paging + M3b frame allocator/mapping done; M3c heap next; M0–M2 merged).

> This is **depth** (the path forward). For **breadth** — alternatives at each layer
> (firmware, bootloader, runner, architecture, display) and where we could branch out
> — see [LANDSCAPE.md](LANDSCAPE.md).

## Tier A — "It's alive" (bootable kernel)

- **M0 — Environment + first boot.** Toolchain (nightly), QEMU, custom target,
  `no_std`/`no_main`, bootloader, text on screen → QEMU shows `ferros booting...`.
  Git + private GitHub repo + local CI skeleton.
- **M1 — Kernel infrastructure.** VGA buffer abstraction, `print!`/`println!`,
  serial output (for tests), panic handler, QEMU exit, integration-test framework.

## Tier B — Kernel core

- **M2 — Interrupts.** GDT, IDT, CPU exceptions (breakpoint, double fault), TSS,
  hardware interrupts via PIC (8259), timer + keyboard, proper `hlt` loop.
- **M3 — Memory.** Read bootloader memory map, physical frame allocator, paging /
  virtual memory, heap (`linked_list_allocator` + `alloc`) → `Box`, `Vec`, `String`.
- **M4 — Multitasking.** Cooperative `async`/`await` tasks (blog_os style) first,
  then preemptive multitasking with context switching and a scheduler.

## Tier C — Userspace and a basic OS

- **M5 — Userspace + syscalls.** Ring 3, `syscall` instruction, per-process address
  spaces, ELF loading, first user program.
- **M6 — Storage + filesystem.** Disk driver (virtio-blk / AHCI), filesystem
  (start with FAT32 read), VFS layer.
- **M7 — Shell.** init process, interactive shell, basic utilities, line editing.

## Tier D — A "real" OS

- **M8 — Networking.** NIC driver (virtio-net / e1000), TCP/IP via `smoltcp`,
  ping, sockets.
- **M9 — POSIX / libc.** Port a libc (candidate: `relibc`) — enough to build and
  run real programs.
- **M10 — Graphics / GUI (optional, huge).** Framebuffer, compositor, window
  manager, toolkit.
- **M11 — Real hardware.** UEFI boot (migrate off bootloader 0.9), drivers for a
  specific laptop / Raspberry Pi, USB.
- **M12 — Self-hosting (the dream).** A compiler runs inside ferros itself.

## Reality check

Tiers A–C are achievable solo (with AI assistance) given unlimited time. Tier D is
a multi-year commitment and a perpetual chase of moving hardware. Smartphones are a
separate project (a mainline-Linux fork, not an OS from scratch) and are out of scope.
