# ferros — Landscape (breadth)

[ROADMAP.md](ROADMAP.md) is **depth** — the linear path forward (M0 → M12).
This document is **breadth** — the menu of real alternatives at each layer: what
ferros uses now, the other options, the trade-offs, and when we might switch. It's
here so the choices are understood, not accidental, and so it's clear where the
project *could* branch out sideways.

A useful mental model — three different things people often conflate:

- **Firmware** — the motherboard's first code. *BIOS* vs *UEFI*.
- **Bootloader** — software that runs after firmware, sets up the CPU, and loads our
  kernel. **Part of our boot image.**
- **Runner** — where we execute the whole thing (QEMU, Bochs, real hardware).
  **Not part of the OS** — swapping it doesn't change ferros, only where it runs.

---

## 1. Firmware: BIOS vs UEFI

| | What it is | ferros |
|---|---|---|
| **BIOS** (legacy) | Old firmware; starts the CPU in 16-bit real mode. Simple, very well documented for learning. Being removed from new hardware. | **current** (via bootloader 0.9) |
| **UEFI** (modern) | Standard on all PCs since ~2012. Boots straight into 64-bit, hands us a pixel framebuffer (GOP), richer services. | planned at **M11** |

## 2. Bootloader (loads our kernel)

| Option | Firmware | Notes |
|---|---|---|
| **`bootloader` 0.9** (Rust) | BIOS | **current.** Trivial to use with `bootimage`; the blog_os path. Maintenance-mode, BIOS-only. |
| **`bootloader` 0.11+** (Rust) | BIOS + UEFI | Same author, modern API, gives a framebuffer. Our **planned** target (M11). |
| **Limine** (C) | BIOS + UEFI | Very robust, great docs, popular with hobby OSes. Strong alternative if we outgrow `bootloader`. |
| **GRUB / Multiboot2** | BIOS + UEFI | The classic (what Linux-likes use). Heavyweight but ubiquitous; boots almost anything. |
| **Write your own** | — | Real mode → protected → long mode by hand. Maximally educational, maximally a rabbit hole. |

## 3. Runner (the test bench — not the OS)

| Option | Why | ferros |
|---|---|---|
| **QEMU** | Fast, scriptable, headless-friendly (great for CI), emulates many architectures. | **current** |
| **Bochs** | Slower but extremely precise emulation + a powerful built-in debugger. Good for nasty CPU-level bugs. | optional |
| **VirtualBox / VMware** | Closer to real virtualization; a good sanity check before real hardware. | optional |
| **Real hardware** | Boot from USB on an actual PC — the ultimate test. Needs UEFI/BIOS boot + real drivers. | **M11+** |
| **Cloud / CI** | Run QEMU headless in pipelines. | later |

## 4. CPU architecture

| Option | Why it matters | ferros |
|---|---|---|
| **x86_64** | Best docs, easiest to test, the dev machine. | **current** |
| **AArch64 (ARM64)** | Raspberry Pi, phones — the path to real devices. | later |
| **RISC-V** | Open, clean, great for experiments. | later |

Switching architecture = new low-level boot code + new drivers; the higher-level
kernel (scheduler, filesystem, etc.) is mostly portable.

## 5. Output / display

| Option | Notes | ferros |
|---|---|---|
| **VGA text mode** (`0xb8000`) | Simplest; BIOS-only; 80×25 characters. | **current (M1)** |
| **Framebuffer** (raw pixels) | Via UEFI GOP or `bootloader` 0.11. Needed for graphics/GUI (M10) and required on UEFI (no text mode there). | M10/M11 |
| **Serial** (COM1) | A "second screen" to the host console; perfect for logs and tests. | **M1b** |

## 6. Kernel design

| Option | Trade-off | ferros |
|---|---|---|
| **Monolithic** | Drivers in the kernel. Simpler, faster to build; the Linux path. | **current** |
| **Microkernel** | Tiny kernel; drivers in userspace (Redox, seL4). Safer/isolated, but slower and more complex. | — |
| **Framekernel** | Monolithic performance + intra-kernel privilege separation: all `unsafe` confined to a tiny audited core (Asterinas's OSTD), the rest is safe Rust. | **under study (D9)** |
| **Hybrid / unikernel / exokernel** | Other points in the design space (e.g. macOS XNU is hybrid; unikernels bundle one app + kernel). | — |

## 7. Rust OS landscape — who's already here

Worth knowing, both as prior art and as honest competitive context (our goal is Linux
compatibility, **D8** — which is a goal, not a distinguishing thesis):

| Project | What it is | Why it matters to us |
|---|---|---|
| **Asterinas** | Linux **ABI-compatible** kernel in safe Rust; **framekernel** (unsafe confined to OSTD, ~14% TCB); 230+ Linux syscalls; perf on par with Linux; USENIX ATC'25, aiming production for x86-64 VMs. | The benchmark for "Rust + Linux-compatible." The exact niche we'd enter — so we don't win on "general-purpose"; the OSTD unsafe-containment idea is worth borrowing (D9). |
| **Redox** | Microkernel OS in Rust with its own POSIX userland (`relibc`). | Source-level POSIX done seriously; `relibc` is our candidate libc (M9). |
| **Theseus** | Research OS, single address space, intralingual design. | A radically different point in the design space; ideas, not a path. |
| **Hubris** (Oxide) | Small, statically-defined RTOS in Rust; **not** Linux-compatible. | If a real-time thesis is ever chosen, this is the closest reference. |

---

## How this maps to the roadmap

Most "breadth" switches are deliberately deferred to keep momentum:

- **M11** is the big lateral jump: BIOS → **UEFI**, `bootloader` 0.9 → 0.11 (or Limine),
  VGA text → **framebuffer**, QEMU → **real hardware**.
- New **architectures** (ARM/RISC-V) are a separate track we can open once the x86_64
  kernel is mature.
- The **runner** can change any time (it's just the test bench) — e.g. add Bochs when
  we hit a CPU bug QEMU hides.
