# ferros — Contributing & Workflow

Solo project for now, but we follow a real workflow from day one.

## Prerequisites

- Rust **nightly** with `rust-src` + `llvm-tools-preview` (pinned in `rust-toolchain.toml`)
- `bootimage` — `cargo install bootimage`
- **QEMU** with `qemu-system-x86_64` on `PATH`

## Build, run, test

```sh
cargo build      # compile the kernel for x86_64-ferros
cargo run        # build the boot image and launch it in QEMU
cargo test       # run integration tests headless in QEMU (from M1 onward)
cargo clippy      # lints
cargo fmt         # format
```

## Branching model

- `main` — always builds and boots. Protected by convention; no direct commits.
- Work branches:
  - `feat/<scope>-<short>` — new functionality (e.g. `feat/m0-boot`)
  - `fix/<scope>-<short>` — bug fixes
  - `docs/<short>` — documentation only
  - `chore/<short>` — tooling, CI, deps

## Commits — Conventional Commits

Format: `type(scope): summary`, scope = kernel subsystem.

```
feat(vga): add scrolling text writer
fix(mem): correct frame allocator off-by-one
docs(boot): document the VGA cell layout
chore(ci): add pre-push hook
```

Types: `feat`, `fix`, `docs`, `chore`, `refactor`, `test`, `perf`.

## Pull requests

Every change lands via a PR:

1. Branch off `main`.
2. Commit using the convention above.
3. Open a PR; run `/code-review` on it.
4. Address findings, then merge into `main`.

## Local CI (free, no GitHub Actions)

A `pre-push` git hook runs the same checks CI would:

```sh
cargo fmt --check
cargo clippy -- -D warnings
cargo build
cargo test
```

If any step fails, the push is rejected. (Installed under `.git/hooks/pre-push`;
the source of truth lives in `docs/` / a tracked `hooks/` dir so it can be reinstalled.)

## Documentation

- Narrative docs live in `docs/`. Keep them current with the code.
- Code carries doc-comments (`//!` per module, `///` per item).
- Each milestone gets a log entry in `docs/journal/`.
- Decisions go in `docs/DECISIONS.md`.
