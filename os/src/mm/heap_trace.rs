//! Heap allocation tracing — track every alloc/dealloc by call site.
//!
//! Enabled by `heap_trace` feature. The global mutex directly owns both fixed tables.
//! Post-processing: `rust-addr2line -e os -f -p 0x8020XXXX` maps PC to source.

use core::alloc::Layout;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

// ── constants ───────────────────────────────────────────────────────────────

const ACTIVE_CAP: usize = 1 << 20; // 1,048,576 entries (~25 MB)
const SITES_CAP: usize = 16384; // 16K sites
const STACK_DEPTH: usize = 6;

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
    active: [ActiveEntry; ACTIVE_CAP],
    active_count: usize,
    active_dropped: usize,
    sites: [SiteEntry; SITES_CAP],
    site_count: usize,
    site_dropped: usize,
    unknown_free: usize,
    tracked_actual: usize,
    tracked_req: usize,
}

const EMPTY_ACTIVE_ENTRY: ActiveEntry = ActiveEntry {
    ptr: 0,
    req_size: 0,
    actual_size: 0,
    site_idx: 0,
};

const EMPTY_SITE_ENTRY: SiteEntry = SiteEntry {
    hash: 0,
    pcs: [0; STACK_DEPTH],
    live_req: 0,
    live_actual: 0,
    peak_req: 0,
    allocs: 0,
    frees: 0,
    max_req: 0,
    _pad: 0,
};

// 缓冲区与其唯一可变 owner 放在同一对象中；只有取得 TRACE guard
// 才能得到数组的 `&mut`。显式放入 BSS，避免约 25 MiB 的全零诊断表
// 膨胀内核镜像；双架构 linker script 会将 `.bss.*` 纳入 `sbss..ebss` 清零区。
#[link_section = ".bss.heap_trace"]
static TRACE: Mutex<TraceState> = Mutex::new(TraceState::new());
static TRACE_ENABLED: AtomicBool = AtomicBool::new(false);

