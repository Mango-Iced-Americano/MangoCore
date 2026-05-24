//! Heap allocation tracing — track every alloc/dealloc by call site.
//!
//! Enabled by `heap_trace` feature. Uses static arrays + spin::Mutex only.
//! Post-processing: `rust-addr2line -e os -f -p 0x8020XXXX` maps PC to source.

use core::alloc::Layout;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

// ── constants ───────────────────────────────────────────────────────────────

const ACTIVE_CAP: usize = 1 << 20;   // 1,048,576 entries (~25 MB)
const SITES_CAP: usize = 16384;      // 16K sites
const STACK_DEPTH: usize = 6;

const KERNEL_TEXT_BASE: usize = 0x8020_0000;
const KERNEL_TEXT_END: usize  = 0x8100_0000;

// ── data structures ─────────────────────────────────────────────────────────

#[derive(Clone, Copy, Default)]
struct ActiveEntry {
    ptr: usize,
    req_size: u32,
    actual_size: u32,
    site_idx: u32,
}

#[derive(Clone, Copy, Default)]
struct SiteEntry {
    hash: u64,
    pcs: [usize; STACK_DEPTH],
    live_req: usize,
    live_actual: usize,
    peak_req: usize,
    allocs: u64,
    frees: u64,
    max_req: u32,
    _pad: u32,
}

// ── global state ────────────────────────────────────────────────────────────

struct TraceState {
    active: &'static mut [ActiveEntry],
    active_count: usize,
    active_dropped: usize,
    sites: &'static mut [SiteEntry],
    site_count: usize,
    site_dropped: usize,
    unknown_free: usize,
    tracked_actual: usize,
    tracked_req: usize,
}

static mut ACTIVE_BUF: [ActiveEntry; ACTIVE_CAP] = [ActiveEntry {
    ptr: 0, req_size: 0, actual_size: 0, site_idx: 0,
}; ACTIVE_CAP];

static mut SITES_BUF: [SiteEntry; SITES_CAP] = [SiteEntry {
    hash: 0, pcs: [0; STACK_DEPTH], live_req: 0, live_actual: 0,
    peak_req: 0, allocs: 0, frees: 0, max_req: 0, _pad: 0,
}; SITES_CAP];

static TRACE: Mutex<TraceState> = Mutex::new(TraceState::new());
static TRACE_ENABLED: AtomicBool = AtomicBool::new(false);

impl TraceState {
    const fn new() -> Self {
        Self {
            active: unsafe { &mut *core::ptr::addr_of_mut!(ACTIVE_BUF) },
            active_count: 0,
            active_dropped: 0,
            sites: unsafe { &mut *core::ptr::addr_of_mut!(SITES_BUF) },
            site_count: 0,
            site_dropped: 0,
            unknown_free: 0,
            tracked_actual: 0,
            tracked_req: 0,
        }
    }
}

// ── public API ──────────────────────────────────────────────────────────────

pub fn enable() {
    TRACE_ENABLED.store(true, Ordering::Relaxed);
}

pub fn is_enabled() -> bool {
    TRACE_ENABLED.load(Ordering::Relaxed)
}

pub fn record_alloc(ptr: *mut u8, layout: Layout, actual_size: usize) {
    if !is_enabled() {
        return;
    }
    let mut trace = TRACE.lock();
    trace.record_alloc_inner(ptr as usize, layout.size(), actual_size, layout.align());
}

pub fn record_dealloc(ptr: *mut u8) {
    if !is_enabled() {
        return;
    }
    let mut trace = TRACE.lock();
    trace.record_dealloc_inner(ptr as usize);
}

pub fn dump_oom(failing_layout: Layout) {
    if !is_enabled() {
        return;
    }
    let trace = TRACE.lock();
    trace.dump_oom_inner(failing_layout);
}

pub fn print_summary() -> bool {
    if !is_enabled() {
        return false;
    }
    let trace = TRACE.lock();
    trace.print_summary_inner()
}

// ── stack capture (RISC-V) ──────────────────────────────────────────────────

