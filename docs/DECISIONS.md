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
