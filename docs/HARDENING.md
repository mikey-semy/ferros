# ferros — Hardening Backlog ("second pass")

The roadmap ([ROADMAP.md](ROADMAP.md)) drives ferros *depth-first* to a working OS:
each milestone's first pass is a clean, minimal **walking skeleton** — "it works and
you can see it", not "it's secure, fast, and production-grade". That is deliberate
(see [CONVENTIONS.md](CONVENTIONS.md) §9): we don't harden a layer that's still moving.

This file is where the deferred work is recorded instead of forgotten — so each
"second pass" is *planned*, not a surprise. Items graduate from here into real
milestones once the underlying layer is stable. Breadth alternatives at each layer
live in [LANDSCAPE.md](LANDSCAPE.md); this file is about hardening what we already have.

> Convention: when a milestone consciously skips hardening, log it here in that
> milestone's section, one line per item, with a short *why-deferred*.

## M1 — drivers (VGA, serial)

- **VGA is ASCII-only** — non-ASCII bytes render as `■`. Proper UTF-8 handling deferred
  (cosmetic; revisit if/when we add a framebuffer at M10/M11).

## M2 — interrupts

- **PIC 8259, not APIC.** Real hardware and SMP need the local APIC + IO-APIC (+ MSI).
  Deferred: PIC is enough for single-CPU QEMU bring-up; APIC is a larger, SMP-era task.
- **No SMP.** Single-CPU only; no per-CPU state, no IPIs, no TLB shootdown yet.

## M3 — memory

- **Slow allocators (performance).** The frame allocator (M3b) is O(n) per
  `allocate_frame` and never frees frames; the heap (M3c, `linked_list_allocator`) is
  O(n) first-fit and fragments. Replace with a bitmap/buddy frame allocator and a
  fixed-size-block (slab) heap on the hot path. Deferred deliberately: correctness-first
  bring-up — these are pure speed wins, not correctness.
- **No W^X / NX.** Heap and mapped pages are `PRESENT | WRITABLE` with no `NO_EXECUTE`;
  nothing enforces write-xor-execute. Revisit with userspace (M5).
- **No guard pages.** Around the kernel heap region; a heap overrun silently corrupts
  neighbours. Add unmapped guard pages.
- **`create_example_mapping` is demo-only.** It maps an arbitrary page to the VGA frame
  via an unchecked `map_to` — a teaching one-shot, not a real mapping API.
- **Fixed 100 KiB heap, no growth-on-demand.** Enlarge or grow dynamically later.
- **KASLR, SMEP/SMAP, huge pages, demand paging / copy-on-write** — none yet.
- **Single global spinlock on the heap** — a contention point once we have SMP (no SMP
  yet, so moot for now).

## Cross-cutting (whole kernel)

- **No real-hardware validation** — QEMU only until M11 (UEFI + real drivers).
- **Spinlocks everywhere** — fine single-CPU; revisit lock strategy when SMP arrives.
