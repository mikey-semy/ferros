# ferros — Roadmap

Each milestone is a complete, observable result ("it now does X") and ends with a
merged PR. We detail a milestone only when we reach it. Current status: **M7 done; M9
started** (M0–M7 merged). ferros **boots into an interactive shell** with redirection
(`<`/`>`/`>>`), pipes (`|`), and file management (`echo`/`cat`/`ls`/`mkdir`/`rm`/`rmdir`
in `/bin`): keyboard input via `read(0)`, programs launched with arguments
(`fork`/`execve` with argv + `wait4`), a per-process working directory
(`cd`/`pwd`/relative paths). The full Unix-process core (PIDs, `fork`/`execve`/`wait4`,
basic signals, memory reclamation) and a readable-writable hierarchical FAT32 underpin it.
**Now on the libc track (M9), prioritized over M8 networking** per the north star (run
Linux software → libc first): **M9a–M9f** built the libc-facing syscall surface (`brk`, TLS,
`stat`/`fstat`, time, identity+`uname`, `writev`/`readv`/`fcntl`), and **M9g** proved a clang-built
**C program** runs in ring 3. Next: grow a libc on that path (minimal → relibc).

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
  - **M6f — Process model** (done). PIDs + fork/exec/wait + basic signals. Stages:
    **M6f1 — process table (PIDs + `getpid`)**, **M6f2 — `execve`** (loads from the FAT disk),
    **M6f3 — `fork`** (copies the address space + fd table + full register context),
    **M6f4 — `wait4`** (zombies + block/wake, on a per-process syscall stack), **M6f5 — basic
    signals** (`kill` + default-terminate actions; ring-3 faults become `SIGSEGV`; `wait`
    reports `WIFSIGNALED`).
  - **M6g — Filesystem breadth** (in progress). Write support across the stack. Stages:
    **M6g1 — virtio-blk sector write** (done; `write_sector` via DMA, `VIRTIO_BLK_T_OUT`),
    **M6g2 — FAT file write** (done; `fs::write_file` — free-cluster alloc + chain link + dir
    entry, mirrored to all FATs), **M6g3 — `write(fd)`/`O_CREAT` syscalls** (done; write-back on
    `close`), **M6g4 — subdirectories + `mkdir`** (done; path resolution, `.`/`..`), **M6g5 —
    directory listing** (done; `getdents64` → `linux_dirent64`, so `ls` works), M6g6 a real VFS
    layer (optional — internal refactor, deferrable until a second filesystem exists).
- **M7 — init + shell** (done). Boot lands in an interactive shell. Decisions: utilities
  are **external ELF** binaries the shell `fork`+`execve`s (so **argv** is on the early path), and
  **cwd lives in the kernel** (`chdir`/`getcwd`, inherited across fork/exec). Stages:
  - **M7a — stdin / console** (done). Line discipline (line-buffered input, echo, backspace) +
    blocking `read(0)`, wired to the keyboard task; reuses the scheduler's block/wake (like
    `wait4`). A ring-3 program reads a typed line.
  - **M7b — argv/envp** on the user stack (`execve` + `spawn_user` build the SysV initial stack).
  - **M7c — cwd in the kernel** (`chdir`/`getcwd` + relative path resolution).
  - **M7d — init + shell** (done; M7e folded in). Clean boot (dropped the A/B demo + the fixed
    process list), and a `shell` launched as PID 1: a no-alloc REPL over stdin with builtins
    `cd`/`pwd`/`exit` and external programs via `fork`/`execve`/`wait4`. Console reads are now
    one line per `read(0)` (canonical). Boot lands in an interactive `/$ ` prompt.
  - **M7f — coreutils** (done). `echo`/`cat`/`ls`/`mkdir` as external ELF in `/bin`; the shell
    resolves a bare command name to `/bin/<cmd>` (one-dir PATH). New `mkdir(2)` syscall (the FAT
    `mkdir` from M6g4 reaches ring 3). Boot has a usable command set.
  - **M7g — redirection, pipes, rm/rmdir** (done). **M7g1** generalized the fd table (0/1/2 are
    real entries) + `dup2` + flush-on-exit → `<`/`>`/`>>`. **M7g2** added `pipe(2)` + blocking
    pipe fds (ref-counted ends) → `a | b`. **M7g3** added `unlink`/`rmdir` syscalls + FAT delete
    → `rm`/`rmdir`. The shell is now a usable interactive environment.

