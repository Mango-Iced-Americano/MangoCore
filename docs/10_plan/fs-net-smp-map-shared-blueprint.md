# MAP_SHARED ↔ PageCache dirty/writeback: Structural Blueprint

## Executive Summary

**Three decisions, directly answering the three blockers:**

1. **Rmap**: Do NOT build a per-page `Weak<AddressSpace>` binding list. Mirror **DragonOS's `FileVmaIndex`**: a per-PageCache registry of `Weak<LockedVMA>` keyed by VMA id, where the `Weak<AddressSpace>` lives **inside the VMA** (`vma.user_address_space: Option<Weak<AddressSpace>>`), not in the fault context. Registration happens at **mmap/fork/unmap time**, never in the fault handler.
2. **Fault retry**: The fault handler must return a **retry token** (`Arc<dyn RetryWait>`), and the trap/uaccess loop must scope the `AddressSpace::write()` guard so it is **released before `wait()`**. DragonOS does exactly this; it is the *only* way to break the writeback↔fault lock inversion with spin locks.
3. **Writeback write-protect**: `clear-dirty-for-io` (Dirty→Writeback CAS) must be preceded by an rmap walk that **write-protects + clears-dirty on every PTE**, under the per-MM VM lock, with TLB work collected into the existing `MmuGather` and flushed **after** unlock.

The key lock inversion you must design around (this is Linux's `i_mmap_rwsem`/`invalidate_lock` dance and DragonOS's `PageCacheFaultInvalidateRead`):

```
writeback:  invalidate_read → rmap walk → mm read/VM lock → PTE wrprotect
fault:      VM write lock → wants invalidate_read  ❌ DEADLOCK
solution:   fault does try_invalidate_read; on failure returns Retry token;
            outer loop drops VM lock, then waits (mirrors VM_FAULT_RETRY)
```

---

## 1. Linux 6.6 file-page reverse mapping

### 1.1 Data structures: page-anchored rmap, not VMA-anchored

Linux's rmap is **anchored on the page**, not on the VMA:

- `struct page` carries `struct address_space *mapping` (union with `slab_list`/`lru` etc.) and `pgoff_t index` (the file offset of the page) — [include/linux/mm_types.h:310-316](https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/include/linux/mm_types.h#L310-L316). `page_index()`/`page_mapping()` recover the file offset and its address_space at any time.
- `atomic_t _mapcount` on `struct page` (mm_types.h:316) — number of PTE mappings, decremented in `page_remove_rmap` ([mm/rmap.c:1410](https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/mm/rmap.c#L1410)). For file pages this is the "mapped count" used to decide `folio_mapped()`, eviction eligibility, etc. No per-page list of mappings is kept — the *interval tree gives the VMAs*, and the page's `index` gives the address within each VMA.
- The address_space holds the VMA interval tree:

```c
// include/linux/fs.h:470-487 (v6.6)
struct address_space {
    ...
    atomic_t        i_mmap_writable;   // number of VM_SHARED VMAs
    struct rb_root_cached i_mmap;      // interval tree of VMAs keyed by [pgoff, pgoff+len)
    struct rw_semaphore i_mmap_rwsem;  // read: traverse; write: insert/remove VMA
    ...
};
```

[include/linux/fs.h:470-487](https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/include/linux/fs.h#L470-L487)

The tree is populated at mmap time — `vma_link()` takes `i_mmap_lock_write()` then `vma_interval_tree_insert(vma, &mapping->i_mmap)` ([mm/mmap.c:395-418](https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/mm/mmap.c#L395-L418), insert at line 391). **This is the key structural answer: reverse mapping is built eagerly at mmap/fork, and the per-page rmap entry (`page_add_file_rmap`, a mapcount bump) is built lazily at fault time** — and it never needs a `Weak<AddressSpace>` because it is *page → address_space* via `page->mapping`, and *address_space → VMA* via the tree. The page pins the mapping via the page lock (see §1.3).

### 1.2 The mkclean walk: page_mkclean_one → folio_mkclean

`folio_mkclean()` is the writeback write-protect engine ([mm/rmap.c:1016-1038](https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/mm/rmap.c#L1016-L1038)):

```c
int folio_mkclean(struct folio *folio)
{
    int cleaned = 0;
    struct rmap_walk_control rwc = {
        .arg = (void *)&cleaned,
        .rmap_one = page_mkclean_one,
        .invalid_vma = invalid_mkclean_vma,   // skips non-VM_SHARED
    };
    BUG_ON(!folio_test_locked(folio));        // ← page lock is the anchor
    if (!folio_mapped(folio))                 // ← _mapcount fast path
        return 0;
    mapping = folio_mapping(folio);           // ← page->mapping
    if (!mapping)
        return 0;
    rmap_walk(folio, &rwc);
    return cleaned;
}
```

`rmap_walk_file` ([mm/rmap.c:2461-2490](https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/mm/rmap.c#L2461-L2490)):

- Requires `folio_test_locked(folio)` — **the page lock protects `page->mapping` from being NULLed by truncate** (comment at 2468-2473: "The page lock not only makes sure that page->mapping cannot suddenly be NULLified by truncation, it makes sure that the structure at mapping cannot be freed and reused yet, so we can safely take mapping->i_mmap_rwsem").
- Takes `i_mmap_lock_read(mapping)` (or `trylock` when `rwc->try_lock`), then interval-tree-iterates VMAs whose `[pgoff, pgoff+nr_pages)` intersects the page ([mm/rmap.c:2479-2490](https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/mm/rmap.c#L2479-L2490)).
- For each VMA, `page_mkclean_one` (rmap.c:997) walks PTEs via `page_vma_mapped_walk`.

The per-PTE write-protect ([mm/rmap.c:935-995](https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/mm/rmap.c#L935-L995)):

```c
static int page_vma_mkclean_one(struct page_vma_mapped_walk *pvmw)
{
    ...
    while (page_vma_mapped_walk(pvmw)) {
        address = pvmw->address;
        if (pvmw->pte) {
            pte_t entry = ptep_get(pte);
            if (!pte_dirty(entry) && !pte_write(entry))
                continue;                       // already clean+RO
            flush_cache_page(vma, address, pte_pfn(entry));
            entry = ptep_clear_flush(vma, address, pte);   // ← TLB flush happens HERE
            entry = pte_wrprotect(entry);
            entry = pte_mkclean(entry);
            set_pte_at(vma->vm_mm, address, pte, entry);
            ret = 1;
        }
        ...
    }
}
```

### 1.3 The exact lock dance (write-protect vs TLB flush)

This is the part you asked to nail down precisely:

| Step | Lock held | Action |
|------|-----------|--------|
| 1 | **page lock** (from writeback) | anchor: `page->mapping` cannot be freed/NULLed; pins address_space |
| 2 | **`i_mmap_rwsem` (read)** | interval-tree iteration finds candidate VMAs |
| 3 | **`mmap_read_lock` (read)** + **PTL** | `page_vma_mapped_walk` acquires the per-VMA-mapping's `mmap_read_lock` and per-PTE-table lock; reads PTE, `ptep_clear_flush`, `pte_wrprotect`, `pte_mkclean`, `set_pte_at` |
| 4 | — | `ptep_clear_flush` **flushes the TLB while PTL is still held** (safe: TLB shootdown for this VMA is complete before the new PTE is installed; concurrent faulters can't install a dirty PTE because the PTE is momentarily non-present and the page lock excludes `filemap_page_mkwrite`) |

**The subtlety**: Linux flushes TLB *inside* the PTE lock (via `ptep_clear_flush`), not after. It can do this because `mmap_read_lock` + PTL fully serialize PTE mutation, and the page lock serializes against the fault's mkwrite. When you adapt to MangoCore's discipline ("collect under lock → seal → unlock → `TlbFlush::execute`"), the *functional equivalent* is: PTE wrprotect + TLB record under the VM lock; execute the flush after unlock. The invariant you must preserve is **no window where a PTE is writable but the flush hasn't happened** — because the PTE *itself* is the synchronization point, not the TLB. Since the new PTE is `!write && !dirty` from the moment it is installed, a stale TLB entry can only cause the next write to fault again (benign), never to bypass write-protect.

### 1.4 Invocation from writeback: `clear_page_dirty_for_io` → `folio_mkclean`

[mm/page-writeback.c:2859-2909](https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/mm/page-writeback.c#L2859-L2909) — `folio_clear_dirty_for_io()`:

```c
if (folio_mkclean(folio))            // ← walk PTEs; if ANY was dirty → returns >0
    folio_mark_dirty(folio);         //    re-mark the folio dirty (see "insane" comment)
...
wb = unlocked_inode_to_wb_begin(inode, &cookie);
if (folio_test_clear_dirty(folio)) { ... }   // ← now clear the folio dirty bit
```

The famous "Yes, Virginia, this is indeed insane" comment (2871-2895) explains the ordering: `folio_mkclean` write-protects PTEs; if any PTE was dirty, the *folio is marked dirty again* so the subsequent `folio_test_clear_dirty` sees it; **then** the folio dirty bit is cleared for I/O. The comment at 2898-2904 states the exclusion contract:

> "We carefully synchronise fault handlers against installing a dirty pte and marking the folio dirty at this point. We do this by having them hold the page lock while dirtying the folio, and folios are always locked coming in here, so we get the desired exclusion."

So the **page lock** (in MangoCore: the `PageEntry.data` RwLock or an equivalent per-entry lock) is the *linearization point* between fault-mark-dirty and writeback-clear-dirty. Your `PG_REDIRTIED` mechanism (page_cache.rs:848-857, 1678-1684) is exactly the right successor for "redirtied during writeback" — but note Linux never needs PG_REDIRTIED *for correctness of the PTE path* because the fault holds the page lock while restoring the write bit AND setting dirty; the writeback sees the page still dirty and keeps it in the dirty set. Keep your PG_REDIRTIED for the race where the fault completes *after* the folio-dirty clear but *before* Writeback→UpToDate; that is the one window Linux also handles (via the page lock held across both, plus stable-page machinery).

### 1.5 Truncate: two passes, unmap first, then wait for writeback

[mm/truncate.c:172-187](https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/mm/truncate.c#L172-L187) — `truncate_cleanup_folio`:

```c
static void truncate_cleanup_folio(struct folio *folio)
{
    if (folio_mapped(folio))
        unmap_mapping_folio(folio);          // ← rmap walk, unmap/wrprotect PTEs
    if (folio_has_private(folio))
        folio_invalidate(folio, 0, folio_size(folio));
    folio_cancel_dirty(folio);
    folio_clear_mappedtodisk(folio);
}
```

`truncate_inode_pages_range` ([mm/truncate.c:329-429](https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/mm/truncate.c#L329-L429)) is explicitly **two-pass**: pass 1 (363-373) is non-blocking — `find_lock_entries` + `truncate_cleanup_folio` + `delete_from_page_cache_batch`, which removes most pages without waiting on writeback; pass 2 (400-428) **waits** — `folio_lock` + `folio_wait_writeback` (line 423) before `truncate_inode_folio`. The comment at 315-319: "The first pass is nonblocking... The second pass will wait. This is to prevent as much IO as possible in the affected region."

`unmap_mapping_folio` is the same rmap machinery as mkclean (walks `i_mmap` under `i_mmap_rwsem` read + mm locks, unmaps/wrprotects PTEs), used with `even_cows` semantics from `unmap_mapping_range` ([mm/memory.c:3565-3580](https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/mm/memory.c#L3565-L3580)). Filesystems call it from `truncate_pagecache` (mm/truncate.c:740, 853).

---

## 2. Linux 6.6 fault retry

### 2.1 The flag/return contract

[include/linux/mm.h:462-482](https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/include/linux/mm.h#L462-L482):

```c
#define FAULT_FLAG_DEFAULT  (FAULT_FLAG_ALLOW_RETRY | \
                             FAULT_FLAG_KILLABLE)
static inline bool fault_flag_allow_retry_first(unsigned int flags)
{
    return (flags & FAULT_FLAG_ALLOW_RETRY) &&
        (!(flags & FAULT_FLAG_TRIED));
}
```

- **`FAULT_FLAG_ALLOW_RETRY`**: the fault is allowed to drop `mmap_lock` and return `VM_FAULT_RETRY` instead of sleeping while holding it.
- **`FAULT_FLAG_TRIED`**: set on re-entry after a retry; a second retry may still happen, but *synchronous* operations (like `__folio_lock`) become mandatory — the fault *must not* drop the lock again without making progress, and "allow retry first" becomes false (so `lock_folio_maybe_drop_mmap` will do `__folio_lock` directly rather than drop + wait, filemap.c:3312 vs 3088).
- **`FAULT_FLAG_RETRY_NOWAIT`**: the kernel-uaccess flavor — on contention, return `VM_FAULT_RETRY` **without dropping `mmap_lock`** (the lock stays held; the caller re-checks). This is the `copy_to/from_user`-in-syscall mode where returning to user is impossible.
- **`VM_FAULT_RETRY`**: the mmap_lock **may have been dropped** — the top-level fault handler must re-acquire it and re-find the VMA. This is the crux: *returning VM_FAULT_RETRY means "the lock is no longer held, restart the fault from the top."* (`handle_mm_fault` doc at mm/memory.c:5027-5029, 5250-5253.)

### 2.2 Where the lock is dropped: `__folio_lock_or_retry` / `lock_folio_maybe_drop_mmap`

[mm/filemap.c:1678-1710](https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/mm/filemap.c#L1678-L1710) — `__folio_lock_or_retry`:

```c
vm_fault_t __folio_lock_or_retry(struct folio *folio, struct vm_fault *vmf)
{
    if (fault_flag_allow_retry_first(flags)) {
        if (flags & FAULT_FLAG_RETRY_NOWAIT)
            return VM_FAULT_RETRY;          // lock NOT released (kernel uaccess)
        release_fault_lock(vmf);            // ← mmap_read_unlock here
        folio_wait_locked(folio);           // ← sleep (not holding mmap_lock)
        return VM_FAULT_RETRY;
    }
    ...
    __folio_lock(folio);                    // ← TRIED: sleep while holding lock
    return 0;
}
```

`lock_folio_maybe_drop_mmap` (mm/filemap.c:3088-3132) is the filemap_fault wrapper: `folio_trylock` first; on failure, if `FAULT_FLAG_RETRY_NOWAIT` → return 0 (VM_FAULT_RETRY, lock held); else `maybe_unlock_mmap_for_io` (drops mmap_lock, pins the file) then `__folio_lock` (sleeps) and returns 1 → filemap_fault sees it dropped the lock and goes to `out_retry` (3387-3399) → returns `VM_FAULT_RETRY`.

`filemap_fault` contract, [mm/filemap.c:3243-3253](https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/mm/filemap.c#L3243-L3253): "If our return value has VM_FAULT_RETRY set, it's because the mmap_lock may be dropped before doing I/O or by lock_folio_maybe_drop_mmap()."

### 2.3 The arch re-entry loop (RISC-V)

[arch/riscv/mm/fault.c:313-366](https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/arch/riscv/mm/fault.c#L313-L366):

```c
retry:
    vma = lock_mm_and_find_vma(mm, addr, regs);    // takes mmap_read_lock
    ...
    fault = handle_mm_fault(vma, addr, flags, regs);
    if (fault_signal_pending(fault, regs)) { ... }
    if (fault & VM_FAULT_COMPLETED)
        return;                                    // fault fully self-completed
    if (unlikely(fault & VM_FAULT_RETRY)) {
        flags |= FAULT_FLAG_TRIED;                 // ← prevent lock-drop loop
        /* mmap_read_unlock already done inside __lock_page_or_retry */
        goto retry;                                // ← re-find VMA, re-enter
    }
    mmap_read_unlock(mm);
```

Two entry flavors at the top (fault.c:286-312): user faults try the fast `lock_vma_under_rcu` path; if that returns `VM_FAULT_RETRY` it falls into `lock_mm_and_find_vma` (full lock). Kernel-mode faults (`!FAULT_FLAG_USER`) skip the VMA-lock fast path and go straight to `lock_mm_and_find_vma` (fault.c:286-287).

### 2.4 Where the PTE write bit is restored after writeback: `filemap_page_mkwrite`

The fault path for a shared file write: `handle_pte_fault` → `do_fault` ([mm/memory.c:4678-4720](https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/mm/memory.c#L4678-L4720)) → `do_shared_fault` (4627-4668) → `__do_fault` (filemap_fault → page in cache, locked) → `do_page_mkwrite` (2923-2949, sets `FAULT_FLAG_MKWRITE`) → `filemap_page_mkwrite` ([mm/filemap.c:3622-3646](https://github.com/torvalds/linux/blob/ffc253263a1375a65fa6c9f62a893e9767fbebfa/mm/filemap.c#L3622-L3646)):

```c
vm_fault_t filemap_page_mkwrite(struct vm_fault *vmf)
{
    ...
    folio_lock(folio);
    if (folio->mapping != mapping) { ... VM_FAULT_NOPAGE; }  // truncate raced
    folio_mark_dirty(folio);          // ← mark dirty UNDER the page lock
    folio_wait_stable(folio);         // ← wait for writeback to finish
    ...
}
```

Then `finish_fault` → `do_set_pte` installs the PTE with `pte_mkdirty` + `maybe_mkwrite` (restoring W) — e.g. mm/memory.c:3991 for the swap-in path and the same idiom at 713/3029/3135/3991 (`maybe_mkwrite(pte_mkdirty(pte), vma)`). The mkwrite order matters: **`folio_mark_dirty` happens *before* `folio_wait_stable`** so a concurrent freeze/writeback sees the dirty flag and re-wrprotects (filemap.c:3636-3641 comment). `fault_dirty_shared_page` (mm/memory.c:2956-2999) then drops the page lock and, for throttling, drops `mmap_lock` via `maybe_unlock_mmap_for_io` — returning `VM_FAULT_COMPLETED` when it did so (2990-2995).

**Key MangoCore takeaway**: the linearization is `page lock`-based. Fault-side: hold the entry lock across `mark_dirty + wait_stable`; writeback-side: hold the entry lock across `mkclean + clear_dirty`. MangoCore's `PageEntry.data: RwLock<()>` + `state: AtomicU8` + `PG_REDIRTIED` already approximates this; the missing piece is the **PTE wrprotect inside the same critical section** and a **wait-for-writeback** equivalent of `folio_wait_stable` (wait until state != Writeback) at fault time.

---

## 3. Linux 6.6 lifecycle

### 3.1 fork → dup_mmap

`dup_mmap` copies the VMA tree. For file VMAs it re-runs `vma_link`-equivalent insertion: `__vma_link_file` (mm/mmap.c:384-393) inserts each copied VMA into `mapping->i_mmap` under `i_mmap_lock_write` — this is why fork is a *re-registration*, not a Weak-copy. **No Weak<AddressSpace> is stored anywhere in the rmap tree**: the tree nodes are VMAs; each VMA points at its mm via `vma->vm_mm` (strong), and the interval tree is keyed by file pgoff range. The `page_add_file_rmap` (mm/rmap.c:1366-1380) happens lazily at the next fault (`finish_fault`/`do_set_pte`), bumping `_mapcount` — so a forked file mapping *with no faults* costs nothing in rmap.

### 3.2 munmap → unmap_region → unmap_vmas

`unmap_region` (mm/mmap.c:2322-2333) → `unmap_vmas(&tlb, ...)` → `unmap_single_vma` → `zap_pte_range`/`zap_pmd_range` walk PTEs under PTL; for each file page `page_remove_rmap(page, vma, ...)` (mm/memory.c:1456, 1483) decrements `_mapcount`. The VMA is removed from `mapping->i_mmap` via `__vma_rb_erase`/`unlink_file_vma` under `i_mmap_lock_write` (the detach side of vma_link). TLB entries are accumulated in the `mmu_gather` and flushed once after the whole region (`unmap_vmas` doc at mm/memory.c:1695-1715: "unmap_vmas() assumes that the caller will flush the whole unmapped address range").

### 3.3 exec → unmap all

`exec_mmap` → `mmput`/`unmap_vmas(&tlb, &mas, vma, 0, ULONG_MAX, ULONG_MAX, false)` (mm/mmap.c:3230) — the same unmap path over the whole address space; every file VMA is unlinked from its `i_mmap` tree and every file page's `_mapcount` is decremented.

### 3.4 truncate

Covered in §1.5: `truncate_inode_pages_range` pass 1 unmap+delete nonblocking, pass 2 `folio_wait_writeback` + delete; `unmap_mapping_range` (with `even_cows`) is the PTE-level zap driven by the same rmap walk. **mapcount matters for eviction**: `mapping_evict_folio` (mm/truncate.c:269-278) refuses to evict a folio whose refcount exceeds the mapped count ("The refcount will be elevated if any page in the folio is mapped") — i.e., a mapped file page is never freed out from under PTEs; it must first be unmapped (which zeroes `_mapcount`), then it can be evicted.

---

## 4. DragonOS: what it actually does (and doesn't)

DragonOS (commit `fc4146b2d`) has **already solved all three blockers**, and it is your declared design reference. This is the single most important finding of this research.

### 4.1 Rmap: per-PageCache VMA registry + per-page VMA set

DragonOS does NOT use Linux's interval tree. It uses two structures:

**(a) `FileVmaIndex` — per-PageCache registry of VMAs (the `i_mmap` analog)**, [page_cache/mapping.rs:7-33](https://github.com/DragonOS-Community/DragonOS/blob/fc4146b2d5d6ddae23b39dee66d9312e80b7a29b/kernel/src/filesystem/page_cache/mapping.rs#L7-L33):

```rust
pub(super) struct FileVmaIndex {
    vmas: HashMap<usize, Weak<LockedVMA>>,   // vma_id → Weak<LockedVMA>
}
```

Protected by `i_mmap_rwsem` on the PageCache (mapping.rs:193-199) plus `file_vma_seq: AtomicU64` (mapping.rs:239-245) — a generation counter for retry revalidation. Register/unregister under `i_mmap_write()` (mapping.rs:247-257).

**Where the `Weak<AddressSpace>` lives: inside the VMA.** [mappings.rs:43-65](https://github.com/DragonOS-Community/DragonOS/blob/fc4146b2d5d6ddae23b39dee66d9312e80b7a29b/kernel/src/mm/ucontext/mappings.rs#L43-L65):

```rust
fn attach_vma(&self, vma: &Arc<LockedVMA>) {
    let vm_file = {
        let mut guard = vma.lock();
        if guard.user_address_space.is_none() {
            guard.user_address_space = Some(self.owner.clone());  // Weak<AddressSpace>
        }
        guard.vm_file.clone()
    };
    if let Some(file) = vm_file {
        if let Some(page_cache) = file.inode().page_cache() {
            page_cache.register_file_vma(vma);   // ← registration at mmap time
        }
    }
}
```

The chain is: `PageCache.FileVmaIndex → Weak<LockedVMA> → LockedVMA.user_address_space: Weak<AddressSpace>`. **Registration happens at VMA attach (mmap/fork) and detach (munmap/exec) — never inside the fault handler.** This is the direct answer to blocker (a).

**(b) Per-page `vma_set: HashSet<Arc<LockedVMA>>`** (page.rs:834-863) — DragonOS's `_mapcount` replacement, a full set of currently-mapped VMAs per page, updated at fault install (`attach_fault_mapped_page`, fault.rs:360-363) and at unmap/mkclean-unmap (page.rs:520-523). `map_count()` = `vma_set.len()` (page.rs:961-963). This is heavier than Linux's atomic counter but gives DragonOS `remove_vma` cleanup for free.

### 4.2 mkclean: exact analog of `folio_mkclean`, with your TLB discipline built in

[page_cache/mapping.rs:617-685](https://github.com/DragonOS-Community/DragonOS/blob/fc4146b2d5d6ddae23b39dee66d9312e80b7a29b/kernel/src/filesystem/page_cache/mapping.rs#L617-L685) — `mkclean_page(page_index, unmap)`:

1. `collect_file_vmas_snapshot` under `i_mmap_read()` + `file_vma_seq` (286-300) — get all VMAs intersecting the page, filter by pgoff intersection.
2. **Group VMAs by `AddressSpace`** (`MmFilePageGroup`, mapping.rs:49-61).
3. For each group: `mm.read()` (AddressSpace **read** lock) + `page_table_edit()` + `MmuGather::gather(&mm)`.
4. For each (vma, virt): if `unmap` → `utable.unmap_phys_preserve_tables(virt)` + `tlb.accumulate_range(virt)`; else → `utable.remap_present(virt, flags.set_write(false).set_dirty(false))` + `tlb.accumulate_range(virt)` (mapping.rs:651-677). `remap_present` is the dedicated `&self` reverse-mapping PTE modifier, documented exactly for this purpose (page.rs:2252-2273: "intended for reverse-mapping walkers such as mkclean / file truncate zap, which run under a separate page-table edit lock").
5. `tlb.finish()` — the TLB flush, **after** the PTE edits, still under the mm read guard in DragonOS's implementation (their MmuGather semantics allow this); your port should follow MangoCore's stricter "flush after unlock" rule.
6. Re-check `file_vma_seq == seq`; if changed, **loop** (mapping.rs:681-684) — the interval-tree revalidation equivalent, needed because the walk ran without blocking and a concurrent mmap/munmap may have changed the VMA set.

Called from two places:
- **Writeback snapshot**: `snapshot_writeback_batch` (writeback.rs:1583-1626) calls `mkclean_page(*page_index, false)` at line 1601 — the `folio_mkclean` analog — while holding `invalidate_read()` (claim_and_snapshot_with_stable_size, writeback.rs:1727-1735) and while the page is in Dirty→Writeback. It then re-checks dirty membership under the page lock and clears PG_DIRTY only if no successor incarnation exists (1602-1618).
- **page_writeback** (page.rs:500-528): `mkclean_page(page_index, unmap)` then removes each unmapped VMA from the page's `vma_set` (520-523).

### 4.3 Fault retry: the retry-token pattern (your blocker (b) solved)

DragonOS **copied the Linux VM_FAULT_RETRY contract** and adapted it to spin locks via a token object:

- `FaultRetryWait` trait + `VmFaultOutcome { reason, retry_wait: Option<Arc<dyn FaultRetryWait>> }` (fault.rs:27-35); `FaultFlags` bitflags mirror Linux exactly including `FAULT_FLAG_ALLOW_RETRY`, `RETRY_NOWAIT`, `TRIED` (fault.rs:37-53); `PageFaultMessage.retry_wait` (fault.rs:81).
- **The admission decision**: `file_fault_invalidate_read()` (mapping.rs:218-233) does a **nonblocking** `try_invalidate_read()`; on failure it returns `PageCacheFaultInvalidateRead::Retry(Arc<dyn FaultRetryWait>)` whose `wait()` just re-acquires `invalidate_read()` (mapping.rs:74-89).
- **do_shared_fault** (fault.rs:750-764): if the retry token is produced, `pfm.set_retry_wait(wait); return VM_FAULT_RETRY;` with the comment "the retry is deliberately armed before entering filesystem page_mkwrite so the outer fault loop releases AddressSpace::write() first." Same in `do_cow_fault` (634-651). The `#[must_use]` on `PageCacheFaultInvalidateRead` (mapping.rs:99) enforces this discipline at compile time.
- **The outer loop** (address_space.rs:87-177, the `populate_range_post_commit` path):

```rust
let retry_wait = {
    let mut guard = self.write();              // ← VM write lock
    ... PageFaultHandler::handle_mm_fault(message) ...
    if outcome.reason.contains(VM_FAULT_RETRY) {
        outcome.retry_wait                     // ← token escapes the guard scope
    } else { ... }
};                                             // ← guard DROPPED here
if let Some(wait) = retry_wait {
    wait.wait()?;                              // ← block with NO VM lock held
}
retried = true;                                // = FAULT_FLAG_TRIED
```

The kernel-uaccess variant is identical in `user_access.rs:93-156` (`'retry: loop { let mut space_guard = mm.write...; ...; if VM_FAULT_RETRY { drop(space_guard); wait.wait()?; continue 'retry; } }`).

This is exactly the "fault must release the VM lock, wait, and retry" boundary you need — **expressed as a scoped guard whose value escapes the scope before the wait**.

### 4.4 DragonOS gaps (what you cannot borrow)

1. **No per-page `_mapcount` atomic**: the `vma_set: HashSet<Arc<LockedVMA>>` is O(mappings) memory and takes the page write lock on every map/unmap. For a 4KB-page kernel this is fine but heavier than Linux.
2. **No interval tree**: `collect_file_vmas` iterates *all* VMAs of the page cache and filters by pgoff (mapping.rs:264-284) — O(V) per mkclean instead of O(log V + k). With the seq-revalidation loop, correctness holds; performance degrades with many mappings. DragonOS accepts this; you can too at first (see §5).
3. **riscv64 user-fault trap path is a stub**: `do_trap_load/store_page_fault` in the riscv64 interrupt handler only logs and spins (handle.rs:220-258) — user-mode hardware faults are NOT wired into `PageFaultHandler` yet. The handler is only exercised via `fault_in_user_va`-style uaccess and mlock population. **This is a gap you must not copy** — your trap path (`hal/arch/riscv/trap/mod.rs:189`) already routes faults, you just need to add the retry loop around it.
4. **mkclean's TLB flush happens under the mm read guard** (tlb.finish() at mapping.rs:678 inside `mm.read()` scope); MangoCore's documented discipline (flush after unlock) is stricter and safer — keep MangoCore's.

---

## 5. Pragmatic adaptation for MangoCore

Grounded against MangoCore's actual code:
- Trap entry: `task.process.vm().write(|vm| vm.do_page_fault(addr, access))` ([os/src/hal/arch/riscv/trap/mod.rs:189](file:///home/pxy/projects/MangoCore-smp/os/src/hal/arch/riscv/trap/mod.rs#L189)); LA64 at hal/arch/loongarch64/trap/mod.rs:281.
- `AddressSpace::write()` closure runs the fault; `MmuGather::seal(&self.tlb)` produces a `TlbFlush` executed after unlock (os/src/mm/address_space.rs:120-158).
- `do_page_fault(&mut self, addr, access) -> Result<PhysAddr, MemoryError>` constructs `FaultContext` inside the lock (address_space.rs:907-929; page_fault.rs:21-44).
- `filemap_shared_write_fault` calls `pc.frame_for_write(page_index)` and maps the cache frame with W (filemap.rs:167-203).
- `PageCache` has `op_gate: RwLock<()>`, `PageEntry { page, data: RwLock<()>, state: AtomicU8, valid_mask, flags }` with `PG_REDIRTIED` (page_cache.rs:248-271, 507-542, 848-857); `writeback_page` does `op_gate.read()` + Dirty→Writeback CAS + backend write (1620-1699).

### 5.1 (i) Rmap: adopt DragonOS's `FileVmaIndex`, NOT a per-page binding list

**Decision: per-PageCache VMA registry (HashMap<vma_id, Weak<LockedVMA>>) + optional per-page `vma_set` for mapcount. No per-page `Weak<AddressSpace>` list.**

Why this wins over both alternatives:

| Option | Verdict |
|---|---|
| Linux interval tree | Correct but needs a balanced tree with pgoff-range queries and careful erase; overkill for the first implementation |
| **DragonOS HashMap registry** | **Adopt** — O(1) register/unregister, O(V) walk; the seq-revalidation loop makes it correct without RCU or blocking locks |
| per-page `Weak<AddressSpace>` binding list | **Reject** — exactly your blocker (a): the fault constructs context inside the VM lock and cannot pre-capture a Weak; a per-page list of Weak<AddressSpace> also can't answer "which VMA at which VA" (you'd need per-AS page tables anyway) and duplicates the per-page-cache index |

**Where `Weak<AddressSpace>` lives**: inside the `Vma` (add `user_address_space: Option<Weak<AddressSpace>>`, set at mmap/fork attach — MangoCore already has the process's AddressSpace Arc available in the mmap path). The PageCache registry stores `Weak<LockedVMA>`; the VMA is the single owner of the AddressSpace Weak. **Registration happens at mmap/mprotect-share/fork (attach) and munmap/exec (detach), not in the fault handler** — resolving blocker (a) by moving registration out of the fault entirely.

Add to `PageCache` (alongside `op_gate`, page_cache.rs:510):
```rust
i_mmap: SpinLock<HashMap<usize, Weak<Vma>>>,  // vma_id → Weak<Vma>
i_mmap_seq: AtomicU64,                        // generation for retry revalidation
```
Registration points (all have the VMA + can reach the inode's PageCache): `mmap` file-backed VMA creation, `fork` VMA duplication, `exec`/`munmap` teardown. The `Vma` already knows its file (`area.vm_file()`); `inode.ensure_page_cache()` is already reachable from the fault path (filemap.rs:177-179), so it is reachable from mmap too.

The per-page mapcount: add `mapped_vmas: SpinLock<HashSet<usize>>` (vma ids) or a simple `AtomicUsize map_count` to `PageEntry`, bumped at `filemap_shared_write_fault`/`filemap_read_fault` PTE install and decremented at munmap/truncate zap. A count is sufficient for eviction/truncate decisions; keep the full set only if you need mlock/unevictable semantics later.

### 5.2 (ii) Fault retry: `Retry(RetryWait)` out-param + scoped-guard loop

**Decision: change the fault entry to return an outcome enum, and restructure the two callers (trap + uaccess) into a DragonOS-style scoped loop.**

```rust
pub enum FaultOutcome {
    Completed(PhysAddr),
    Retry(Arc<dyn RetryWait>),   // lock already dropped by caller loop
    Error(MemoryError),
}
```

- `AddressSpaceInner::do_page_fault` keeps its current signature but the *outer* `vm.write(...)` closures in `hal/arch/riscv/trap/mod.rs:189` and `hal/arch/loongarch64/trap/mod.rs:281` must become:

```rust
loop {
    let wait = {
        let mut guard = vm.write();
        match guard.do_page_fault(addr, access) {
            Ok(pa) => break pa,               // completed
            Err(Retry(w)) => w,               // guard dropped at scope end
            Err(e) => return /* signal / errno */,
        }
    };
    wait.wait();                              // NO VM lock held here
    retried = true;                           // = FAULT_FLAG_TRIED
}
```

- **Where the Retry is produced**: the only place that needs it is `filemap_shared_write_fault` (and `filemap_private_fault` if you want the same invalidation protection). Before touching the PageCache, do a **nonblocking** `op_gate.try_read()`; on failure return `Retry(PcRetryWait { pc })` whose `wait()` blocks on `op_gate.read()`. This breaks the inversion: writeback holds `op_gate.read()` (and wants the VM lock for mkclean); the fault holds the VM lock (and wants `op_gate`); the fault yields first.
- **Kernel uaccess** (`fault_in_user_va`, address_space.rs:936-951): same loop, but do **not** return to user — loop in kernel with the retry boundary; this is your `FAULT_FLAG_RETRY_NOWAIT` analog. Since MangoCore uaccess never holds the VM lock across arbitrary sleeps today, and the fault handler is already entered from `fault_in_trap_va`/`fault_in_user_va` under `vm.write()`, the only change is the loop + try-read admission.
- **The `retried` flag** prevents livelock: on re-entry, the shared-write fault must not re-return Retry for the same reason; it either acquires `op_gate` via the blocking `read()` (safe now — the outer loop already dropped the VM lock... but wait, re-entry means the VM lock is re-acquired; the blocking `op_gate.read()` must then happen *before* taking the VM lock, i.e. in the admission step of the trap loop, not inside `do_page_fault`).

**Concrete rule**: perform the `try_read`/`read` admission of `op_gate` in the *outer loop* (which alternates VM-lock-held and VM-lock-free), so the blocking acquire never happens under the VM lock. Inside `do_page_fault`, only use the nonblocking `try_read`; a failure → `Retry`.

- **`folio_wait_stable` analog**: after acquiring the page and before restoring W, if `state == Writeback` (or `PG_REDIRTIED` pending), you must wait. This wait also cannot happen under the VM lock → return `Retry(WaitWriteback { pc, index })` whose `wait()` blocks on the entry's state/wait-queue. This is the second, equally important Retry source (mirrors `filemap_page_mkwrite`'s `folio_wait_stable`, filemap.c:3642).

### 5.3 (iii) Lifecycle: register / unregister / truncate-zap

**fork** — in VMA duplication: for each file-backed VMA, call `pc.register_file_vma(vma)` (via `vma.user_address_space` Weak + `vm_file` → PageCache), under `i_mmap` lock; bump `i_mmap_seq`. No page-level work until the first fault installs a PTE (then bump `PageEntry.map_count`).

**munmap / exec** — in VMA teardown: `pc.unregister_file_vma(vma_id)`; for each resident `PageEntry` of the unmapped range, decrement `map_count` (and optionally remove from `mapped_vmas`). The `MmuGather`/`TlbFlush` discipline already in the unmap path covers the TLB side.

**truncate** — mirror `truncate_inode_pages_range` + DragonOS `truncate` (mapping.rs:375-395):
1. **Pass A (nonblocking)**: `unmap_mapping_pages_even_cow(hole_start, None)`-equivalent — snapshot VMAs intersecting the hole under `i_mmap` lock + seq; group by AddressSpace; for each, take its VM lock, **unmap or write-protect** PTEs (`remap_present(set_write(false).set_dirty(false))` or `unmap`), collect into `MmuGather`, decrement `PageEntry.map_count`; release VM lock; `TlbFlush::execute`; re-check seq, loop if changed. This is DragonOS's `unmap_mapping_pages_with_mode` (mapping.rs:330-373) and matches MangoCore's existing "锁内 record_change — seal — 解锁 — TlbFlush::execute" invariant (address_space.rs:10-12).
2. **Pass B (waiting)**: under `op_gate.write()`, for each entry in the hole: if `Loading`/`Writeback`, **wait** (outside any VM lock — the `op_gate` write guard is the barrier); then remove the entry. Mirror the DragonOS `truncate_locked` loop (mapping.rs:449-615) and Linux pass-2 `folio_wait_writeback` (truncate.c:423).

**Truncate vs fault lock order**: truncate takes `op_gate.write()` then VM locks (via unmap); fault takes VM lock then `try op_gate.read()`. They never hold both the same way — the fault's `Retry` guarantees it. This is the entire lock-inversion story, and it is why the retry token is not optional.

### 5.4 Writeback write-protect integration (the mkclean placement)

In `writeback_page`/`writeback_pages_run` (page_cache.rs:1620-1833), after the Dirty→Writeback CAS and before the backend write — and *while holding the entry's `data` read lock* (your page-lock analog):

1. `pc.mkclean_page(page_index, false)` — the §5.3 rmap walk, write-protecting + clearing PTE dirty bits (this is `folio_mkclean`, rmap.c:1016, at the position of `folio_clear_dirty_for_io`, page-writeback.c:2896).
2. Re-check under the entry lock: if a fault already re-dirtied (PG_REDIRTIED or state==Dirty), skip the PTE-clear... actually the correct order is mkclean *first* then snapshot (DragonOS writeback.rs:1601 does exactly this inside snapshot). The existing `PG_REDIRTIED` completion path (1678-1684) remains correct: the fault that raced in after mkclean re-marked dirty; the writeback restores Dirty.

**TLB discipline for mkclean**: collect the flush records into a *temporary* `MmuGather` per affected AddressSpace, and execute after that AddressSpace's VM lock is released (stricter than DragonOS's in-guard `tlb.finish()`; identical to MangoCore's existing rule). The writeback worker does NOT hold the VM lock, so this is straightforward — the VM lock is acquired only for the brief PTE edit + record.

### 5.5 Lock-order summary (the whole protocol)

```
Fault (user trap / uaccess):
    loop {
        [no lock]  wait for admission: op_gate.try_read() → Retry(wait) if busy
                   (also: page state Writeback → Retry(WaitWriteback))
        VM write lock (vm.write)
            resolve VMA → page_fault → filemap_shared_write_fault
                try op_gate.read (admission)  → on failure return Retry
                frame_for_write (Loading wait happens HERE — only if op_gate held? see note)
                mark PG_DIRTY + install PTE (W restored)
        unlock VM → MmuGather.execute (TlbFlush)
    }

Writeback worker:
    op_gate.read()                                (invalidate_read analog)
        Dirty→Writeback CAS
        entry.data read lock
            mkclean: i_mmap read → per-MM VM lock → PTE wrprotect/cleandirty
                     → record TLB → unlock VM → TlbFlush.execute
            snapshot bytes
        PG_REDIRTIED completion handling
    op_gate released
    backend I/O

Truncate:
    Pass A: i_mmap read+seq → per-MM VM lock → unmap/wrprotect PTE → unlock → flush
            loop on seq change
    Pass B: op_gate.write() → wait for Writeback/Loading → remove entries → release
```

One caveat to flag for your Oracle: `frame_for_write` (page_cache.rs:976) currently does a `get_or_create` that can wait on `Loading` (state machine at 880-899). Under this protocol, **that wait must be moved out of the VM lock too** — either the admission step pre-creates the entry (like Linux `__filemap_get_folio(FGP_CREAT|FGP_FOR_MMAP)` at filemap.c:3301-3303, which happens *before* the lock-drop point but still under mmap_lock... Linux actually drops mmap_lock only for I/O, and `get_or_create` can block on the invalidate_lock — that's why `filemap_fault` takes `filemap_invalidate_lock_shared` at 3298 and can return VM_FAULT_RETRY via `out_retry`). Simplest MangoCore-consistent rule: **any wait inside the fault (Loading, Writeback, op_gate) returns `Retry`; the outer loop re-enters with a `retried` flag.** Your `frame_for_write` should get a `try_` variant or the fault loop should pre-reserve the frame outside the VM lock.

---
