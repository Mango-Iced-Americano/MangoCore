//! LoongArch64 TLB 与 ASID 管理。
//!
//! 提供用户 ASID 分配、当前 ASID 设置、页级/全局 TLB invalidation，以及少量
//! TLB 调试读取辅助函数。

use super::{ASId, TLBEHi, TLBIdx, TLBEL, TLBELO0, TLBELO1};
use crate::config::PAGE_SIZE_BITS;
use crate::mm::{PhysPageNum, VirtPageNum};
use core::arch::asm;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

/// Kernel ASID — never assigned to user processes.
pub const KERN_ASID: u16 = 0;
/// First user-available ASID.
const USER_ASID_BASE: u16 = 1;
/// CSR.ASID 中硬件 ASID 字段固定占低 10 位；高位只由软件保存 epoch。
const ASID_CONTEXT_BITS: u32 = 10;
const ASID_CONTEXT_MASK: u64 = (1u64 << ASID_CONTEXT_BITS) - 1;

/// 全局 ASID 分配状态。
///
/// 同一 epoch 内只递增 `next`，MM 销毁时不立即复用编号。这样 ASID 的复用点
/// 只有 rollover，而 rollover 会先同步清空全部 CPU 的 non-global TLB。
struct AsidAllocator {
    epoch: u64,
    next: u16,
    max: u16,
}

static ASID_ALLOCATOR: Mutex<AsidAllocator> = Mutex::new(AsidAllocator {
    epoch: 1,
    next: USER_ASID_BASE,
    max: 0,
});
static ASID_ROLLOVER: AtomicBool = AtomicBool::new(false);
static ASID_ROLLOVERS: AtomicUsize = AtomicUsize::new(0);

/// CPU0 根据 CSR.ASID.ASIDBITS 初始化硬件可用编号范围。
pub fn init_asid_allocator() -> usize {
    let width = ASId::read().get_asid_width();
    assert!(
        (1..=ASID_CONTEXT_BITS as usize).contains(&width),
        "unsupported LoongArch ASID width {}",
        width
    );
    let max = ((1usize << width) - 1) as u16;
    let mut allocator = ASID_ALLOCATOR.lock();
    assert_eq!(allocator.max, 0, "ASID allocator initialized twice");
    allocator.max = max;
    max as usize
}

/// 为一个 MM 返回当前 epoch 内的 ASID context；耗尽或换代中返回 `None`。
///
/// `current` 的低 10 位是硬件 ASID，其余位是软件 epoch。调用方在同一 MM 的
/// VM 锁内调用，因此同一地址空间不会并发消耗多个编号。
pub fn try_assign_asid(current: u64) -> Option<u64> {
    let mut allocator = ASID_ALLOCATOR.lock();
    assert_ne!(
        allocator.max, 0,
        "ASID allocator used before initialization"
    );
    if ASID_ROLLOVER.load(Ordering::Acquire) {
        return None;
    }

    let current_epoch = current >> ASID_CONTEXT_BITS;
    let current_asid = (current & ASID_CONTEXT_MASK) as u16;
    if current_epoch == allocator.epoch
        && current_asid >= USER_ASID_BASE
        && current_asid <= allocator.max
    {
        return Some(current);
    }
    if allocator.next > allocator.max {
        return None;
    }

    let asid = allocator.next;
    allocator.next += 1;
    Some((allocator.epoch << ASID_CONTEXT_BITS) | u64::from(asid))
}

/// 从软件 ASID context 中取出写入 CSR.ASID 的硬件编号。
pub const fn hardware_asid(context: u64) -> u16 {
    (context & ASID_CONTEXT_MASK) as u16
}

/// ASID 空间耗尽时执行一次全 CPU flush，再发布新 epoch。
///
/// 调用点不得持有 VM、runqueue 或其它普通锁。等待者临时开放本地中断，保证
/// rollover leader 发来的 TLB IPI 能被处理，不会形成双方等待 ack 的死锁。
pub fn rollover_asids() {
    {
        let allocator = ASID_ALLOCATOR.lock();
        if allocator.next <= allocator.max {
            return;
        }
    }

    if ASID_ROLLOVER
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        let irq_was_enabled = crate::hal::local_irq_save();
        crate::hal::local_irq_restore(true);
        while ASID_ROLLOVER.load(Ordering::Acquire) {
            spin_loop();
        }
        let _ = crate::hal::local_irq_save();
        crate::hal::local_irq_restore(irq_was_enabled);
        return;
    }

    // CAS 前的容量判断可能与上一轮完成交错；成为 leader 后必须再次确认。
    let old_epoch = {
        let allocator = ASID_ALLOCATOR.lock();
        if allocator.next <= allocator.max {
            ASID_ROLLOVER.store(false, Ordering::Release);
            return;
        }
        allocator.epoch
    };

    let targets = crate::smp::online_cpu_mask();
    if let Err(error) = crate::smp::synchronize_user_tlb(targets, KERN_ASID, None) {
        panic!("ASID rollover TLB flush failed: {:?}", error);
    }

    {
        let mut allocator = ASID_ALLOCATOR.lock();
        assert_eq!(
            allocator.epoch, old_epoch,
            "ASID epoch changed by two leaders"
        );
        let new_epoch = old_epoch
            .checked_add(1)
            .expect("LoongArch ASID epoch exhausted");
        assert!(
            new_epoch <= (u64::MAX >> ASID_CONTEXT_BITS),
            "LoongArch ASID context encoding exhausted"
        );
        allocator.epoch = new_epoch;
        allocator.next = USER_ASID_BASE;
    }
    ASID_ROLLOVERS.fetch_add(1, Ordering::Relaxed);
    ASID_ROLLOVER.store(false, Ordering::Release);
}