## Tier D — A "real" OS

> **Order:** M9 (libc) is being taken **before** M8 (networking) — the north star is running
> Linux software, and a libc is the most direct path there. M8 remains queued.

- **M9 — POSIX / libc.** Port a libc (candidate: `relibc`) — enough to build and run real
  programs. Approached by first filling out the syscalls a libc needs, then the port. Stages:
  - **M9a — process heap (`brk`)** (done). Per-process `brk`/program break with on-demand page
    mapping in a private heap region (`USER_HEAP_BASE`), grown/shrunk by mapping/unmapping user
    pages in the active address space; inherited across `fork` (heap pages copied), reset on
    `execve`, freed on `exit`. Unblocks `malloc`/`Vec` in ring 3.
  - **M9b — TLS (`arch_prctl ARCH_SET_FS`)** (done). Per-thread FS-segment base for thread-local
    storage (libc keeps `errno` in TLS). `arch_prctl(158)` SET_FS/GET_FS writes `IA32_FS_BASE`;
    the base is stored per-thread and **restored on every context switch** (else TLS would leak
    between processes), inherited on `fork`, reset on `execve`.
  - **M9c — file metadata (`stat`/`fstat`/`lstat`)** (done). Synthesizes the Linux x86-64
    `struct stat` (type + size + pseudo-inode) by exact ABI offsets; `fstat` distinguishes the fd
    backing (regular/dir/char-device/FIFO, so `isatty` works).
  - **M9d — time (`clock_gettime`/`gettimeofday`/`time`)** (done). Derives uptime from the PIT tick
    counter (`u128` tick→ns conversion); REALTIME == MONOTONIC == uptime (no RTC yet).
  - **M9e — identity + `uname`** (done). `getuid`/`geteuid`/`getgid`/`getegid` (all 0, single-user),
    `getppid` (parent PID), `uname` (`struct utsname`: ferros/x86_64).
  - **M9f — `writev`/`readv`/`fcntl`** (done). Vectored I/O (libc buffers output through `writev`),
    reusing `sys_write`/`sys_read` per `iovec`; `fcntl` does `F_DUPFD`/`F_GETFL` + no-op flag cmds.
    Completes the libc-facing syscall surface.
  - **M9g — first C program** (done). A clang-compiled freestanding C binary (no libc) runs in
    ring 3 and makes Linux syscalls — proves the clang→ELF→loader→ring-3 path and the C ABI.
    `build.rs` now compiles C user programs (`clang -ffreestanding -mcmodel=large` + our linker
    script), so **clang/lld is a build requirement**.
  - **relibc recon** (done, blocked). Confirmed relibc has a `linux` platform backend making raw
    Linux syscalls (our ABI), so it's a valid eventual target — but its build is blocked here: the
    required `dlmalloc-rs` submodule lives only on the slow `gitlab.redox-os.org` (clone times out),
    plus Redox-centric deps. Pivoted to a hand-rolled minimal libc.
  - **M9h — minimal libc** (done). `user/c/libc`: `crt0` (`_start`→`main`+argv→`exit`),
    `malloc`/`free` (bump over `brk`), `mem`/`str`/`puts`. A C program with a standard `int main()` +
    `malloc` runs. Required **enabling SSE** at boot (`arch::enable_sse`) — clang `-O2` vectorizes to
    SSE, which x86-64 mandates; the kernel stays soft-float.
  - **M9i — FPU/SSE context save** (done). Now that ring 3 uses SSE, `switch_task` `fxsave`/`fxrstor`s
    a per-thread FPU area on every switch (inherited on `fork`), so XMM/MXCSR no longer leak between
    processes. Next: grow the libc (`printf`/stdio), and/or revisit relibc when its build is reachable.