#[cfg(target_arch = "riscv64")]
unsafe fn capture_stack(pcs: &mut [usize; STACK_DEPTH]) -> usize {
    let mut ra: usize;
    let mut fp: usize;
    let mut sp: usize;
    core::arch::asm!(
        "mv {}, ra",
        "mv {}, s0",
        "mv {}, sp",
        out(reg) ra,
        out(reg) fp,
        out(reg) sp,
    );
    pcs[0] = ra;
    let mut count = 1;

    // Kernel stacks live in [TRAMPOLINE - N * STACK_SIZE, TRAMPOLINE].
    // Use the current sp as lower bound, TRAMPOLINE as upper bound.
    let kstack_upper = crate::hal::config::TRAMPOLINE;

    for i in 1..STACK_DEPTH {
        if fp < sp || fp > kstack_upper {
            break;
        }
        let saved_ra = *(fp.wrapping_sub(8) as *const usize);
        let saved_fp = *(fp.wrapping_sub(16) as *const usize);

        // Validate saved_ra looks like a kernel text address.
        if saved_ra < KERNEL_TEXT_BASE || saved_ra > KERNEL_TEXT_END {
            break;
        }
        // Frame pointer must move upward (toward older frames).
        if saved_fp != 0 && (saved_fp <= fp || saved_fp > kstack_upper) {
            break;
        }

        pcs[i] = saved_ra.wrapping_sub(4);
        fp = saved_fp;
        count += 1;
    }
    count
}

#[cfg(not(target_arch = "riscv64"))]
unsafe fn capture_stack(pcs: &mut [usize; STACK_DEPTH]) -> usize {
    for i in 0..STACK_DEPTH {
        pcs[i] = 0;
    }
    0
}

// ── table operations ────────────────────────────────────────────────────────

impl TraceState {
    fn active_probe(&self, ptr: usize) -> usize {
        ((ptr.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) as usize) % ACTIVE_CAP
    }

    fn site_hash(pcs: &[usize; STACK_DEPTH]) -> u64 {
        let mut h: u64 = 0x9AE1_6A3B_2F90_404F;
        for &pc in pcs.iter() {
            h = h.wrapping_mul(0xC6A4_A793_5BD1_E995).wrapping_add(pc as u64);
        }
        h
    }

    fn site_probe(hash: u64) -> usize {
        (hash as usize) % SITES_CAP
    }

    // ── alloc ───────────────────────────────────────────────────────────

    fn record_alloc_inner(&mut self, ptr: usize, req: usize, actual: usize, _align: usize) {
        let mut pcs = [0usize; STACK_DEPTH];
        let _depth = unsafe { capture_stack(&mut pcs) };

        let hash = Self::site_hash(&pcs);
        let site_idx = self.find_or_create_site(hash, &pcs);

        if !self.active_insert(ptr, req as u32, actual as u32, site_idx) {
            return;
        }

        let site = &mut self.sites[site_idx as usize];
        site.live_req = site.live_req.wrapping_add(req);
        site.live_actual = site.live_actual.wrapping_add(actual);
        if site.live_req > site.peak_req {
            site.peak_req = site.live_req;
        }
        site.allocs = site.allocs.wrapping_add(1);
        if (req as u32) > site.max_req {
            site.max_req = req as u32;
        }

        self.tracked_actual = self.tracked_actual.wrapping_add(actual);
        self.tracked_req = self.tracked_req.wrapping_add(req);
    }

    fn find_or_create_site(&mut self, hash: u64, pcs: &[usize; STACK_DEPTH]) -> u32 {
        let mut idx = Self::site_probe(hash);
        let mut step: usize = 1;
        loop {
            let site = &mut self.sites[idx];
            if site.hash == 0 {
                if self.site_count >= SITES_CAP {
                    self.site_dropped += 1;
                    idx = hash as usize % SITES_CAP;
                    break;
                }
                site.hash = hash;
                site.pcs = *pcs;
                self.site_count += 1;
                return idx as u32;
            }
            if site.hash == hash && site.pcs == *pcs {
                return idx as u32;
            }
            idx = (idx + step) % SITES_CAP;
            step += 1;
            if step > 64 {
                self.site_dropped += 1;
                idx = hash as usize % SITES_CAP;
                break;
            }
        }
        idx as u32
    }

