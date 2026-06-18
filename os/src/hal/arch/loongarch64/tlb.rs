use super::{ASId, TLBEHi, TLBIdx, TLBEL, TLBELO0, TLBELO1};
use crate::config::PAGE_SIZE_BITS;
use crate::mm::{PhysPageNum, VirtPageNum};
use core::arch::asm;
use core::sync::atomic::{AtomicU16, Ordering};
use spin::Mutex;

/// Kernel ASID — never assigned to user processes.
pub const KERN_ASID: u16 = 0;
/// First user-available ASID.
const USER_ASID_BASE: u16 = 1;
/// Maximum user ASIDs (exclusive).
const USER_ASID_MAX: u16 = 256;
/// Sentinel for "no ASID assigned".
pub const ASID_NONE: u16 = u16::MAX;

/// Simple bitmap ASID allocator.  ASIDs recycle when exhausted.
static ASID_BITMAP: Mutex<[u64; 4]> = Mutex::new([0u64; 4]);
static ASID_NEXT_HINT: AtomicU16 = AtomicU16::new(USER_ASID_BASE);

/// Allocate a free user ASID.  Returns `ASID_NONE` if exhausted.
pub fn asid_alloc() -> u16 {
    let mut map = ASID_BITMAP.lock();
    let start = ASID_NEXT_HINT.load(Ordering::Relaxed);
    for offset in 0..(USER_ASID_MAX - USER_ASID_BASE) {
        let id = USER_ASID_BASE + ((start - USER_ASID_BASE + offset) % (USER_ASID_MAX - USER_ASID_BASE));
        let word = (id as usize) / 64;
        let bit = (id as usize) % 64;
        if map[word] & (1u64 << bit) == 0 {
            map[word] |= 1u64 << bit;
            ASID_NEXT_HINT.store(id.wrapping_add(1).min(USER_ASID_MAX - 1).max(USER_ASID_BASE), Ordering::Relaxed);
            return id;
        }
    }
    ASID_NONE
}

/// Free a user ASID so it can be reused.
pub fn asid_free(id: u16) {
    if id >= USER_ASID_BASE && id < USER_ASID_MAX {
        let mut map = ASID_BITMAP.lock();
        let word = (id as usize) / 64;
        let bit = (id as usize) % 64;
        map[word] &= !(1u64 << bit);
    }
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
    unsafe {
        asm!("invtlb 0x3,$zero, $zero");
    }
    crate::task::perf::record_tlb_full();
}
#[inline(always)]
pub fn tlb_invalidate_page(vpn: VirtPageNum) {
    // INVTLB_ADDR_GFALSE_AND_ASID requires the target ASID in rj.
    let vaddr = (vpn.0 & !1) << PAGE_SIZE_BITS;
    let asid = current_asid() as usize;
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
    unsafe {
        asm!("tlbrd");
    }
}
fn tlbsrch() {
    unsafe {
        asm!("tlbsrch");
    }
}
