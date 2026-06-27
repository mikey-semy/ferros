# ferros — Decision Log

Short, append-only ADR-style records of significant choices. Newest at the bottom.

---

## D1 — Target architecture: x86_64

**Decision:** Start on x86_64.
**Why:** Best documentation, easiest to test in QEMU, and the dev machine is x86_64.
ARM/RISC-V can be added later once the kernel fundamentals are understood.

## D2 — Kernel design: monolithic

**Decision:** Monolithic kernel (drivers in kernel space).
**Why:** Simpler to bring up and faster than a microkernel; the canonical learning
path (blog_os, Linux). A microkernel (à la Redox) was considered but adds complexity
that isn't worth it for a solo start.

## D3 — Follow the blog_os path for M0–M4

**Decision:** Track Philipp Oppermann's "Writing an OS in Rust" for the early kernel.
**Why:** Reliable, well-documented, lets us learn fast and spot its weak points to
improve on later. We deviate deliberately, not accidentally.

## D4 — Bootloader: `bootloader` 0.9 + `bootimage` (BIOS)

**Decision:** Use `bootloader` 0.9.x with the `bootimage` tool for now.
**Why:** Lowest-friction, matches blog_os exactly. The newer `bootloader` 0.11
(BIOS+UEFI) has a different API and thinner tutorial coverage. Plan to migrate to
0.11 / UEFI around M11 (real hardware); kernel code is largely independent of this.

## D5 — License: proprietary, All Rights Reserved

**Decision:** Proprietary license; private repository.
**Why:** This is intended as a real product; the author wants to prevent copying
without benefit. Easy to relicense later (e.g. to a source-available license) if the
strategy changes. Downside: no external contributors while proprietary.

## D6 — Toolchain pinned to nightly

**Decision:** Pin `nightly` in `rust-toolchain.toml`; freeze the exact date after the
first green build for reproducibility.
**Why:** OS dev needs unstable features (`build-std`, custom targets,
`custom_test_frameworks`) that stable does not provide.

## D7 — Source layout: subsystem modules behind an `arch/` seam

**Decision:** Organize `src/` by subsystem directories — `arch`, `drivers`, `mm`,
`sched`, `syscall`, `fs`, `net`, `util` — instead of a flat file list, introduced at the
start of M3 before the tree grows. All CPU/platform-specific code lives under
`arch/<isa>/`; portable subsystems reach hardware only through the `arch` seam
(`arch::init()`). Names use short, conventional kernel terms (`mm`, `fs`, `net`, …).
Code-level patterns are recorded in [CONVENTIONS.md](CONVENTIONS.md), and consciously
deferred hardening in [HARDENING.md](HARDENING.md).
**Why:** The portable-vs-arch boundary is the one that is structurally expensive to
retrofit, and the roadmap commits to ARM/RISC-V later — isolating it now keeps the
portable kernel portable (a new arch = a new `arch/` submodule, nothing else moves).
Subsystem directories prevent a flat "everything in `src/`" that becomes unnavigable at
scale. Short names match Linux/BSD convention and read as professional rather than
verbose. Shipped as its own `refactor(layout)` change with **no behavior change** (build
+ fmt + clippy green; existing tests compile and pass unchanged).

## D8 — Goal: Linux compatibility (ABI-level north star, POSIX-first path)