    fn active_insert(&mut self, ptr: usize, req: u32, actual: u32, site_idx: u32) -> bool {
        if self.active_count >= ACTIVE_CAP {
            self.active_dropped += 1;
            return false;
        }
        let mut idx = self.active_probe(ptr);
        let mut step: usize = 1;
        loop {
            let entry = &mut self.active[idx];
            if entry.ptr == 0 {
                entry.ptr = ptr;
                entry.req_size = req;
                entry.actual_size = actual;
                entry.site_idx = site_idx;
                self.active_count += 1;
                return true;
            }
            idx = (idx + step) % ACTIVE_CAP;
            step += 1;
            if step > 64 {
                self.active_dropped += 1;
                return false;
            }
        }
    }

    // ── dealloc ─────────────────────────────────────────────────────────

    fn record_dealloc_inner(&mut self, ptr: usize) {
        let entry = match self.active_remove(ptr) {
            Some(e) => e,
            None => {
                self.unknown_free += 1;
                return;
            }
        };

        let site_idx = entry.site_idx as usize;
        if site_idx < SITES_CAP {
            let site = &mut self.sites[site_idx];
            site.live_req = site.live_req.wrapping_sub(entry.req_size as usize);
            site.live_actual = site.live_actual.wrapping_sub(entry.actual_size as usize);
            site.frees = site.frees.wrapping_add(1);
        }

        self.tracked_actual = self.tracked_actual.wrapping_sub(entry.actual_size as usize);
        self.tracked_req = self.tracked_req.wrapping_sub(entry.req_size as usize);
    }

    /// Remove an entry and re-compact the probe chain so future lookups
    /// don't hit fake misses.  Uses backward-shift deletion.
    fn active_remove(&mut self, ptr: usize) -> Option<ActiveEntry> {
        let start = self.active_probe(ptr);
        let mut idx = start;
        let mut step: usize = 1;

        loop {
            let entry = self.active[idx];
            if entry.ptr == 0 {
                return None;
            }
            if entry.ptr == ptr {
                // Remove this entry.
                self.active_count = self.active_count.saturating_sub(1);
                let result = Some(entry);

                // Backward-shift: pull subsequent entries in the same
                // probe chain forward to fill the gap.
                let mut gap = idx;
                loop {
                    idx = (idx + 1) % ACTIVE_CAP;
                    let next = self.active[idx];
                    if next.ptr == 0 {
                        self.active[gap].ptr = 0;
                        return result;
                    }
                    let ideal = self.active_probe(next.ptr);
                    // Does this entry belong before or at the gap?
                    // Check if ideal is "between" gap and idx in the
                    // circular table sense.
                    if !in_probe_range(ideal, gap, idx) {
                        continue;
                    }
                    self.active[gap] = next;
                    self.active[idx].ptr = 0;
                    gap = idx;
                }
            }
            idx = (idx + step) % ACTIVE_CAP;
            step += 1;
            if step > 64 {
                return None;
            }
        }
    }

    // ── dump ────────────────────────────────────────────────────────────

