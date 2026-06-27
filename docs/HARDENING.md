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

- **Slow allocators (performance).** The frame allocator (M3b) is O(n) per *fresh*
  `allocate_frame` (it recycles freed single frames via a free-list since M6e1, but still
  rescans `usable_frames` when bumping the cursor); the heap (M3c, `linked_list_allocator`)
  is O(n) first-fit and fragments. Replace with a bitmap/buddy frame allocator and a
  fixed-size-block (slab) heap on the hot path. Deferred deliberately: correctness-first
  bring-up — these are pure speed wins, not correctness.
- **Frame allocator: single-frame free only, no coalescing (M6e1).** `deallocate_frame`
  recycles individual frames via an intrusive LIFO free-list, but there's no buddy/coalescing,
  and **contiguous runs (`allocate_contiguous`, virtio virtqueues) are never freed** (bump-only,
  can't be reassembled from the LIFO list). Fine while virtio lives forever; a device teardown
  path would need a contiguous-aware allocator.
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
- **`uaccess` fault-tolerance is pre-validation, not extable (M5b → addressed M6d1).**
  `with_user_bytes` now (a) range-checks the user half *and* (b) walks the active page tables
  (`mm::paging::user_range_accessible`) to confirm every page is present + `USER_ACCESSIBLE`
  before the access, returning `-EFAULT` instead of faulting. This closes the
  "valid-range-but-unmapped pointer panics the kernel" hole (and the `write(unmapped_ptr)`
  DoS) on single-CPU, where syscalls run `IF=0` so the mapping can't change between check and
  use. It is **not** the Linux-style fault-fixup (extable): it's two-pass (walk then copy)
  and would race on SMP — a true extable (single-pass, restartable copy, `#PF` looks up the
  faulting RIP) is the SMP/perf upgrade, still deferred. Also `write` content capture
  (`LAST_WRITE_*`) is test observability in the production path — drop it once there's a
  better test hook.
- ~~**No address-space teardown (M5c2).**~~ **Addressed (M6e2):** `AddressSpace::destroy`
  frees the process's private (user) subtree + its PML4 via the freeing allocator, leaving the
  shared kernel entries intact. (Hooking it into process exit is the reaper, M6e3.)
- **User space confined to one L4 slot via a hand-picked high address (M5c2).** Because
  bootloader 0.9 loads the kernel in the lower half, the user ELF/stack are hard-pinned to
  L4 slot 255 (`0x7F80…`, built with `code-model=large`) to guarantee a kernel-free slot.
  After the higher-half move (M11) the user should get the whole lower half (and the small
  code model / a normal base); the `AddressSpace` mechanism itself carries over unchanged.
- **AddressSpace is x86_64-specific but lives in `mm`.** Like the existing `mm::paging`
  (which already uses `Cr3`/`OffsetPageTable` directly), `mm::addr_space` is not yet behind
  the arch seam (D7). Abstract the whole `mm` page-table layer per-arch in a later pass.
- ~~**No process reaping (M5c3a).**~~ **Addressed (M6e3):** a deferred reaper on the zero
  thread (`thread::reap`, run from the executor loop) frees each `Dead` task's address space
  (`AddressSpace::destroy`), kernel stack (`Box`), and fd table (`forget_process`). Remaining:
  the reaped `Thread` stays as a tiny `Reaped` tombstone in the scheduler `Vec` (no compaction
  yet); kernel threads (M4) still aren't torn down; there's still no real process table / PIDs
  (that's Phase 2a, fork/exec/wait).
- **Shared global `syscall` kernel stack (M5c3a).** The `syscall` entry trampoline still
  switches to one global `SYSCALL_KERNEL_RSP` (M5a). Safe while syscalls run to completion
  with IF=0 (non-preemptible, non-reentrant), but a blocking/yielding syscall or SMP needs a
  per-task syscall stack (the per-task kernel stack / rsp0 already exists for ring-3
  interrupts).
- **User-fault termination is coarse, and only covers ring-3 *code* faults (M5c3b).** A
  #PF/#GP taken while executing in ring 3 now kills *the process* (not the kernel) via
  `exit_current`, but there's no signal delivery (`SIGSEGV`), faulting-instruction reporting,
  or core dump — just terminate + a serial line. A real fault model (signals, `wait`-able
  status) comes with a process model. (The separate hole — a *kernel* fault on a user pointer
  inside a syscall, e.g. `write(1, unmapped_ptr, n)` — is **fixed in M6d1**: `uaccess`
  pre-validates the mapping and returns `-EFAULT`; see the uaccess item above.)
- **Scheduler now carries an arch `PhysFrame` (CR3).** `sched::thread::Thread` holds
  `Option<PhysFrame>` and the switch goes through `arch::context::switch_task`; the data type
  leaks x86_64 into the portable scheduler. A neutral "address-space handle" is a later seam.
- **Process address spaces snapshot the kernel's L4 at creation (M5c3a).**
  `AddressSpace::new_sharing_kernel` copies the **kernel** PML4's L4 entries once (M6f2: it
  takes the kernel frame explicitly, not the active one — so `execve`/`fork` from a user
  context don't drag the caller's user slot into the new space); a kernel mapping added
  *later* (a new L4 entry) would NOT appear in already-created process address spaces.
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
- **No safe BAR iterator (64-bit caveat) (M6a).** `PciDevice::bar(i)` decodes a single 64-bit
  BAR correctly (it combines slot `i`+`i+1`), but there's no iterator that *skips* the high
  half — naively walking `bar(0..6)` on a device with a 64-bit memory BAR misreads its high
  dword as a spurious separate BAR. No caller iterates today (virtio uses only `bar(0)`, an
  I/O BAR); add a proper `bars()` iterator when M6c+ first drives an MMIO device.
- **No bus-master enable / MSI yet (M6a).** `config_write_u32` exists but M6a doesn't touch
  the command register; M6b will set bus-master (offset 0x04 bit 2) for virtio DMA. No
  MSI/MSI-X — virtio will be polled, not interrupt-driven, to start.
- **virtio device assumptions (M6a).** The test pins the *transitional legacy* virtio-blk id
  `1af4:1001` with a port-I/O BAR0; a modern-only device (`disable-legacy=on`, id `1af4:1042`,
  MMIO BARs) would need the modern capability-based path. Single virtio-blk device assumed.
- **Disk image is a build artifact, not a fixture (M6a/M6c).** `build.rs` generates
  `target/ferros-disk.img` (64 MiB, FAT32-formatted via the `fatfs` build-dependency) and is
  regenerated only when the size/boot-signature don't match — so editing the embedded test
  file's content without changing the image size won't auto-regenerate (delete the image to
  force it). The test file's name/content are duplicated between `build.rs` and
  `tests/fat_read.rs` (separate crates can't share a const).
- **FAT reader is read-only, FAT32-only, root-dir-only, 8.3-only (M6c).** `fs::fat` reads;
  there's no write/create/delete. It only handles FAT32 (rejects FAT12/16), assumes 512-byte
  sectors, finds files only in the **root** directory (no path parsing / subdirectory
  traversal, though `find_in_dir` is cluster-generic), matches only short **8.3** names (LFN
  entries are skipped, not assembled), and reads a whole file into a `Vec` (no seek/streaming,
  no partial reads). It also re-reads the BPB on every `mount()` and re-reads FAT/dir sectors
  per call with **no caching** — O(sectors) per lookup. A real VFS + a buffer cache + LFN +
  subdirectories + write come later.
- **FAT reader trusts a well-formed image (M6c).** It now bounds cluster numbers to the
  volume (`valid_cluster`, prevents sector-address overflow / wild reads) and caps chain
  traversal (prevents a cyclic-chain hang), but it still **trusts the directory's file size**:
  if the cluster chain is shorter than `size`, `read_file` returns a silently *truncated*
  buffer rather than an error. Like the ELF loader (D10), this is defensive-but-not-complete;
  a fuller reader would cross-check size vs chain length and surface mismatches.
- **virtio-blk is polled, single-request, read-only (M6b).** The driver suppresses the
  device interrupt (`VIRTQ_AVAIL_F_NO_INTERRUPT`) and busy-polls the used ring — no IRQ
  handler, so a `read_sector` blocks the caller (and, under the global `DEVICE` Mutex,
  everyone) until the device replies; there's also no poll timeout, so a wedged device hangs
  the kernel. Only one descriptor chain (desc 0..2) is reused, so there's no queue depth /
  async I/O. No write/flush path. The read bounces device→DMA page→caller (an extra copy)
  and serves exactly one 512-byte sector per request — M6c (FAT) will want multi-sector
  reads and may read straight into the caller's buffer. Interrupt-driven, multi-request,
  writable I/O is a later pass (needs MSI/INTx handling + a real block layer).
- **Accept-zero-features negotiation (M6b).** The driver writes Driver Features = 0 (enough
  for basic legacy read) and doesn't inspect Device Features — it never checks
  `VIRTIO_BLK_F_RO`, block-size, or geometry. Real negotiation comes with write support.
- **`virtio_blk::init` is bound to `BootInfoFrameAllocator` (M6b).** It needs
  `allocate_contiguous` (not on the `FrameAllocator` trait), so it takes the concrete
  allocator. The virtqueue/buffer frames are allocated once and never freed (M3 item). A
  trait for contiguous/DMA allocation would decouple it.

- **File syscalls are minimal and read-only (M6d2).** `open`/`read`/`close`/`lseek` exist,
  but: `open` reads the **whole file into memory** (no streaming/`mmap`, no large-file
  support) and ignores `flags`/`mode`; there's **no** `write`/`create`/`unlink`/`stat`/`dup`,
  no `O_*` flags, no directories (root, 8.3 names — the M6c FAT limits), and `read` on fd 0
  (stdin) isn't a thing. Paths are capped at 256 bytes. A real file model (streaming, write,
  a proper VFS with mount points/inodes) is later work.
- **Per-process fd table keyed by CR3 (M6d2; leak fixed M6e3).** `syscall::files` stores each
  process's open files in a `BTreeMap` keyed by its PML4 physical address (avoids touching the
  scheduler). Now that frames are recycled (M6e1), the reaper **must** drop the entry on exit
  (`forget_process`) — done (M6e3) — else a recycled PML4 would inherit a dead process's fds.
  The CR3 key is still a stand-in for a real PID/process table (Phase 2a).
- **A syscall holds the process-files lock across copy-to-user (M6d2).** `sys_read` keeps the
  global `PROCESSES` `Mutex` while it `copy_to_user`s the data. Safe on single-CPU (syscalls
  run `IF=0`, no ISR touches the map), but on SMP this serialises all file I/O and a blocking
  copy under the lock would be bad. Pairs with the non-preemptible-syscall limitation.

## Cross-cutting (whole kernel)

- **No real-hardware validation** — QEMU only until M11 (UEFI + real drivers).
- **Spinlocks everywhere** — fine single-CPU; revisit lock strategy when SMP arrives.
