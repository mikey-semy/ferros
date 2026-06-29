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
- **Fixed 1 MiB heap, no growth-on-demand (raised from 100 KiB in M6f3).** The process model
  holds control structures + each process's 16 KiB kernel stack on the heap, and fork/exec copy
  address spaces and read ELFs — a handful of processes overran 100 KiB. Still a fixed size; grow
  dynamically later.
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

- **Syscall kernel stack is per-process now, but still no `swapgs`/per-CPU (M5a → M6f4).**
  The `syscall` entry trampoline switches to a kernel stack via a RIP-relative static. M6f4
  made that static **per-process** (the scheduler points it at the current task's own kernel
  stack on each switch) so a blocking syscall (`wait`) can yield mid-call without corrupting
  another process's frame on a shared stack. Still single-CPU: real SMP needs a *per-CPU* stack
  selected via `swapgs` + `GS` base. Also the per-process kernel stack is now shared by ring-3
  interrupts *and* syscalls (mutually exclusive in time on one core — IF=0 syscalls don't nest
  with ring-3 interrupts — but a tighter design might separate them).
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
- **User-fault termination is coarse, and only covers ring-3 *code* faults (M5c3b → improved
  M6f5).** A #PF/#GP taken while executing in ring 3 kills *the process* (not the kernel); since
  M6f5 it terminates via `exit_current_killed(SIGSEGV)`, so the status is a `wait`-able
  `WIFSIGNALED(SIGSEGV)` rather than a hardcoded code 139. Still missing: signal *handlers* (a
  process can't catch `SIGSEGV`), faulting-instruction/address reporting to the process, and core
  dumps — just terminate + a serial line. (The separate hole — a *kernel* fault on a user pointer
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
- **FAT is FAT32-only, 8.3-only, no LFN/cache (M6c; write M6g2; subdirs M6g4).** `fs::fat` reads
  and writes (`write_file` creates/overwrites) and now resolves **subdirectory paths** + `mkdir`,
  but: only FAT32 (rejects FAT12/16), 512-byte sectors, short **8.3** names only (LFN entries
  skipped, not assembled — so long names are invisible), whole-file in a `Vec` (no
  seek/streaming/partial I/O). Path resolution is **forward-only**: `.`/`..` components aren't
  interpreted (they're matched literally as 8.3 names — `..` happens to resolve via the on-disk
  `..` entry, but `.`/relative paths and `mkdir -p`-style auto-create of parents are not handled).
  It re-reads the BPB on every `mount()` and re-reads FAT/dir sectors per call with **no
  caching** — O(sectors) per lookup. A buffer cache + LFN come later.
- **No directory growth or removal (M6g2/M6g4).** A directory occupies the cluster(s) it has; if
  its existing slots fill up, creating another entry returns `DirFull` (no dir-cluster extension).
  A new `mkdir` directory is one cluster (`.`/`..` + room for entries until it fills). There is no
  `rmdir`/`unlink` (no removal of files or dirs), and `mkdir` requires the parent to already exist.
- **`getdents64` snapshots the listing at `open` (M6g5).** Opening a directory serializes its
  whole entry list into the fd's buffer once; a file created/removed afterward by another process
  won't appear/disappear in an already-open dir fd. `d_ino` is a pseudo value (entry index, not a
  real inode), `d_off` is a byte cursor (no `lseek`-on-a-dir support), and listings include the
  `.`/`..` entries for subdirectories (the FAT32 root has none). LFN entries are skipped, so
  long-named files are invisible to `ls`.
- **FAT write is whole-file, not crash-safe, no dir growth / delete (M6g2).** `write_file`
  replaces a file's entire contents (no append/random-write/truncate-to-size); there's no
  `unlink`/delete and no directory **extension** — if the root dir's existing clusters have no
  free 32-byte slot, it returns `DirFull` instead of allocating another dir cluster. Free-cluster
  search is a linear FAT scan from cluster 2 with **no FSInfo / next-free hint** (O(FAT) per
  allocation; a multi-cluster file rescans from the start each cluster). The ordering is
  *failure-safe within the API* (build new chain + data → commit the dir entry → only then free
  the old chain; a `NoSpace`/device error before commit rolls back the new chain and leaves the
  existing file intact), but it is **not crash-consistent across power loss**: the commit is a
  single non-barriered sector write and the post-commit free of the old chain is a separate write,
  so a crash at the wrong moment can still leak clusters (and, on real hardware without a flush
  barrier, reorder). Because new clusters are allocated while the old chain is still live, an
  overwrite needs room for both copies at once (can `NoSpace` even when in-place would fit).
  Directory timestamps are written as zero. A real FS needs ordered writes / journaling, an
  allocation cursor, and dir growth.
- **FAT reader trusts a well-formed image (M6c).** It now bounds cluster numbers to the
  volume (`valid_cluster`, prevents sector-address overflow / wild reads) and caps chain
  traversal (prevents a cyclic-chain hang), but it still **trusts the directory's file size**:
  if the cluster chain is shorter than `size`, `read_file` returns a silently *truncated*
  buffer rather than an error. Like the ELF loader (D10), this is defensive-but-not-complete;
  a fuller reader would cross-check size vs chain length and surface mismatches.
- **virtio-blk is polled and single-request (M6b; write added M6g1).** The driver suppresses
  the device interrupt (`VIRTQ_AVAIL_F_NO_INTERRUPT`) and busy-polls the used ring — no IRQ
  handler, so a `read_sector`/`write_sector` blocks the caller (and, under the global `DEVICE`
  Mutex, everyone) until the device replies; there's also no poll timeout, so a wedged device
  hangs the kernel. Only one descriptor chain (desc 0..2) is reused, so there's no queue depth /
  async I/O, and both directions bounce through the single DMA page (an extra copy) one 512-byte
  sector per request. **Write (M6g1)** has no `VIRTIO_BLK_T_FLUSH`/barrier/FUA — a completed
  `write_sector` means the device accepted it, not that it's durable on real hardware; and there's
  no multi-sector write. Interrupt-driven, multi-request, cached, flush-aware I/O (a real block
  layer) is a later pass (needs MSI/INTx handling).
- **Accept-zero-features negotiation (M6b; still true after M6g1 write).** The driver writes
  Driver Features = 0 and doesn't inspect Device Features — basic legacy read *and write* work
  without feature bits, but it never checks `VIRTIO_BLK_F_RO` (so a write to a read-only-exported
  disk would just fail at the status byte), block-size, or geometry. Real negotiation is still
  deferred.
- **`virtio_blk::init` is bound to `BootInfoFrameAllocator` (M6b).** It needs
  `allocate_contiguous` (not on the `FrameAllocator` trait), so it takes the concrete
  allocator. The virtqueue/buffer frames are allocated once and never freed (M3 item). A
  trait for contiguous/DMA allocation would decouple it.

- **File syscalls are minimal; write is buffered write-back (M6d2; write M6g3; fd model M7g1).**
  `open`/`read`/`write`/`close`/`lseek`/`dup2` exist. `open` reads the **whole file into memory** (no
  streaming/`mmap`, no large-file support) and honors `O_RDONLY`/`O_WRONLY`/`O_RDWR`/`O_CREAT`/
  `O_TRUNC`/`O_APPEND` (no `O_EXCL`/…, `mode` ignored). **Write is write-back**: `write` mutates the
  in-memory buffer and the buffer is flushed via `fs::write_file` on `close` OR synchronously on a
  normal `exit` (M7g1 `flush_current_process`). A process killed by a signal/fault loses its
  unflushed writes (no `fsync`, no journaling). `write` past EOF zero-fills the gap.
  **`O_APPEND` is open-time only** — the offset is set to EOF at `open`, NOT before each `write`, so
  it isn't true atomic append (a seek-then-write or a second appender would overwrite); fine for the
  shell's sequential `cmd >> file`, wrong for general use.
  **fd backings (M7g1):** fds 0/1/2 are real table entries (`Console`/`Vga`/`Serial`) so they can be
  redirected via `dup2`; `dup2` **copies** the backing rather than sharing one open-file description
  (real Unix shares offset/buffer). Same copy semantics on `fork` (M6f3). Consequences: two dirty
  copies of the same file (via `fork` or `dup2`) each flush independently → **last-writer-wins / lost
  update**; and `dup2` over a dirty writable `newfd` drops its unflushed buffer silently. The shell
  redirect flow (open→`dup2`→close, only the child writes) avoids these, but direct `dup2`/`fork`
  use with writable files hits them. `unlink`/`rmdir` exist (M7g3) but are minimal: `rmdir` only
  removes an **empty** dir (no recursive `rm -r`), deletion marks the dir entry `0xE5` then frees
  the cluster chain (no LFN-chain cleanup since we don't write LFN). There is **no reference
  counting / delete-on-last-close**: unlinking a file a process has open for writing removes the
  entry, but the open fd keeps its in-memory copy AND **`close`/exit flushes it back via
  `write_file`, re-creating the file on disk** (a "resurrection"). Not reachable from the shell
  (`rm` holds no fd), but a real hazard for a program that unlinks its own open file; a proper
  open-file/inode model with link counts is the fix. Still **no** `stat`/`rename`/`truncate`, no LFN (root + 8.3 only),
  paths capped at 256 bytes. A real file model (streaming, shared open-file table, atomic append, a
  proper VFS with mounts/inodes) is later work.
- **Pipes are minimal (M7g2).** `pipe(2)` + read/write/`dup2` work and the shell runs `a | b`, but:
  the pipe buffer is **unbounded** (writes never block / apply no backpressure — a fast producer
  into a slow/stalled consumer grows kernel memory without limit); a write with no readers returns
  `-EPIPE` instead of raising **SIGPIPE** (there's no signal); the shell supports only a **single**
  `|` (two commands, no `a | b | c`) and **doesn't pipe builtins** (`pwd | …` runs pwd to the
  terminal). The shell waits the left child before the right — safe ONLY because writes never
  block; **if the buffer is ever bounded, this must change** (a left producer that fills the pipe
  while the parent blocks in `wait4(left)` and never runs the right consumer would deadlock — wait
  on `-1`/both instead). A bounded ring buffer with blocking writes, SIGPIPE, and multi-stage
  pipelines are later work. Lock-order note: a pipe end's `Drop` takes the `PipeBuf` lock while the
  `PROCESSES` lock is held (close/dup2/exit), whereas `read`/`write` take `PipeBuf` without
  `PROCESSES` — harmless on the single core (IF=0 syscalls, no reentrancy) but an inconsistency to
  resolve before SMP (see the spinlock note below).
- **Per-process fd table keyed by CR3 (M6d2; leak fixed M6e3).** `syscall::files` stores each
  process's open files in a `BTreeMap` keyed by its PML4 physical address (avoids touching the
  scheduler). Now that frames are recycled (M6e1), the reaper **must** drop the entry on exit
  (`forget_process`) — done (M6e3) — else a recycled PML4 would inherit a dead process's fds.
  The CR3 key is still a stand-in for a real PID/process table (Phase 2a).
- **A syscall holds the process-files lock across copy-to-user (M6d2).** `sys_read` keeps the
  global `PROCESSES` `Mutex` while it `copy_to_user`s the data. Safe on single-CPU (syscalls
  run `IF=0`, no ISR touches the map), but on SMP this serialises all file I/O and a blocking
  copy under the lock would be bad. Pairs with the non-preemptible-syscall limitation.

- **`fork` copies every page eagerly — no copy-on-write (M6f3).** `AddressSpace::fork_from`
  allocates a fresh frame and `memcpy`s the contents of *every* user page of the parent. For a
  large process this is slow and wasteful (the common case is fork-then-exec, which throws the
  copy away). COW (share pages read-only, duplicate on write-fault) is the standard fix — needs a
  write-fault handler and per-frame refcounts. Deferred.
- **`fork`/`execve` panic on frame exhaustion instead of returning `-ENOMEM` (M6f3).** Building
  the child/new address space goes through `new_sharing_kernel` / `map_to` / `fork_from`, all of
  which `.expect()` on `allocate_frame` — like the rest of the kernel's allocators. Since fork/exec
  are user-triggerable, OOM there *should* surface as `-ENOMEM` to the caller, not bring down the
  kernel. Needs allocation-failure plumbing (the `None` arm already handles "allocator not
  installed"; this is the genuine-exhaustion arm).
- **`fork` is the whole `clone` surface (M6f3).** No `clone`/threads (`CLONE_VM` etc.), no
  `vfork`, no `argv`/`envp` to the child beyond what's already in its copied memory.

- **`wait4` is minimal (M6f4).** Supports `pid == -1` (any child) and `pid > 0` (specific);
  `options` (no `WNOHANG`/`WUNTRACED`), `rusage`, and process groups (`pid == 0` / `pid < -1`)
  are ignored/unsupported. Status encodes only normal `exit` (`(code & 0xff) << 8`) — no
  signal-termination encoding yet (comes with M6f5). The blocking `wait` busy-cycles through the
  scheduler (parent `Blocked`, woken by the child's `exit`) — correct, but there's no wait-queue;
  a process waiting on many children rescans the whole thread table each wakeup.
- **Orphan zombies are reparented to the kernel (PID 0), not a real `init` (M6f4).** When a
  process exits, its children are reparented to PID 0 and the reaper collects PID-0 zombies
  (discarding their status). A proper `init` (PID 1) that adopts orphans and `wait`s them comes
  with M7. Also: a process that `fork`s but never `wait`s leaks its child as a zombie only until
  it itself exits (then the child is reparented + reaped) — fine for now, but a long-lived
  non-waiting parent accumulates zombies.

- **PID 1 (the shell) exiting is a graceful dead-end, not respawn (M7d).** The shell is launched
  as PID 1 and can `exit` like any process. The kernel does NOT panic (the always-runnable PID-0
  zero thread keeps the scheduler alive, so the system just idles on `hlt`), but there is no
  init-respawn policy: once the only shell exits there is no way back without a reboot. A real
  `init` should never exit — either loop forever, or re-`fork`/`exec` a fresh shell when its child
  dies (login-getty style). Deferred until init becomes a separate process from the shell.

- **Signals are terminate-only — no handlers (M6f5).** `kill` + the per-process pending-signal
  set exist, and default-terminate actions are applied, but there is NO user-handler delivery:
  no `sigaction`/`signal` to register a handler, no signal frames pushed on the user stack, no
  `sigreturn`, no signal masking (`sigprocmask`), no real-time/queued signals. The
  `pending_signals` bitmask is recorded but only consulted for the immediate default action;
  pending non-terminating signals just sit there. Full handler delivery is the next signals
  stretch.
- **`kill` applies the default action synchronously from the killer's context (M6f5).** Because
  there are no handlers, terminating a target is just editing its scheduler entry (it isn't on a
  CPU — single core), so `kill` does it inline rather than making the victim run signal-handling
  code at its next return-to-userspace. Once handlers exist, delivery must move to the victim's
  return-to-ring-3 path (and a blocked `wait` must become interruptible by a signal — today a
  process blocked in `wait` is woken only by a child exit, not by a signal). No process groups
  (`kill(pid <= 0)` is `-ESRCH`), and `SIGSTOP`/job-control stop actions are unimplemented
  (treated as no-op rather than stopping the process).

## M9 — POSIX / libc

- **`brk` heap is a fixed region, no `mmap` (M9a).** The process heap is a fixed window
  `[USER_HEAP_BASE, USER_HEAP_MAX)` (1 GiB) that grows up by mapping pages on demand. There is
  **no `mmap`/`munmap`** (anonymous or file-backed), so large/aligned allocations and
  memory-mapped files aren't possible yet — a real `malloc` arena beyond 1 GiB, or one that
  prefers `mmap`, would hit the wall. The heap also can't grow *toward* the stack; it's one
  bounded slab.
- **No heap guard page (M9a).** Nothing unmapped sits between the heap top and higher addresses,
  so a ring-3 overrun past `brk` just faults if unmapped or silently runs into the next mapping if
  something is ever placed above. Add a guard page when the user VA layout grows.
- **`brk` shrink frees leaf frames but leaks intermediate tables (M9a).** Lowering the break
  unmaps pages and returns their frames to the allocator, but the L1/L2/L3 page tables that held
  them stay allocated until the address space is torn down on `exit`. Fine for the typical
  grow-mostly malloc pattern; a workload that repeatedly grows and shrinks a large heap would
  accrete page-table frames.
- **`writev`/`fcntl` are minimal (M9f).** `writev`/`readv` loop the existing per-buffer
  `write`/`read` (no single atomic vectored transfer — between two iovecs another writer could
  interleave on a pipe; fine single-threaded). `fcntl` `F_DUPFD` copies the fd backing like `dup2`
  (independent file buffer/offset, not a shared open-file description — POSIX `dup` shares it).
  `F_SETFL`/`F_SETFD` are accepted but **ignored**: no `O_NONBLOCK` (all I/O stays blocking) and no
  close-on-exec (an fd is never closed across `execve` — `FD_CLOEXEC` is a no-op). `F_GETFL` reports
  only the access mode and **can't distinguish `O_WRONLY` from `O_RDWR`** (an open file tracks just a
  `writable` bit, not the full access mode, so a write-only fd reads back as `O_RDWR`); status flags
  aren't reported. Other `fcntl` commands (locks, `F_SETOWN`, …) → `-EINVAL`.
- **FPU/SSE context save is eager and SSE-only, not AVX (M9h SSE + M9i save).** `arch::enable_sse`
  turns on SSE (CR0/CR4) so clang-vectorized C/libc code runs in ring 3, and `switch_task` now
  `fxsave`/`fxrstor`s a per-thread 512-byte FPU area on **every** context switch (M9i) — so XMM/MXCSR/
  x87 no longer leak between processes. Remaining gaps: (1) it's **eager**, not lazy (`CR0.TS`
  on-demand), so every switch pays the `fxsave`+`fxrstor` even for soft-float kernel threads; (2)
  `fxsave` covers x87+SSE but **not AVX/YMM/ZMM** — once a program uses AVX, those upper bits would
  leak (need `xsave` + a larger area); (3) no `#XM`/`#MF` handlers yet (SIMD/x87 exceptions are
  masked by default in MXCSR, so they don't fire for ordinary code).
- **The bundled libc is minimal (M9h, M9j).** `user/c/libc` provides `crt0` + `write`/`read`/`exit`,
  `malloc`/`free` (a **bump allocator over `brk` — `free` never reclaims**),
  `memset`/`memcpy`/`strlen`/`strcmp`/`putchar`/`puts`, and `printf`/`snprintf` (M9j). The `printf`
  engine handles `%d %i %u %x %X %p %s %c %%` + the `l` length modifier only — **no width/precision/
  flags (`%-5.2f`), no floating point, no locale** (those are the "genuinely complex" parts that per
  D13 we grow on demand or take from a real libc). Still no `errno`, no full stdio (`FILE*`,
  buffering), no threads. It's a foundation to grow, or to replace with a real libc port (relibc)
  once buildable.
- **No users/groups; `uname` is fixed strings (M9e).** `getuid`/`geteuid`/`getgid`/`getegid` all
  return 0 — there is no user/group model, no `setuid`/credentials, no permission enforcement
  (every process is effectively root). `getppid` is real (the thread's parent PID). `uname` returns
  hard-coded fields (`sysname`=ferros, `nodename`=ferros, `machine`=x86_64, …) — no real hostname
  (`sethostname`/`gethostname`) or domain. Fine for single-user bring-up; a real multi-user model is
  far-future.
- **Time is uptime, not wall-clock; tick-coarse (M9d).** `clock_gettime`/`gettimeofday`/`time`
  derive from the PIT tick counter (uptime since boot), so **`CLOCK_REALTIME` is not real wall-clock
  time** — it starts at 0 at boot, not the Unix epoch (there's no RTC read yet; reading the CMOS RTC
  at boot for a real epoch is the fix). `CLOCK_MONOTONIC` == `CLOCK_REALTIME` (same source).
  Resolution is one PIT tick (~54.9 ms) — time is a staircase, not smooth, so sub-tick intervals
  read as zero. No `CLOCK_PROCESS_CPUTIME_ID`/per-thread clocks, no `clock_getres`, no `settimeofday`/
  `clock_settime`. The PIT divisor is the power-on default (~18.2 Hz); a finer tick (e.g. 1 kHz) or a
  TSC/HPET time source would improve resolution.
- **`stat` timestamps stay zero even now that a clock exists (M9d).** `build_stat` still writes 0 for
  `st_atime`/`mtime`/`ctime`: the clock gives "now", but a file's stored mtime (which FAT keeps and we
  don't read) is the correct value, and stamping every `stat` with the current time would be wrong.
  Reading FAT's date/time fields is the real fix.
- **`stat` is a thin synthesis, not real metadata (M9c).** Timestamps (`st_atime`/`mtime`/`ctime`)
  are all 0 — we have no clock yet (M9d); FAT does store a mtime we don't read. `st_ino` is the
  file's first cluster, which is **0 for empty files** (so not unique, and `stat`/`fstat` can
  disagree — `fstat` reports `st_ino=0` always since the open-file table doesn't keep the cluster).
  `st_uid`/`st_gid`/`st_rdev` are 0, `st_mode` permission bits are a fixed 0644/0755 (no real
  permissions). `lstat` == `stat` (no symlinks). `fstat` on a regular file reports the in-memory
  buffer length, not the on-disk size (they differ for an unflushed dirty file). `st_nlink` is a
  fixed 1/2. Good enough for "does it exist / how big / is it a tty / a dir", not for tooling that
  relies on times, inodes, or link counts.
- **`arch_prctl` is FS-only, no GS, single-thread TLS (M9b).** `ARCH_SET_FS`/`ARCH_GET_FS` work;
  `ARCH_SET_GS`/`ARCH_GET_GS` return `-EINVAL` (the kernel will want GS for per-CPU once SMP lands,
  so user GS-base needs care then). There's one thread per process, so "thread-local" is really
  "process-local" for now; real `pthread`-style multithreading (multiple FS bases within one
  address space) is later. The FS base is validated to be a canonical user address
  (`< USER_SPACE_END`) rather than allowing the full non-canonical range Linux's `wrmsr` would
  `#GP` on.
- **FS base is reloaded on every context switch (M9b, perf).** `switch_task` writes `IA32_FS_BASE`
  unconditionally (including 0 for kernel threads), so every switch pays a `wrmsr` (~tens–hundreds
  of cycles) even when the base is unchanged. Cache the live base (or skip kernel→kernel switches)
  to write only on change. Correctness-first for now.
- **Only the `brk` path zeroes user pages; ELF/stack mapping does not (M9a).** `brk` growth zeroes
  every page it hands out (`map_active_user_page`), matching Linux and closing the cross-process
  info leak from frame reuse (M6e1). But `map_user_page` (ELF segments, the user stack) still does
  **not** zero its frames — BSS and fresh stack pages rely on frames happening to be zero, which a
  recycled frame isn't. That's a pre-existing correctness+security gap (stale bytes in BSS/stack);
  zeroing should move into the shared user-page mapping path kernel-wide.

## Cross-cutting (whole kernel)

- **No real-hardware validation** — QEMU only until M11 (UEFI + real drivers).
- **Spinlocks everywhere** — fine single-CPU; revisit lock strategy when SMP arrives.