    fn dump_oom_inner(&self, failing_layout: Layout) {
        let mut top: [Option<(usize, usize)>; 20] = [None; 20];

        for i in 0..SITES_CAP {
            let s = &self.sites[i];
            if s.hash == 0 || s.live_actual == 0 {
                continue;
            }
            let la = s.live_actual;
            for j in 0..20 {
                match top[j] {
                    None => { top[j] = Some((i, la)); break; }
                    Some((_, t)) if la > t => {
                        for k in (j + 1..20).rev() {
                            top[k] = top[k - 1];
                        }
                        top[j] = Some((i, la));
                        break;
                    }
                    _ => {}
                }
            }
        }

        let sum_live_req: usize = self.sites.iter().map(|s| s.live_req).sum();
        let sum_live_actual: usize = self.sites.iter().map(|s| s.live_actual).sum();

        println!(
            "[heap_trace] oom fail size={} align={} active={} live_req={}K live_actual={}K tracked_req={}K tracked_actual={}K dropped={}/{} unknown_free={}",
            failing_layout.size(),
            failing_layout.align(),
            self.active_count,
            sum_live_req >> 10,
            sum_live_actual >> 10,
            self.tracked_req >> 10,
            self.tracked_actual >> 10,
            self.active_dropped,
            self.site_dropped,
            self.unknown_free,
        );

        for (rank, entry) in top.iter().enumerate() {
            if let Some((idx, _)) = entry {
                let s = &self.sites[*idx];
                println!(
                    "[heap_trace] top_live rank={} live_req={}K live_actual={}K allocs={} frees={} peak={}K max={} site={} pcs={:#018x},{:#018x},{:#018x},{:#018x},{:#018x},{:#018x}",
                    rank + 1,
                    s.live_req >> 10,
                    s.live_actual >> 10,
                    s.allocs,
                    s.frees,
                    s.peak_req >> 10,
                    s.max_req,
                    idx,
                    s.pcs[0], s.pcs[1], s.pcs[2], s.pcs[3], s.pcs[4], s.pcs[5],
                );
            }
        }

        let mut by_order = [0usize; 32];
        let mut by_order_bytes = [0usize; 32];
        for i in 0..ACTIVE_CAP {
            let e = self.active[i];
            if e.ptr == 0 {
                continue;
            }
            let order = (e.actual_size as usize)
                .next_power_of_two()
                .trailing_zeros() as usize;
            if order < 32 {
                by_order[order] += 1;
                by_order_bytes[order] += e.actual_size as usize;
            }
        }
        print!("[heap_trace] by_order ");
        for o in 0..32 {
            if by_order[o] > 0 {
                print!("{}:{}/{}K ", o, by_order[o], by_order_bytes[o] >> 10);
            }
        }
        println!("");
    }

    // ── periodic summary ────────────────────────────────────────────────

    fn print_summary_inner(&self) -> bool {
        let total_live: usize = self.sites.iter().map(|s| s.live_actual).sum();
        if total_live < 4 * 1024 * 1024 {
            return false;
        }

        let mut top: [Option<(usize, usize)>; 3] = [None; 3];
        for i in 0..SITES_CAP {
            let s = &self.sites[i];
            if s.hash == 0 || s.live_actual == 0 {
                continue;
            }
            let la = s.live_actual;
            for j in 0..3 {
                match top[j] {
                    None => {
                        top[j] = Some((i, la));
                        break;
                    }
                    Some((_, t)) if la > t => {
                        for k in (j + 1..3).rev() {
                            top[k] = top[k - 1];
                        }
                        top[j] = Some((i, la));
                        break;
                    }
                    _ => {}
                }
            }
        }

        print!(
            "[htrace] live={}K active={} top:",
            total_live >> 10,
            self.active_count
        );
        for entry in top.iter() {
            if let Some((idx, la)) = entry {
                let s = &self.sites[*idx];
                // Print the first non-zero, non-obvious-hook PC.
                let pc = first_useful_pc(&s.pcs);
                print!(
                    " {:#x}:{}K/{}/{}",
                    pc, s.live_req >> 10, la >> 10, s.allocs
                );
            }
        }
        println!("");
        true
    }
}

/// Return true if `ideal` is between `start` (inclusive) and `end`
/// (exclusive) when moving forward in the circular table.
fn in_probe_range(ideal: usize, start: usize, end: usize) -> bool {
    if start <= end {
        ideal >= start && ideal <= end
    } else {
        // Wrapped around
        ideal >= start || ideal <= end
    }
}

/// Return the first PC in `pcs` that is not a hook-internal address
/// (i.e. not inside `heap_trace` or the allocator wrapper), falling
/// back to pcs[0] if none found.
fn first_useful_pc(pcs: &[usize; STACK_DEPTH]) -> usize {
    // Hook functions live roughly in 0x802c_0000..0x802d_0000 (for this kernel build).
    // Skip PCs from within that range.
    for &pc in pcs.iter() {
        if pc == 0 {
            continue;
        }
        // Skip addresses inside heap_trace / allocator hook (approximate range).
        if pc >= 0x802c_0000 && pc < 0x802e_0000 {
            continue;
        }
        return pc;
    }
    pcs[0]
}
