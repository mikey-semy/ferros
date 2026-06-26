# ferros — Conventions & Patterns

How we write kernel code in this repo. This is the *code-level* companion to
[ARCHITECTURE.md](ARCHITECTURE.md) (current design), [CONTRIBUTING.md](CONTRIBUTING.md)
(workflow), and [DECISIONS.md](DECISIONS.md) (the why behind locked choices).
These rules exist so the codebase grows the same way regardless of who (or which
session) writes the next module.

## 1. Layering — the `arch` seam (most important)

A kernel's hardest-to-retrofit boundary is **portable vs CPU/platform-specific**.
The roadmap commits to ARM/RISC-V later, so we isolate that boundary from day one.

- **Only `src/arch/**` may touch architecture specifics:** privileged instructions,
  port/MMIO I/O, control registers, descriptor tables, the `x86_64` crate's
  privileged APIs, `asm!`, etc.
- **Everything above** (`mm`, `sched`, `syscall`, `fs`, `net`, and driver *policy*)
  reaches hardware **only through the `arch` seam** — e.g. `arch::init()`.
- **Porting = additive:** a new architecture is a new `arch/<isa>/` submodule that
  implements the same seam. The portable kernel above stays untouched.

## 2. Module layout

- **One subsystem = one directory** `foo/` with `foo/mod.rs`. `mod.rs` declares the
  submodules and owns the public surface (re-exports, the subsystem's `init`);
  implementation lives in sibling files (`mm/paging.rs`, `mm/frame.rs`, …).
- **Short, conventional kernel names:** `arch`, `drivers`, `mm`, `sched`, `syscall`,
  `fs`, `net`, `util` — not verbose prose names.
- **lib + thin bin split:** reusable kernel in `src/lib.rs`; `src/main.rs` is only the
  entry point + panic handler. This lets integration tests in `tests/` link the kernel.

## 3. `unsafe` policy

- **Every `unsafe` block/fn carries a `// SAFETY:` comment** stating the invariant that
  makes it sound. No silent `unsafe`.
- **Keep the `unsafe` surface small:** wrap raw hardware (ports, MMIO, page tables) in
  safe abstractions so callers stay in safe Rust.
- **Never reference a `static mut`** — use `&raw const` / `&raw mut` (see the IST stack
  in `arch/x86_64/gdt.rs`).

## 4. Global mutable state

There's no allocator at boot and no OS to park a thread, so globals are:

- `static X: Mutex<T> = Mutex::new(...)` (`spin::Mutex`) when `T` is `const`-constructible;
- `static X: LazyLock<...>` (`spin::LazyLock`) when init needs runtime work (e.g. the
  serial port runs real I/O — see `drivers/serial.rs`).
- **Deadlock rule:** any lock that an interrupt handler may take must be acquired under
  `x86_64::instructions::interrupts::without_interrupts(...)` on the non-handler side
  (see `_print` in `drivers/vga.rs` and `drivers/serial.rs`).

## 5. Error handling

- Prefer `Result<T, E>` with an explicit per-subsystem error enum over panicking.
- `panic!` is for **genuinely unrecoverable** kernel faults (it halts the machine).
  `unwrap`/`expect` only at init, and only with a message naming the invariant.

## 6. Documentation (teach-as-we-go)

- `//!` at the top of **every module**: what it is *and the hardware/OS why*, at a
  beginner-friendly level — this is also a learning project.
- `///` on **every public item**.
- Tag milestones in docs (`M2a`, `M3a`, …) so code maps to `docs/journal/`.

## 7. Testing

- **Unit tests:** `#[test_case]` in-module (`custom_test_frameworks`), run inside QEMU
  via `cargo test`; output goes to serial, exit via the `isa-debug-exit` device.
- **Integration tests:** `tests/*.rs`, each with its own `_start`. Use `harness = false`
  when the test itself defines success (e.g. `tests/stack_overflow.rs` exits QEMU from
  its double-fault handler).

## 8. Commits & PRs

Conventional Commits, scope = subsystem (`feat(mm): …`, `refactor(arch): …`,
`docs(boot): …`). PR-only: branch → PR → `/code-review` → merge to `main`. The
`pre-push` hook gates on `cargo fmt --check`, `cargo clippy`, `cargo build`,
`cargo test`. Full workflow in [CONTRIBUTING.md](CONTRIBUTING.md).

## 9. Hardening backlog — the "second pass"

The first pass makes a layer **work, cleanly and minimally**. Hardening (security,
performance, real-hardware breadth) is deferred **deliberately**, recorded in
[HARDENING.md](HARDENING.md), and done in dedicated later passes. We don't gold-plate
a layer that's still moving — but we also don't pretend "it boots" means "it's done".