pub fn asid_capacity() -> usize {
    ASID_ALLOCATOR.lock().max as usize
}

pub fn asid_rollover_count() -> usize {
    ASID_ROLLOVERS.load(Ordering::Acquire)
}

/// Set Address Space ID of current core.
#[inline(always)]
pub fn set_asid(asid: u16) {
    let mut id = ASId::read();
    id.set_asid(asid as usize).write();
}

/// Get current ASID.
#[inline(always)]
pub fn current_asid() -> u16 {
    ASId::read().get_asid() as u16
}
#[allow(unused)]
pub fn tlb_addr_allow_write(vpn: VirtPageNum, ppn: PhysPageNum) -> Result<(), ()> {
    TLBEHi::read().set_vppn(vpn).write();
    tlbsrch();
    let ret = TLBIdx::read();
    if ret.is_non_existent() {
        return Err(());
    } else {
        if vpn.0 & 1 == 0 {
            TLBELO0::read().set_ppn(ppn).set_dirty(true).write();
        } else {
            TLBELO1::read().set_ppn(ppn).set_dirty(true).write();
        }
        Ok(())
    }
}
#[inline(always)]
/// Invalidate non-global TLB entries
pub fn tlb_invalidate() {
    // Safety: `invtlb 0x3` invalidates non-global TLB entries for the current
    // core and does not access memory through Rust references.
    unsafe {
        asm!("invtlb 0x3,$zero, $zero");
    }
    crate::task::perf::record_tlb_full();
}
#[inline(always)]
pub fn tlb_invalidate_user_page(asid: u16, vpn: VirtPageNum) {
    // LoongArch 的一个普通 TLB entry 同时保存相邻的偶/奇页，因此官方和
    // Linux 都把 VA 对齐到 2 * PAGE_SIZE；这是硬件能提供的最小失效粒度。
    let vaddr = (vpn.0 & !1) << PAGE_SIZE_BITS;
    let asid = asid as usize;
    // Safety: `invtlb 0x5` treats `asid` and `vaddr` as architectural operands
    // for invalidation only; `vaddr` is not dereferenced.
    unsafe {
        asm!(
            "invtlb 0x5, {asid}, {vaddr}",
            asid = in(reg) asid,
            vaddr = in(reg) vaddr,
            options(nostack)
        );
    }
    crate::task::perf::record_tlb_page();
}
#[inline(always)]
pub fn tlb_invalidate_global_page(vpn: VirtPageNum) {
    // INVTLB_ADDR_GTRUE_OR_ASID with ASID 0 covers global kernel mappings.
    let vaddr = (vpn.0 & !1) << PAGE_SIZE_BITS;
    // Safety: `invtlb 0x6` invalidates by virtual address and does not
    // dereference `vaddr`.
    unsafe {
        asm!(
            "invtlb 0x6, $zero, {vaddr}",
            vaddr = in(reg) vaddr,
            options(nostack)
        );
    }
    crate::task::perf::record_tlb_page();
}
#[inline(always)]
pub fn tlb_global_invalidate() {
    // Safety: `invtlb 0x0` performs a global TLB invalidation on the current
    // core and has no Rust-visible memory access.
    unsafe {
        asm!("invtlb 0x0,$zero, $zero");
    }
    crate::task::perf::record_tlb_global();
}
#[allow(unused)]
pub fn tlb_read(idx: usize) -> Result<(PhysPageNum, PhysPageNum), ()> {
    TLBIdx::read().set_index(idx).write();
    let ret = TLBIdx::read();

    tlbrd();

    if ret.is_non_existent() {
        Err(())
    } else {
        Ok((TLBELO0::read().get_ppn(), TLBELO1::read().get_ppn()))
    }
}
#[allow(unused)]
pub fn tlb_search(vpn: VirtPageNum) -> Result<PhysPageNum, ()> {
    TLBEHi::read().set_vppn(vpn).write();

    tlbsrch();

    let ret = TLBIdx::read();
    if ret.is_non_existent() {
        Err(())
    } else {
        if vpn.0 & 1 == 0 {
            Ok(tlb_read(ret.get_index()).unwrap().0)
        } else {
            Ok(tlb_read(ret.get_index()).unwrap().1)
        }
    }
}

fn tlbrd() {
    // Safety: `tlbrd` only transfers the selected hardware TLB entry into CSR
    // state, which is read by the caller through register wrappers.
    unsafe {
        asm!("tlbrd");
    }
}
fn tlbsrch() {
    // Safety: `tlbsrch` searches the hardware TLB using CSR state prepared by
    // the caller and writes the result back to TLBIdx.
    unsafe {
        asm!("tlbsrch");
    }
}
