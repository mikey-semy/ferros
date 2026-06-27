# ferros — Roadmap

Each milestone is a complete, observable result ("it now does X") and ends with a
merged PR. We detail a milestone only when we reach it. Current status: **M5 done**
(ring 3; Linux-shaped `write`/`exit`; real ELF loading; per-process address spaces;
processes as scheduled tasks with preemption; multiple processes + a ring-3 fault kills
the process, not the kernel; M0–M4 merged). **Next: M6** (storage + filesystem) —
process reaping / polish still pending (needs a freeing frame allocator).

> **North star (D8):** run the existing **Linux** software ecosystem rather than write a
> native app ecosystem from scratch. Long-term aim is **ABI-level** compatibility
> (unmodified Linux binaries), reached via a **POSIX syscall layer + libc first** (M5,
> M9). Windows only via **Wine-class** layers on the Linux ABI; Wine itself is a far
> aspiration, not a milestone. This choice **bites at M5 (next)** — so the syscall layer
> there should be designed Linux-shaped from day one. See [DECISIONS.md](DECISIONS.md) D8.

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
- **M6 — Storage + filesystem.** Disk driver (**virtio-blk**, chosen over ATA/AHCI),
  filesystem (**hand-rolled FAT read**), VFS layer. Decomposed:
  - **M6a — Minimal PCI bus enumeration** (done). virtio is a PCI device, so the PCI
    config-space scan (ports 0xCF8/0xCFC) is the prerequisite foundation: find the
    virtio-blk device, decode its BARs.
  - **M6b — Legacy virtio-blk driver** (done). Bus-master + one virtqueue (DMA), polled
    `read_sector`: read a sector off the virtual disk and verify its content.
  - **M6c — Hand-rolled FAT32 read** (done). BPB → FAT → root dir → cluster chain → file
    bytes; reads a file off the disk. Test image is FAT32 (image grown to 64 MiB), created by
    `fatfs` as a build-only dependency; the kernel reader is hand-rolled.
  - **M6d — VFS + file syscalls** (split):
    - **M6d1 — Fault-tolerant `uaccess`** (done). `with_user_bytes` pre-validates the user
      range against the page tables → `-EFAULT` instead of a kernel panic on a bad pointer
      (the consolidation trigger before user buffers proliferate in `read`).
    - **M6d2 — VFS + file syscalls:** `open`/`read`/`close`/`lseek`, per-process fd table,
      copy-to-user; a user program reads a file from disk.
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
