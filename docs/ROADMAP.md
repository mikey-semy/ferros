# ferros — Roadmap

Each milestone is a complete, observable result ("it now does X") and ends with a
merged PR. We detail a milestone only when we reach it. Current status: **M6 done**
(PCI enumeration; virtio-blk disk over DMA; hand-rolled FAT32 read; fault-tolerant
`uaccess`; VFS + `open`/`read`/`close`/`lseek` — a user process reads a file from disk;
M0–M5 merged). **Now in the pre-M7 "maturity" consolidation:** memory reclamation
(M6e — freeing frame allocator, address-space teardown, process reaping) is **done**,
and the process model (M6f) is underway — PIDs/`getpid`, `execve`, `fork`, and `wait4`
are **done**; basic signals (M6f5) and then filesystem breadth remain before **M7**
(init process + shell).

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
    - **M6d2 — VFS + file syscalls** (done). `open`/`read`/`close`/`lseek` (Linux numbers
      2/0/3/8), per-process fd table (keyed by the process's CR3), copy-to-user, a minimal
      VFS facade over FAT; a ring-3 program opens and reads a file from disk.
- **Maturity phase (pre-M7 consolidation).** Floor-first hardening before the shell:
  - **M6e — Memory reclamation.** The kernel must stop leaking on process exit. Stages:
    **M6e1 — freeing frame allocator** (done; free-list + `deallocate_frame`), **M6e2 —
    address-space teardown**, **M6e3 — process reaping**.
  - **M6f — Process model** (in progress). PIDs + fork/exec/wait + basic signals. Stages:
    **M6f1 — process table (PIDs + `getpid`)** (done), **M6f2 — `execve`** (done; loads from the
    FAT disk), **M6f3 — `fork`** (done; copies the address space + fd table + full register
    context), **M6f4 — `wait4`** (done; zombies + block/wake, on a per-process syscall stack),
    M6f5 signals.
  - Then **filesystem** breadth (write + subdirectories + a real VFS).
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
