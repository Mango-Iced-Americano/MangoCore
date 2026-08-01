#![allow(static_mut_refs)]

//! Boot protocol primitives.
//!
//! The entry assembly saves firmware-provided registers into static cells.
//! `save_boot_info()` is called as the VERY FIRST thing in rust_main(),
//! before any arch init that might clobber registers.
//!
//! `boot_info()` provides read-only access throughout the kernel lifetime.

/// How the firmware transferred control to the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootProtocol {
    /// Standard RISC-V SBI boot: a0=hartid, a1=dtb_paddr.
    RiscvFdt,
    /// LoongArch64 QEMU direct boot or legacy U-Boot handoff.
    LoongArchLegacy,
    /// Kernel self-test or regression mode (no firmware).
    Test,
}

/// Raw boot information saved by entry assembly before any Rust code runs.
#[derive(Debug, Clone, Copy)]
pub struct RawBootInfo {
    pub protocol: BootProtocol,
    pub hart_id: usize,
    /// Physical address of the DTB (Flattened Device Tree) blob, or 0 if none.
    pub dtb_paddr: usize,
    /// Physical address of the entry instruction reached from firmware.
    pub entry_paddr: usize,
    /// Physical address of the first byte of the relative Image.
    pub image_paddr: usize,
}

// These #[no_mangle] statics are written by entry.asm BEFORE rust_main().
// They live in .bss (zero-initialized). The entry asm writes to them directly
// using their symbol addresses.
#[no_mangle]
#[link_section = ".data.boot"]
pub static mut RAW_HART_ID: usize = 0;

#[no_mangle]
#[link_section = ".data.boot"]
pub static mut RAW_DTB_PADDR: usize = 0;

#[no_mangle]
#[link_section = ".data.boot"]
pub static mut RAW_ENTRY_PADDR: usize = 0;

#[no_mangle]
#[link_section = ".data.boot"]
pub static mut RAW_IMAGE_PADDR: usize = 0;

/// Call this as the VERY FIRST line of rust_main().
/// Detects the boot protocol from feature flags and saves the raw register values.
pub fn save_boot_info() {
    let protocol = detect_protocol();
    // SAFETY: Entry assembly initializes these cells before calling rust_main(),
    // and boot runs single-threaded until the scheduler is initialized.
    let hart_id = unsafe { RAW_HART_ID };
    // SAFETY: Same single-threaded entry handoff as RAW_HART_ID above.
    let dtb_paddr = unsafe { RAW_DTB_PADDR };
    // SAFETY: Same single-threaded entry handoff as RAW_HART_ID above.
    let entry_paddr = unsafe { RAW_ENTRY_PADDR };
    // SAFETY: Same single-threaded entry handoff as RAW_HART_ID above.
    let image_paddr = unsafe { RAW_IMAGE_PADDR };
    // SAFETY: save_boot_info() is invoked once before any concurrent execution;
    // boot_info() only exposes an immutable reference after this initialization.
    unsafe {
        SAVED_BOOT_INFO = Some(RawBootInfo {
            protocol,
            hart_id,
            dtb_paddr,
            entry_paddr,
            image_paddr,
        });
    }
}

/// Returns the saved boot info. Panics if save_boot_info() was not called.
pub fn boot_info() -> &'static RawBootInfo {
    // SAFETY: save_boot_info() completes before any caller can retrieve boot info,
    // and the saved value is never mutated afterwards.
    unsafe {
        match SAVED_BOOT_INFO.as_ref() {
            Some(info) => info,
            None => panic!("boot info not saved"),
        }
    }
}

/// Translate an RV64 kernel-linked virtual address to its runtime physical
/// address. Other architectures retain their existing identity layout.
pub fn kernel_linked_to_phys(linked_vaddr: usize) -> usize {
    #[cfg(target_arch = "riscv64")]
    {
        linked_vaddr - crate::config::KERNEL_LINK_VADDR + boot_info().image_paddr
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        linked_vaddr
    }
}

// Internal storage.
#[link_section = ".data.boot"]
static mut SAVED_BOOT_INFO: Option<RawBootInfo> = None;

/// Determine protocol from the target ABI.
fn detect_protocol() -> BootProtocol {
    #[cfg(target_arch = "riscv64")]
    {
        return BootProtocol::RiscvFdt;
    }
    #[cfg(target_arch = "loongarch64")]
    {
        return BootProtocol::LoongArchLegacy;
    }
    #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
    {
        BootProtocol::Test
    }
}
