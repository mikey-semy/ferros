# ferros

An operating system written from scratch in **Rust**, targeting **x86_64** with a
**monolithic** kernel.

> **Status: paused at M9 — POSIX / libc**, 30 June 2026.
> Written in five days (26–30 June 2026): 78 commits, ~18k lines of Rust, tests
> a quarter of that. Not abandoned in confusion — stopped at the top of what one
> person can carry. See [docs/ROADMAP.md](docs/ROADMAP.md) for M0 → M12.
>
> **Where it actually got to.** ferros boots, runs ring-3 processes from ELF,
> talks virtio-blk and virtio-net, gets an IP over DHCP, pings its gateway,
> resolves names over DNS, opens TCP sockets from userspace and makes an HTTP
> request — on top of its own libc (`FILE*`, `getopt`, `strtol`, `environ`) with
> a working `wc` built against it.
>
> **What is not done, and why it is the hard part.** Everything above lives in
> QEMU, where each device class has exactly one well-behaved implementation.
> Real hardware (M11) is not more code — it is an endless chase of controller
> models and silicon errata that no documentation describes. M10 (graphics) and
> M12 (self-hosting) are each a separate product.

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

Dual-licensed under either **MIT** ([LICENSE-MIT](LICENSE-MIT)) or **Apache
License 2.0** ([LICENSE-APACHE](LICENSE-APACHE)), at your option — the usual
choice in the Rust ecosystem, and the same terms as
[blog_os](https://os.phil-opp.com/), whose approach the early milestones follow.

Unless you state otherwise, any contribution you intentionally submit for
inclusion shall be dual-licensed as above, with no additional terms.