**Decision:** ferros aims to **run the existing Linux software ecosystem** rather than
require a from-scratch native app ecosystem. North star = **ABI-level** compatibility
(run *unmodified* Linux ELF binaries by emulating the Linux syscall interface). The
**path** is staged: **source-level POSIX first** — a syscall layer + a ported libc
(relibc, M9) so programs recompile — then grow that syscall surface toward
unmodified-binary support. Windows apps are out of scope except via **Wine-class**
translation layers running on the Linux ABI.
**Why:** Writing a thousand native apps is infeasible solo; the value is leverage of the
Linux ecosystem. Source-level is a strict prerequisite for ABI-level (same syscall
layer), so staging keeps every step independently useful and avoids an all-or-nothing
bet. The decision bites at **M5** (userspace/syscalls), not before — M0–M4 are
compatibility-agnostic.
**Honest caveats:** (1) ABI-level "any Linux binary runs" is a multi-year, hundreds-of-
syscalls effort (signals, futex, mmap semantics, `/proc`, ioctls). (2) This is **exactly
[Asterinas](https://github.com/asterinas/asterinas)'s niche** (Rust + Linux-ABI, 230+
syscalls, USENIX ATC'25) — so "Linux-compatible Rust OS" is a *goal*, **not a
distinguishing thesis**; a sharper angle (e.g. real-time/determinism) would layer on top
(still open — see D9 note). (3) **Wine** is among the most demanding hosts; treat it as a
far aspiration, not a milestone.

## D9 — Unsafe discipline: framekernel-inspired containment (under study)

**Decision (tentative):** Keep `unsafe` **minimal and contained**, and study the
**framekernel** approach (Asterinas's OSTD): confine all `unsafe` to a small, auditable
core and write the rest of the kernel in safe Rust. Not adopting a formal OSTD-style
framework yet — but treat "where does unsafe live" as a deliberate boundary, like the
`arch` seam (D7), because it is far cheaper to establish early than to retrofit once
`unsafe` is scattered. Each `unsafe` block already carries a `// SAFETY:` note
([CONVENTIONS.md](CONVENTIONS.md) §3); this decision is to revisit a stronger containment
boundary before userspace (M5) accretes much more of it.
**Why:** Memory-safety is Rust's whole pitch for an OS; an explicit `unsafe` boundary is
the difference between "Rust kernel" and "kernel that happens to be in Rust." Recorded as
*under study* rather than committed, since a full framekernel is a research-grade effort.

## D10 — M5 sequencing: ELF binaries before per-process address spaces

**Decision:** Within M5 (userspace), do **ELF loading first** (M5c1: load and run a real,
separately-compiled static ELF in ring 3, in the *shared* kernel address space), and
**defer true per-process address-space isolation** (own PML4 per process) to a later step
(M5c2), aligned toward the higher-half / `bootloader` 0.11 migration (M11). User code is
protected from reading kernel memory by the leaf-PTE `USER_ACCESSIBLE` bit; what's deferred
is inter-process isolation and user-cannot-see-other-user.
**Why:** `bootloader` 0.9 loads the kernel in the **lower** half (physmem at L4 index 3,
heap at 170), so a clean per-process split (kernel in the higher half, user in the lower)
isn't natural yet — doing it now means awkward "find a free L4 slot" surgery that the M11
higher-half move makes trivial. ELF loading is **orthogonal** to isolation and is the
higher-value step for the D8 north star (it's exactly how relibc/real programs will load),
so it goes first. The two parts compose cleanly later: the ELF loader already takes a
mapper, so pointing it at a per-process `AddressSpace` is a small change. ELF parser is
**hand-rolled** (no new dependency) — minimal-deps + teach-as-we-go.

## D11 — Storage path: PCI + virtio-blk + hand-rolled FAT

**Decision:** For M6 storage, use a **virtio-blk** disk driver (over **ATA PIO** and AHCI),
which makes **minimal PCI bus enumeration** the prerequisite first step (M6a); read the
filesystem with a **hand-rolled FAT** reader (over a crate like `fatfs`, and over ext2).
Start read-only. Decomposition: M6a PCI enumeration → M6b legacy virtio-blk (read sectors)
→ M6c FAT read → M6d VFS + file syscalls.
**Why:** virtio is the modern, fast, well-specified device QEMU emulates cleanly; it is the
same family we'll want for networking (virtio-net, M8), so the PCI + virtqueue groundwork
pays off twice. ATA PIO would have been simpler (no PCI), but virtio better matches a
"build to last" kernel and the eventual real-hardware story. PCI enumeration is small and
reusable (every PCI device needs it). FAT is hand-rolled for the same reasons as the ELF
loader (D10): no dependency, full control, teach-as-we-go; FAT (not ext2) because it is the
simplest real filesystem to read and the roadmap's stated starting point. The **arch seam**
(CONVENTIONS §1) splits the PCI driver: the x86-specific config mechanism (ports 0xCF8/0xCFC)
lives in `arch/x86_64/pci.rs`; the portable device model + enumeration in `drivers/pci.rs`.

**Refinement (M6c):** the FAT variant is **FAT32** (the roadmap's choice), so the test disk
image grew from 4 MiB to 64 MiB (FAT32 needs ≥65525 clusters). The kernel's reader is
hand-rolled (read-only, 8.3 names), but the test **image** is created with the `fatfs` crate
as a **`[build-dependencies]`** entry — a host-only tool in `build.rs` that formats the image
and writes a test file. `fatfs` is therefore *not* a kernel/runtime dependency: we only use a
trusted tool to produce a fixture, while still learning to *read* FAT by hand.
