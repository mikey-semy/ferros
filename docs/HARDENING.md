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

## M4 — multitasking

- **No thread teardown.** Kernel threads (M4d/M4e) are never removed from the scheduler
  or have their stacks freed — a finished worker just loops yielding forever. Fine for
  bring-up (the demo/test threads run for the lifetime of the kernel); real thread exit
  + stack reclamation comes with a proper process model (M5+).
- **Fixed round-robin, no time accounting.** The scheduler is plain round-robin with one
  timer tick per quantum (~55 ms at the PIT's 18.2 Hz) and no priorities, no per-thread
  CPU accounting, no sleep/wait queues. Good enough to prove preemption; a real scheduler
  (priorities, fairness, a higher-resolution timer than the PIT) is a later pass.
- **Preemption ignores lock holders.** A thread preempted while holding a `spin::Mutex`
  keeps the lock held until it's rescheduled; another thread then spin-waits (and is
  itself preempted, so it resolves on single-CPU, but it's wasteful). Needs a real
  blocking primitive once we care about throughput / SMP.

## M5 — userspace + syscalls

- **Single global syscall stack, no `swapgs`/per-CPU.** The `syscall` entry trampoline
  switches to one global kernel stack via a RIP-relative static — correct only for a
  single core and non-reentrant syscalls (we enter with IF=0). Real userspace needs a
  per-CPU kernel stack selected via `swapgs` + `GS` base, and per-thread kernel stacks.
- **User faults panic the kernel.** The page-fault / GPF handlers (M5a) `panic!` — fine
  while there are no processes, but once M5c loads real programs a user fault must
  terminate *the process*, not the kernel (and eventually become a signal).
- **No W^X / NX on user pages.** `map_user_page` maps `PRESENT|WRITABLE|USER_ACCESSIBLE`
  (the M5a probe page is both writable and executable). Add `NO_EXECUTE` + write-xor-execute
  once the ELF loader sets per-segment permissions (M5c).
- **No per-process address space yet (M5a).** The M5a probe runs in the kernel's address
  space (user pages mapped in the free lower half). Per-process page tables (own PML4,
  kernel higher-half shared) + CR3 switching come in M5c — until then there's no
  user/user or user/kernel memory isolation between "processes".
- **Bootstrap ring-3 entry is one-shot.** `enter_user`/`resume_kernel` do a single
  kernel→user→kernel excursion via a global saved RSP; real scheduling of user threads
  (timer-preempted ring 3 via `rsp0`, many processes) lands in M5c.
- **`sysretq` to a non-canonical RIP (CVE-2012-0217 class).** The syscall return path
  `sysretq`s to `rcx` = the user RIP. In M5a `rcx` is hardware-set to the (canonical) user
  return address, so it's safe. Once user RIP can be set indirectly (signals/`sigreturn`,
  `ptrace`, exec of arbitrary entry) the kernel must reject non-canonical user RIPs (or
  return via `iretq`, which faults in *user* context instead of #GP-ing in ring 0).
- **Page-fault handler not on an IST stack.** It runs on the current kernel stack and only
  reads `CR2` today (safe). When M5c extends it to inspect user memory, a fault *inside*
  the handler could recurse on the same stack — give `#PF` its own IST entry (like
  `#DF`) or keep the handler strictly memory-access-free.
- **`uaccess` is not fault-tolerant (M5b).** `with_user_bytes` range-checks that the buffer
  is in the user half (blocks "pass a kernel pointer"), but a *valid-looking but unmapped*
  user pointer still faults → kernel panic today. Real `copy_from/to_user` needs a fault
  fixup table (extable): the `#PF` handler recognises a fault inside a uaccess region and
  returns `-EFAULT` instead of dying. Also `write` content capture (`LAST_WRITE_*`) is
  test observability in the production path — drop it once there's a better test hook.
- **No per-process address space yet (M5c1).** A loaded ELF runs in the *shared* kernel
  address space (its pages live in the kernel's tables, user-gated by the leaf PTE). There's
  no inter-process isolation and no user/user separation. Deferred per **D10** toward the
  higher-half / bootloader-0.11 move (M11); the ELF loader already takes a mapper, so
  retargeting it to a per-process `AddressSpace` is a small change (M5c2).
- **No W^X / NX on loaded ELF segments (M5c1).** `mm::map_user_page` maps every user page
  `PRESENT|WRITABLE|USER_ACCESSIBLE`, so even an ELF's `.text` is writable and its `.data`
  is executable. The loader ignores `p_flags`. Honor per-segment R/W/X (and set `NO_EXECUTE`)
  once the paging supports it.
- **ELF loader trusts a well-formed, fixed-address `ET_EXEC` (M5c1).** Parsing is bounds-
  checked, but the loader only handles static `ET_EXEC` with absolute vaddrs — no PIE/ASLR,
  no relocations, no dynamic linking, no segment-overlap/`p_align` validation beyond
  page-dedup. Fine for our own embedded binary; real/untrusted binaries need a fuller loader.

## Cross-cutting (whole kernel)

- **No real-hardware validation** — QEMU only until M11 (UEFI + real drivers).
- **Spinlocks everywhere** — fine single-CPU; revisit lock strategy when SMP arrives.