- **M8 — Networking** (in progress). NIC driver (virtio-net) + TCP/IP via `smoltcp` (vendored per
  D13 — a real stack is the "genuinely complex" kind we reuse). Stages:
  - **M8a — NIC detection** (done). QEMU gets a `virtio-net-pci` over user-mode (SLIRP) networking;
    `drivers/virtio_net` finds the device (`1af4:1000`, legacy I/O BAR like virtio-blk), does the
    status handshake, and reads its MAC. Proves the NIC is visible.
  - **M8b — the driver proper** (done). Two virtqueues (RX/TX, shared `Virtq` type), the 10-byte
    `virtio_net_hdr`, `send`/`recv` of Ethernet frames. Tested by a deterministic **ARP round-trip**
    against the SLIRP gateway: ARP-who-has 10.0.2.2 → SLIRP's ARP reply (sender IP 10.0.2.2) comes
    back on RX.
  - **M8c — `smoltcp` integration** (done). Vendored `smoltcp` 0.13 (no_std); `net::VirtioPhy`
    adapts our driver to `smoltcp::phy::Device`; `net::dhcp_acquire` runs a DHCP client and gets an
    IP. Tested: SLIRP's DHCP server leases the guest **10.0.2.15/24** — proving the whole stack
    (our Device, ARP, UDP, DHCP) works end-to-end.
  - **M8d** — ICMP ping / TCP sockets, then a socket syscall surface for ring 3. Decomposed:
    - **M8d1 — ICMP ping** (done). `net::ping` brings up DHCP (refactored into a shared
      `dhcp_configure` that also sets the interface IP + default route), then drives a `smoltcp`
      `icmp` socket (feature `socket-icmp`): sends Echo Requests, counts matching Echo Replies. Tested
      by pinging the SLIRP gateway **10.0.2.2** (SLIRP answers its own gateway's echo internally — no
      host ICMP/internet needed, so the test is deterministic): `[test] ping 10.0.2.2: 3/3 replies`.
    - **M8d2 — UDP socket + DNS resolve** (done). `net::resolve` brings up DHCP, then drives a
      smoltcp `udp` socket (feature `socket-udp`) — the same `bind`/`send`/`recv` path the ring-3
      syscalls will wrap. DNS itself (`net::dns`: `build_query`/`parse_first_a`, bounds-safe,
      handles name compression) is hand-rolled; the full resolver would be reused. Tested by
      resolving `dns.google` via SLIRP's resolver 10.0.2.3 to one of Google's stable anycast IPs:
      `[test] dns.google -> 8.8.4.4`. (Needs working host DNS — the accepted trade-off of the UDP
      path vs. ICMP, where SLIRP itself answered.)
    - **M8d3** — socket syscall surface for ring 3 (`socket`/`bind`/`connect`/`sendto`/`recvfrom`…) +
      a ring-3 demo program. Closes networking onto the north star (Linux software via POSIX sockets).
      Will need a persistent net stack (interface + sockets under a `Mutex`, polled on demand).
- **M10 — Graphics / GUI (optional, huge).** Framebuffer, compositor, window
  manager, toolkit.
- **M11 — Real hardware.** UEFI boot (migrate off bootloader 0.9), drivers for a
  specific laptop / Raspberry Pi, USB.
- **M12 — Self-hosting (the dream).** A compiler runs inside ferros itself.

## Reality check

Tiers A–C are achievable solo (with AI assistance) given unlimited time. Tier D is
a multi-year commitment and a perpetual chase of moving hardware. Smartphones are a
separate project (a mainline-Linux fork, not an OS from scratch) and are out of scope.
