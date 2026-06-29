# ferros — guide for Claude Code

ferros is an operating system written from scratch in Rust. Long-term project
(a real product, 1–3 year horizon — not a toy). **This file is the source of
truth for how we work in this repository.**

## Locked decisions

- **Architecture:** x86_64 first (ARM/RISC-V later).
- **Kernel:** monolithic.
- **Path:** follow [blog_os](https://os.phil-opp.com/) for milestones M0–M4.
- **Bootloader:** `bootloader` 0.9 + `bootimage` (BIOS) for now; migrate to
  `bootloader` 0.11 / UEFI around M11.
- **License:** proprietary, All Rights Reserved (see `LICENSE`).

## Rules of engagement

1. **Never invent APIs.** Verify crate/language APIs against context7 / docs.rs
   before writing code. Crate APIs drift between versions — confirm against the
   version actually pinned in `Cargo.toml`.
2. **PR-only development.** Work on a branch (`feat/...`, `fix/...`, `docs/...`),
   open a PR, run `/code-review`, then merge into `main`.
3. **Conventional Commits**, scope = subsystem:
   `feat(vga): ...`, `fix(mem): ...`, `docs(boot): ...`, `chore(ci): ...`.
4. **Local, free CI only.** A `pre-push` git hook runs `cargo fmt --check`,
   `cargo clippy`, `cargo build`, and `cargo test`. No GitHub Actions.
5. **Docs are part of the work.** `docs/` (narrative) + doc-comments (`//!`, `///`)
   in code. Keep tidy; update the milestone journal in `docs/journal/` as we go.
6. **One milestone at a time.** Each milestone = a working, observable result and
   a merged PR. Detail each milestone only when we reach it.
7. **Teach as we go.** This is also a learning project for the author. Explain the
   *why* behind each piece at a beginner-friendly level (the concept + the hardware/OS
   background, not just the change) — both in chat and in code doc-comments.

## Layout

- `src/` — kernel source, organized by subsystem (`arch`, `drivers`, `mm`, `sched`,
  `syscall`, `fs`, `net`, `util`); all CPU/platform code is isolated under `arch/`.
- `x86_64-ferros.json` — custom build target.
- `.cargo/config.toml` — `build-std` + runner config.
- `rust-toolchain.toml` — pinned toolchain (nightly + components).
- `docs/ROADMAP.md` — full M0 → M12 plan.
- `docs/ARCHITECTURE.md` — current architecture.
- `docs/CONVENTIONS.md` — code-level conventions & patterns (how we write modules).
- `docs/DECISIONS.md` — decision log.
- `docs/HARDENING.md` — deferred "second pass" backlog (security/perf/real-hw).
- `docs/CONTRIBUTING.md` — workflow details.
- `docs/journal/` — per-milestone work log.

## Build & run

```sh
cargo run     # build kernel image and launch QEMU
cargo test    # run integration tests headless in QEMU (from M1 onward)
```

Requires: Rust nightly + `rust-src` + `llvm-tools-preview`, `bootimage`, QEMU, and
**`clang` + LLVM `lld`** (`build.rs` compiles the C user programs from M9g on — the C/libc track).
