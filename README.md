# ferros

An operating system written from scratch in **Rust**, targeting **x86_64** with a
**monolithic** kernel.

> **Status: M1 — kernel infrastructure** (VGA `println!`, serial, in-QEMU tests).
> See [docs/ROADMAP.md](docs/ROADMAP.md) for the full plan (milestones M0 → M12).

## What this is

A long-term effort to build a real, eventually POSIX-compatible operating system.
The early kernel milestones (M0–M4) follow the [blog_os](https://os.phil-opp.com/)
approach; from there it grows toward userspace, drivers, networking, and real hardware.

## Build & run

Prerequisites:

- Rust **nightly** with the `rust-src` and `llvm-tools-preview` components
- [`bootimage`](https://crates.io/crates/bootimage) — `cargo install bootimage`
- **QEMU** with `qemu-system-x86_64` on `PATH`

```sh
cargo run     # builds the kernel image and boots it in QEMU
```

## Documentation

- [Roadmap](docs/ROADMAP.md) — milestones M0 → M12 (depth)
- [Landscape](docs/LANDSCAPE.md) — alternatives at each layer (breadth)
- [Architecture](docs/ARCHITECTURE.md) — current design
- [Decisions](docs/DECISIONS.md) — decision log (ADR-style)
- [Contributing / workflow](docs/CONTRIBUTING.md) — branching, commits, local CI

## License

Proprietary — **All Rights Reserved**. See [LICENSE](LICENSE).