impl TraceState {
    const fn new() -> Self {
        Self {
            active: [EMPTY_ACTIVE_ENTRY; ACTIVE_CAP],
            active_count: 0,
            active_dropped: 0,
            sites: [EMPTY_SITE_ENTRY; SITES_CAP],
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
    let mut trace = TRACE.lock();
    trace.dump_oom_inner(failing_layout);
}

pub fn print_summary() -> bool {
    if !is_enabled() {
        return false;
    }
    let mut trace = TRACE.lock();
    trace.print_summary_inner()
}

// ── stack capture (RISC-V) ──────────────────────────────────────────────────

#[cfg(target_arch = "riscv64")]
/// 捕获当前内核栈上的返回地址。
///
/// # Safety
///
/// 只能在内核上下文中调用；当前 `s0` 必须是本内核栈上的有效 frame pointer。
/// 本函数会按 RISC-V 调用约定读取保存的 `ra`/`fp`，并用栈范围和内核 text 范围限制遍历。
unsafe fn capture_stack(pcs: &mut [usize; STACK_DEPTH]) -> usize {
    let mut ra: usize;
    let mut fp: usize;
    let mut sp: usize;
    // Safety: 只读取当前寄存器值，不访问内存，也不修改控制流。
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

    // Resolve the exact guarded stack slot containing SP.  The stack arena is
    // no longer adjacent to TRAMPOLINE, and accepting the whole gap would let
    // a corrupt frame pointer cross a guard page before being dereferenced.
    let arena_bottom = crate::hal::config::KERNEL_STACK_BOTTOM;
    let arena_top = crate::hal::config::KERNEL_STACK_TOP;
    if sp < arena_bottom || sp >= arena_top {
        return count;
    }
    let slot = (arena_top - 1 - sp) / crate::hal::config::KERNEL_STACK_SLOT_SIZE;
    let kstack_upper = arena_top - slot * crate::hal::config::KERNEL_STACK_SLOT_SIZE;
    let kstack_lower = kstack_upper - crate::hal::config::KERNEL_STACK_SIZE;
    if sp < kstack_lower || sp >= kstack_upper {
        return count;
    }

    for i in 1..STACK_DEPTH {
        if fp < core::cmp::max(sp, kstack_lower + 2 * core::mem::size_of::<usize>())
            || fp > kstack_upper
        {
            break;
        }
        // Safety: `fp` 已被限制在当前内核栈范围内；读取的是标准栈帧中保存的
        // return address 和上一帧 frame pointer。
        let saved_ra = *(fp.wrapping_sub(8) as *const usize);
        let saved_fp = *(fp.wrapping_sub(16) as *const usize);

        extern "C" {
            fn stext();
            fn etext();
        }
        let text_start = stext as *const () as usize;
        let text_end = etext as *const () as usize;
        if !(text_start..text_end).contains(&saved_ra) {
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
/// 非 RISC-V 架构暂不捕获堆栈。
///
/// # Safety
///
/// 不读取 raw pointer 或寄存器，只把输出缓冲区清零。
unsafe fn capture_stack(pcs: &mut [usize; STACK_DEPTH]) -> usize {
    for i in 0..STACK_DEPTH {
        pcs[i] = 0;
    }
    0
}

// ── table operations ────────────────────────────────────────────────────────

impl TraceState {
    fn active_entry(&self, idx: usize) -> ActiveEntry {
        debug_assert!(idx < ACTIVE_CAP);
        self.active[idx]
    }

    fn active_entry_mut(&mut self, idx: usize) -> &mut ActiveEntry {
        debug_assert!(idx < ACTIVE_CAP);
        &mut self.active[idx]
    }

    fn site_entry(&self, idx: usize) -> &SiteEntry {
        debug_assert!(idx < SITES_CAP);
        &self.sites[idx]
    }

    fn site_entry_mut(&mut self, idx: usize) -> &mut SiteEntry {
        debug_assert!(idx < SITES_CAP);
        &mut self.sites[idx]
    }

    fn active_probe(&self, ptr: usize) -> usize {
        ((ptr.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) as usize) % ACTIVE_CAP
    }

    fn site_hash(pcs: &[usize; STACK_DEPTH]) -> u64 {
        let mut h: u64 = 0x9AE1_6A3B_2F90_404F;
        for &pc in pcs.iter() {
            h = h
                .wrapping_mul(0xC6A4_A793_5BD1_E995)
                .wrapping_add(pc as u64);
        }
        h
    }

    fn site_probe(hash: u64) -> usize {
        (hash as usize) % SITES_CAP
    }

    // ── alloc ───────────────────────────────────────────────────────────

    fn record_alloc_inner(&mut self, ptr: usize, req: usize, actual: usize, _align: usize) {
        let mut pcs = [0usize; STACK_DEPTH];
        // Safety: 仅在分配器内部诊断路径调用；失败时最多得到 0 PC，不影响分配语义。
        let _depth = unsafe { capture_stack(&mut pcs) };

        let hash = Self::site_hash(&pcs);
        let site_idx = self.find_or_create_site(hash, &pcs);

        if !self.active_insert(ptr, req as u32, actual as u32, site_idx) {
            return;
        }

        let site = self.site_entry_mut(site_idx as usize);
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
            let site_hash = self.site_entry(idx).hash;
            if site_hash == 0 {
                if self.site_count >= SITES_CAP {
                    self.site_dropped += 1;
                    idx = hash as usize % SITES_CAP;
                    break;
                }
                let site = self.site_entry_mut(idx);
                site.hash = hash;
                site.pcs = *pcs;
                self.site_count += 1;
                return idx as u32;
            }
            let site = self.site_entry(idx);
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
            let entry = self.active_entry_mut(idx);
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
            let site = self.site_entry_mut(site_idx);
            site.live_req = site.live_req.wrapping_sub(entry.req_size as usize);
            site.live_actual = site.live_actual.wrapping_sub(entry.actual_size as usize);
            site.frees = site.frees.wrapping_add(1);
        }

        self.tracked_actual = self.tracked_actual.wrapping_sub(entry.actual_size as usize);
        self.tracked_req = self.tracked_req.wrapping_sub(entry.req_size as usize);
    }

    /// Remove an entry using the same bounded probe sequence as insertion.
    ///
    /// The table uses a triangular/quadratic probe sequence, so linear
    /// backward-shift deletion would corrupt lookup chains.  Keep removal
    /// simple: clear the matching slot, and let future lookups scan the full
    /// bounded sequence instead of stopping at the first empty slot.
    fn active_remove(&mut self, ptr: usize) -> Option<ActiveEntry> {
        let mut idx = self.active_probe(ptr);
        let mut step: usize = 1;

        loop {
            let entry = self.active_entry(idx);
            if entry.ptr == ptr {
                self.active_count = self.active_count.saturating_sub(1);
                *self.active_entry_mut(idx) = ActiveEntry::default();
                return Some(entry);
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
            let s = self.site_entry(i);
            if s.hash == 0 || s.live_actual == 0 {
                continue;
            }
            let la = s.live_actual;
            for j in 0..20 {
                match top[j] {
                    None => {
                        top[j] = Some((i, la));
                        break;
                    }
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

        let mut sum_live_req: usize = 0;
        let mut sum_live_actual: usize = 0;
        for i in 0..SITES_CAP {
            let s = self.site_entry(i);
            sum_live_req = sum_live_req.wrapping_add(s.live_req);
            sum_live_actual = sum_live_actual.wrapping_add(s.live_actual);
        }

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
                let s = self.site_entry(*idx);
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
            let e = self.active_entry(i);
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
        let mut total_live: usize = 0;
        for i in 0..SITES_CAP {
            total_live = total_live.wrapping_add(self.site_entry(i).live_actual);
        }
        if total_live < 4 * 1024 * 1024 {
            return false;
        }

        let mut top: [Option<(usize, usize)>; 3] = [None; 3];
        for i in 0..SITES_CAP {
            let s = self.site_entry(i);
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
                let s = self.site_entry(*idx);
                // Print the first non-zero, non-obvious-hook PC.
                let pc = first_useful_pc(&s.pcs);
                print!(
                    " {:#x}:{}K/{}K/a{}/f{} pcs={:#x},{:#x},{:#x},{:#x},{:#x},{:#x}",
                    pc,
                    s.live_req >> 10,
                    la >> 10,
                    s.allocs,
                    s.frees,
                    s.pcs[0],
                    s.pcs[1],
                    s.pcs[2],
                    s.pcs[3],
                    s.pcs[4],
                    s.pcs[5],
                );
            }
        }
        println!("");
        true
    }
}

/// Sentinel function for `first_useful_pc` — see module-level docs.
#[no_mangle]
pub fn heap_trace_text_marker() {}

/// Return the first PC in `pcs` whose address does not fall inside
/// the allocator or heap_trace module.  We probe run-time addresses of
/// sentinel functions placed in each module (±8 KB window) so the filter
/// stays correct across linker layout changes.
fn first_useful_pc(pcs: &[usize; STACK_DEPTH]) -> usize {
    // Addresses of sentinel functions placed in the two modules.
    let alloc_base = super::heap_allocator::heap_allocator_text_marker as usize;
    let trace_base = heap_trace_text_marker as usize;

    // Module code is typically < 8 KB; this window safely covers it.
    const WINDOW: usize = 8 * 1024;

    let is_alloc_frame = |pc: usize| -> bool {
        if pc >= alloc_base.saturating_sub(WINDOW) && pc < alloc_base.saturating_add(WINDOW) {
            return true;
        }
        if pc >= trace_base.saturating_sub(WINDOW) && pc < trace_base.saturating_add(WINDOW) {
            return true;
        }
        false
    };

    for &pc in pcs.iter() {
        if pc == 0 {
            continue;
        }
        if is_alloc_frame(pc) {
            continue;
        }
        return pc;
    }

    // Fallback: return the first non-zero PC (the outermost frame we have).
    for &pc in pcs.iter() {
        if pc != 0 {
            return pc;
        }
    }
    0
}

// ── targeted 49152/8 probe (temporary diagnostic) ───────────────────────────

/// 临时诊断探针：专门观察 `size=49152, align=8` 的内核堆分配。
///
/// 背景：CI 上 `HEAP ALLOCATION FAILED layout: size=49152, align=8`
/// 在 netperf TCP_CRR / pipe09 场景双架构复现，需要区分：
///   - "凶手"：某调用点反复分配 49152 却不释放，堆被它耗尽；
///   - "受害者"：49152 分配本身无辜，只是堆被其它对象泄漏耗尽后
///     第一个撞上 OOM 的分配。
/// 判定方式：对比 `PROBE_49152_ALLOCS` 与 `PROBE_49152_DEALLOCS`——
/// alloc 远大于 dealloc（且随时间单调上涨）=> 该调用点是凶手；
/// 两者基本平衡 => 是受害者，真正的泄漏在别处（用 dump_oom 的
/// top_live 站点表找凶手）。
///
/// 仅 `heap_trace` feature 下编译（整个模块已按该 feature 门控），
/// 默认构建完全不含此代码，无性能开销。命中时打印前 N 次调用栈
/// （RISC-V 通过 frame pointer 捕获；LA64 尚无栈回溯，pcs 为 0），
/// 并带出 `KERNEL_HEAP_CURRENT_BYTES` 当前堆水位。
pub static PROBE_49152_ALLOCS: AtomicUsize = AtomicUsize::new(0);
pub static PROBE_49152_DEALLOCS: AtomicUsize = AtomicUsize::new(0);
static PROBE_49152_LOGGED: AtomicUsize = AtomicUsize::new(0);

/// 前 N 次命中打印完整调用栈；之后只累加计数，避免刷屏。
const PROBE_49152_LOG_LIMIT: usize = 5;

const PROBE_49152_SIZE: usize = 49152;
const PROBE_49152_ALIGN: usize = 8;

#[inline]
fn is_probe_49152(layout: Layout) -> bool {
    layout.size() == PROBE_49152_SIZE && layout.align() == PROBE_49152_ALIGN
}

pub fn probe_49152_alloc(layout: Layout) {
    if !is_probe_49152(layout) {
        return;
    }
    // 计数始终累加，不受 LOG 级别影响。
    PROBE_49152_ALLOCS.fetch_add(1, Ordering::Relaxed);
    if PROBE_49152_LOGGED.fetch_add(1, Ordering::Relaxed) >= PROBE_49152_LOG_LIMIT {
        return;
    }
    // `log_off` 时 `log::error!` 完全展开为空，这里直接不计算，
    // 避免无用变量告警；诊断构建需用 `LOG=error`（或更高级别）。
    #[cfg(not(feature = "log_off"))]
    {
        let mut pcs = [0usize; STACK_DEPTH];
        // Safety: 与 capture_stack 文档一致——只在内核栈上下文读 fp/ra。
        let depth = unsafe { capture_stack(&mut pcs) };
        let heap_current = super::heap_allocator::KERNEL_HEAP_CURRENT_BYTES.load(Ordering::Relaxed);
        let useful = first_useful_pc(&pcs);
        log::error!(
            "[heap-probe] 49152 alloc: size={} align={} heap_current={}K allocs={} deallocs={} site={:#x} depth={} pcs={:#018x},{:#018x},{:#018x},{:#018x},{:#018x},{:#018x}",
            layout.size(),
            layout.align(),
            heap_current >> 10,
            PROBE_49152_ALLOCS.load(Ordering::Relaxed),
            PROBE_49152_DEALLOCS.load(Ordering::Relaxed),
            useful,
            depth,
            pcs[0], pcs[1], pcs[2], pcs[3], pcs[4], pcs[5],
        );
    }
}

pub fn probe_49152_dealloc(layout: Layout) {
    if is_probe_49152(layout) {
        PROBE_49152_DEALLOCS.fetch_add(1, Ordering::Relaxed);
    }
}
