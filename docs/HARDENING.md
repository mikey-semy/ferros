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
- **No address-space teardown (M5c2).** `AddressSpace::new_sharing_kernel` allocates a PML4
  frame, and the process's user page-table subtree + page frames are never freed (the frame
  allocator never frees anyway — M3 item). Fine for the one-shot program; real process exit
  must reclaim them.
- **User space confined to one L4 slot via a hand-picked high address (M5c2).** Because
  bootloader 0.9 loads the kernel in the lower half, the user ELF/stack are hard-pinned to
  L4 slot 255 (`0x7F80…`, built with `code-model=large`) to guarantee a kernel-free slot.
  After the higher-half move (M11) the user should get the whole lower half (and the small
  code model / a normal base); the `AddressSpace` mechanism itself carries over unchanged.
- **AddressSpace is x86_64-specific but lives in `mm`.** Like the existing `mm::paging`
  (which already uses `Cr3`/`OffsetPageTable` directly), `mm::addr_space` is not yet behind
  the arch seam (D7). Abstract the whole `mm` page-table layer per-arch in a later pass.
- **No process reaping (M5c3a).** `exit` marks the task `Dead` and the scheduler skips it,
  but its memory (kernel stack, the process PML4 + user page tables + user frames) is never
  freed — a zombie leak. Add reaping (and a real process table) in M5c3b; depends on a
  frame allocator that frees (M3 item).
- **Shared global `syscall` kernel stack (M5c3a).** The `syscall` entry trampoline still
  switches to one global `SYSCALL_KERNEL_RSP` (M5a). Safe while syscalls run to completion
  with IF=0 (non-preemptible, non-reentrant), but a blocking/yielding syscall or SMP needs a
  per-task syscall stack (the per-task kernel stack / rsp0 already exists for ring-3
  interrupts).
- **User-fault termination is coarse, and only covers ring-3 *code* faults (M5c3b).** A
  #PF/#GP taken while executing in ring 3 now kills *the process* (not the kernel) via
  `exit_current`, but: (a) no signal delivery (`SIGSEGV`), faulting-instruction reporting,
  or core dump — just terminate + a serial line; and (b) a fault the **kernel** takes on a
  user pointer inside a syscall (uaccess to a valid-range-but-unmapped address) is a ring-0
  fault → still panics the kernel (the uaccess/extable item above). So a user can still DoS
  the kernel via e.g. `write(1, unmapped_user_ptr, n)` until uaccess is fault-tolerant.
  Both need the extable + a real process/signal model.
- **Scheduler now carries an arch `PhysFrame` (CR3).** `sched::thread::Thread` holds
  `Option<PhysFrame>` and the switch goes through `arch::context::switch_task`; the data type
  leaks x86_64 into the portable scheduler. A neutral "address-space handle" is a later seam.
- **Process address spaces snapshot the kernel's L4 at creation (M5c3a).**
  `AddressSpace::new_sharing_kernel` copies the active L4 entries once; a kernel mapping
  added *later* (a new L4 entry) would NOT appear in already-created process address spaces.
  This is correct **only** because the kernel heap is a fixed, pre-mapped 100 KiB region
  (one L4 entry, never grows) and the kernel maps no new L4 entries after boot — so process
  kernel stacks (heap) and the loader's allocations always live in copied entries. A growing
  heap / new kernel mappings (and SMP) would need to propagate kernel higher-half changes to
  all process tables (the standard kernel concern); revisit with a dynamic heap / the
  higher-half move (M11).
- **No W^X / NX on loaded ELF segments (M5c1).** `mm::map_user_page` maps every user page
  `PRESENT|WRITABLE|USER_ACCESSIBLE`, so even an ELF's `.text` is writable and its `.data`
  is executable. The loader ignores `p_flags`. Honor per-segment R/W/X (and set `NO_EXECUTE`)
  once the paging supports it.
- **ELF loader trusts a well-formed, fixed-address `ET_EXEC` (M5c1).** Parsing is bounds-
  checked, but the loader only handles static `ET_EXEC` with absolute vaddrs — no PIE/ASLR,
  no relocations, no dynamic linking, no segment-overlap/`p_align` validation beyond
  page-dedup. Fine for our own embedded binary; real/untrusted binaries need a fuller loader.

## M6 — storage + filesystem

- **Brute-force PCI scan, no bridges/PCIe ECAM (M6a).** `pci::enumerate` probes all
  256 buses × 32 slots (× 8 funcs when multifunction) via the legacy 0xCF8/0xCFC mechanism.
  Fine on QEMU's flat i440fx bus, but it doesn't recurse through PCI-to-PCI bridges
  (`config_read` of a bus behind an unconfigured bridge returns `0xFFFF`), ignores PCIe
  extended config (MMIO ECAM, offsets ≥ 0x100), and re-scans the whole bus on every `find`.
  A real enumerator walks bridges, caches devices, and supports ECAM.
- **No BAR sizing / remapping (M6a).** `PciDevice::bar` decodes the BARs the firmware already
  programmed; it never sizes a BAR (write all-ones, read back the mask) or assigns addresses.
  QEMU pre-assigns them, so reading is enough for virtio (M6b); a from-scratch resource
  allocator is a later concern.
- **No bus-master enable / MSI yet (M6a).** `config_write_u32` exists but M6a doesn't touch
  the command register; M6b will set bus-master (offset 0x04 bit 2) for virtio DMA. No
  MSI/MSI-X — virtio will be polled, not interrupt-driven, to start.
- **virtio device assumptions (M6a).** The test pins the *transitional legacy* virtio-blk id
  `1af4:1001` with a port-I/O BAR0; a modern-only device (`disable-legacy=on`, id `1af4:1042`,
  MMIO BARs) would need the modern capability-based path. Single virtio-blk device assumed.
- **Disk image is a build artifact, not a fixture (M6a).** `build.rs` generates a 4 MiB raw
  `target/ferros-disk.img` with a known signature; it's regenerated on size/signature
  mismatch only. M6c will format it as FAT (likely via a host-side `fatfs` build-dependency).

## Cross-cutting (whole kernel)

- **No real-hardware validation** — QEMU only until M11 (UEFI + real drivers).
- **Spinlocks everywhere** — fine single-CPU; revisit lock strategy when SMP arrives.
