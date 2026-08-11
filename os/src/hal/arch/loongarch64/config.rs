//! LoongArch64 平台内存布局和内核常量。
//!
//! 定义 QEMU/2K1000 使用的用户地址空间、内核栈窗口、MMIO、页大小和时钟参数。

// Sizes
/// QEMU la64 exposes high memory as memory@80000000 with size 0x30000000.
#[cfg(feature = "boot_la_qemu")]
pub const MEMORY_SIZE: usize = 0x3000_0000;
/// 2K1000LA exposes two DRAM banks whose combined capacity is 2 GiB.
#[cfg(feature = "boot_la_uboot_dmw")]
pub const MEMORY_SIZE: usize = 0x8000_0000;
pub const USER_STACK_SIZE: usize = PAGE_SIZE * 0x100;
pub const USER_STACK_INIT_SIZE: usize = PAGE_SIZE * 0x40;
pub const USER_HEAP_SIZE: usize = PAGE_SIZE * 0x100;
pub const SYSTEM_TASK_LIMIT: usize = {
    // la64 kernel stacks are mapped in a guarded kernel VA window.
    let by_ram = MEMORY_SIZE / (KERNEL_STACK_SIZE * 4);
    let limit = if by_ram < KERNEL_STACK_MAX_SLOTS {
        by_ram
    } else {
        KERNEL_STACK_MAX_SLOTS
    };
    if limit < 512 {
        512
    } else {
        limit
    }
};
pub const SYSTEM_TASK_SOFT_LIMIT: usize = SYSTEM_TASK_LIMIT * 9 / 10;
pub const SYSTEM_FD_LIMIT: usize = 4096;
pub const PAGE_SIZE: usize = 0x1000;
pub const PAGE_SIZE_BITS: usize = PAGE_SIZE.trailing_zeros() as usize;
pub const PTE_WIDTH: usize = 8;
pub const PTE_WIDTH_BITS: usize = PTE_WIDTH.trailing_zeros() as usize;
pub const DIR_WIDTH: usize = PAGE_SIZE_BITS - PTE_WIDTH_BITS;
/// 三级页表的三个 9 位索引覆盖 VA[38:12]。VA[VALEN-1] 用于选择 PGDL/PGDH，
/// 不属于页表内部索引。因此，仅在第 38 位以上不同的虚拟地址可能在同一根页表中
/// 形成别名，必须显式隔离各虚拟地址分配区间。
pub const PAGE_TABLE_VA_BITS: usize = PAGE_SIZE_BITS + DIR_WIDTH * 3;
pub const PAGE_TABLE_VA_MASK: usize = (1usize << PAGE_TABLE_VA_BITS) - 1;

#[cfg(debug_assertions)]
pub const KSTACK_PG_NUM_SHIFT: usize = 16usize.trailing_zeros() as usize;
#[cfg(not(debug_assertions))]
pub const KSTACK_PG_NUM_SHIFT: usize = 16usize.trailing_zeros() as usize;

pub const KERNEL_STACK_SIZE: usize = PAGE_SIZE * 0x20;
pub const KERNEL_STACK_SLOT_SIZE: usize = KERNEL_STACK_SIZE + PAGE_SIZE;
pub const KERNEL_STACK_MAX_SLOTS: usize = 1024;
pub const BOOT_STACK_SIZE: usize = PAGE_SIZE * 0x40;
/// Bootstrap-only heap used before FRAME_ALLOCATOR can reserve runtime backing.
///
/// 必须容纳 36G 内存时 frame allocator 的 `FrameRegion::recycled_flags`
/// （`Vec<bool>`，每页 1 字节，buddy 圆整到 order 12 = 16 MiB 连续块）以及
/// `laflex::IdentityDirtyMap`（36G 时约 1.19 MiB，圆整到 order 9）。32 MiB
/// 为双架构统一余量，避免大内存启动早期在 runtime heap 建立前 OOM。
pub const KERNEL_BOOTSTRAP_HEAP_SIZE: usize = 32 * 1024 * 1024;

// Addresses
/// QEMU 提供 48 位物理/虚拟地址模型。
#[cfg(feature = "boot_la_qemu")]
pub const PALEN: usize = 48;
#[cfg(feature = "boot_la_qemu")]
pub const VALEN: usize = 48;
/// 2K1000LA 在 CPUCFG1 中报告 PABITS=VABITS=40。如果误用 QEMU 的 48 位模型，
/// 内核会接受非规范地址，CPU 将在硬件页表遍历器读取有效 PTE 前抛出 AddressError。
#[cfg(feature = "boot_la_uboot_dmw")]
pub const PALEN: usize = 40;
#[cfg(feature = "boot_la_uboot_dmw")]
pub const VALEN: usize = 40;
/// Maximum address in virtual address space.
/// May be used to extract virtual address from a segmented address
/// `0`-extension may be performed using this mask.
/// e.g. `flag` &= `VA_MASK`
pub const VA_MASK: usize = (1 << VALEN) - 1;
/// Mask for extracting segment number from usize address.
/// `1`-extension may be performed using this mask.
/// e.g. `flag` |= `SEG_MASK`
pub const SEG_MASK: usize = !VA_MASK;
/// 未进行符号扩展的 VPN 位掩码，对应 VA[VALEN-1:PAGE_SHIFT]。
pub const VPN_MASK: usize = VA_MASK >> PAGE_SIZE_BITS;
/// TLB 偶奇页对的 VPPN 字段掩码，对应 VA[VALEN-1:PAGE_SHIFT+1]。
pub const VPPN_MASK: usize = VPN_MASK >> 1;
/// Mask for extracting segment number from VPN.
/// All-one for segment field.
/// `1`-extension may be performed using this mask.
/// e.g. `flag` |= `SEG_MASK`
pub const VPN_SEG_MASK: usize = SEG_MASK >> PAGE_SIZE_BITS;

/// 为原始 VALEN 位虚拟地址恢复架构规定的符号扩展。
pub const fn canonicalize_vaddr(addr: usize) -> usize {
    let raw = addr & VA_MASK;
    if raw & (1usize << (VALEN - 1)) != 0 {
        raw | SEG_MASK
    } else {
        raw
    }
}

pub const fn is_canonical_vaddr(addr: usize) -> bool {
    canonicalize_vaddr(addr) == addr
}

/// 恢复规范虚拟地址转换为 VPN 时移除的符号扩展。
pub const fn canonicalize_vpn(vpn: usize) -> usize {
    let raw = vpn & VPN_MASK;
    if raw & (1usize << (VALEN - PAGE_SIZE_BITS - 1)) != 0 {
        raw | VPN_SEG_MASK
    } else {
        raw
    }
}

pub const HIGH_BASE_EIGHT: usize = 0x8000_0000_0000_0000;
pub const HIGH_BASE_ZERO: usize = 0x0000_0000_0000_0000;

// manually make usable memory space equal
pub const SUC_DMW_VSEG: usize = 8;
pub const MEMORY_HIGH_BASE: usize = HIGH_BASE_ZERO;
pub const MEMORY_HIGH_BASE_VPN: usize = MEMORY_HIGH_BASE >> PAGE_SIZE_BITS;
pub const USER_STACK_BASE: usize = TASK_SIZE - PAGE_SIZE | LA_START;
#[cfg(feature = "boot_la_qemu")]
pub const MEMORY_START: usize = 0x0000_0000_8000_0000;
// `MEMORY_START` remains the kernel load bank base. It is not the lowest DRAM
// address on 2K1000LA; callers that need all RAM must iterate firmware regions.
#[cfg(feature = "boot_la_uboot_dmw")]
pub const MEMORY_START: usize = 0x0000_0000_9000_0000;
#[cfg(feature = "boot_la_qemu")]
pub const MEMORY_END: usize = MEMORY_SIZE + MEMORY_START;
#[cfg(feature = "boot_la_uboot_dmw")]
pub const MEMORY_END: usize = 0x0000_0001_0000_0000;

/// Physical DRAM banks as half-open byte ranges.
///
/// The 2K1000LA hole at 0x10000000..0x90000000 contains MMIO/non-RAM and must
/// never be converted into allocatable frames. U-Boot enters MangoCore through
/// a DMW alias, but these are the raw physical addresses used in PTEs and DMA.
#[cfg(feature = "boot_la_qemu")]
pub const MEMORY_REGIONS_FALLBACK: &[(usize, usize)] = &[(MEMORY_START, MEMORY_END)];
#[cfg(feature = "boot_la_uboot_dmw")]
pub const MEMORY_REGIONS_FALLBACK: &[(usize, usize)] =
    &[(0x0000_0000, 0x1000_0000), (0x9000_0000, MEMORY_END)];

/// DRAM ranges still owned by firmware or active devices after `bootm`.
#[cfg(feature = "boot_la_qemu")]
pub const FIRMWARE_RESERVED_REGIONS_FALLBACK: &[(usize, usize)] = &[];
#[cfg(feature = "boot_la_uboot_dmw")]
pub const FIRMWARE_RESERVED_REGIONS_FALLBACK: &[(usize, usize)] = &[
    // U-Boot LMB/stack, the active DVO framebuffer, CPU1's U-Boot park loop,
    // and BPI/SMBIOS data. This can be split and reclaimed only after those
    // owners have been explicitly quiesced or copied.
    (0x0cbf_4000, 0x1000_0000),
];

/// RAM currently available to MangoCore after static firmware reservations.
///
/// `MEMORY_SIZE` is the installed DRAM capacity. Linux-compatible memory
/// statistics use this smaller value until the board handoff code explicitly
/// quiesces the firmware owners and releases their carveouts.
#[cfg(feature = "boot_la_qemu")]
pub const USABLE_MEMORY_SIZE: usize = MEMORY_SIZE;
#[cfg(feature = "boot_la_uboot_dmw")]
pub const USABLE_MEMORY_SIZE: usize =
    MEMORY_SIZE
        - (FIRMWARE_RESERVED_REGIONS_FALLBACK[0].1 - FIRMWARE_RESERVED_REGIONS_FALLBACK[0].0)
        - PAGE_SIZE;

pub const SV39_SPACE: usize = 1 << 39;
pub const USR_SPACE_LEN: usize = SV39_SPACE >> 2;
pub const LA_START: usize = 0x1_2000_0000;
pub const USR_VIRT_SPACE_END: usize = USR_SPACE_LEN - 1;
pub const USER_VA_BASE: usize = LA_START;
pub const USER_VA_END: usize = LA_START + USR_SPACE_LEN;
pub const ELF_PIE_BASE: usize = USER_VA_BASE + 0x0040_0000;
pub const SIGNAL_TRAMPOLINE: usize = USR_VIRT_SPACE_END - PAGE_SIZE + 1;
pub const TRAMPOLINE: usize = SIGNAL_TRAMPOLINE - PAGE_SIZE;
pub const TRAP_CONTEXT_BASE: usize = TRAMPOLINE - KERNEL_STACK_MAX_SLOTS * PAGE_SIZE;
pub const USR_MMAP_END: usize = TRAP_CONTEXT_BASE;
pub const USR_MMAP_BASE: usize = USR_MMAP_END - USR_SPACE_LEN / 8 + 0x3000;
pub const TASK_SIZE: usize = USR_MMAP_BASE - USR_SPACE_LEN / 8;
pub const ELF_DYN_BASE: usize = (((TASK_SIZE - LA_START) / 3 * 2) | LA_START) & (!(PAGE_SIZE - 1));

// 512G的虚拟内存？
pub const MMAP_BASE: usize = 0xFFFF_FF80_0000_0000;
pub const MMAP_END: usize = 0xFFFF_FFFF_FFFF_0000;
/// 临时 ELF 载荷必须避开 PGDH 中低地址恒等映射的 39 位别名。
///
/// `MMAP_BASE` 的 VA[38:0] 为零；内核把 FDT 发现的低地址资源写入同一
/// PGDH 页表后，该起点会和物理零页的 PTE 重合。保留低 4 GiB 作为所有
/// 固件/PCI 恒等映射的别名保护带，临时 ELF 窗口从其上方开始。
pub const KERNEL_PROGRAM_BASE: usize = MMAP_BASE + (1usize << 32);
#[cfg(feature = "boot_la_qemu")]
pub const KERNEL_STACK_TOP: usize = MMAP_BASE - PAGE_SIZE;
// 当 VALEN=40 时，MMAP_BASE 是高半区第一个规范地址。若将栈放在它下方，会产生
// 位于非规范空洞中的 0xffffff7... 地址，并在页表转换开始前触发 AddressError。
// 因此改为从规范高半区顶部向下分配栈槽。
#[cfg(feature = "boot_la_uboot_dmw")]
pub const KERNEL_STACK_TOP: usize = MMAP_END - PAGE_SIZE;
pub const KERNEL_STACK_BOTTOM: usize =
    KERNEL_STACK_TOP - KERNEL_STACK_SLOT_SIZE * KERNEL_STACK_MAX_SLOTS;
/// 内核临时 ELF 映射的上界。PGDH 负责选择高半区，其下三级页表仅索引 VA[38:12]。
/// 因此该表达式同时保留真实栈窗口及其最近的低 39 位别名，防止临时 ELF 映射
/// 覆盖内核栈 PTE。
pub const KERNEL_PROGRAM_END: usize =
    (!PAGE_TABLE_VA_MASK) | (KERNEL_STACK_BOTTOM & PAGE_TABLE_VA_MASK);
pub const SKIP_NUM: usize = 1;

const _: () = {
    assert!(PALEN > PAGE_SIZE_BITS && PALEN < usize::BITS as usize);
    assert!(VALEN > PAGE_TABLE_VA_BITS && VALEN < usize::BITS as usize);
    assert!(VA_MASK & SEG_MASK == 0);
    assert!(VA_MASK | SEG_MASK == usize::MAX);
    assert!(VPN_MASK | VPN_SEG_MASK == usize::MAX >> PAGE_SIZE_BITS);
    assert!(VPPN_MASK == (1usize << (VALEN - PAGE_SIZE_BITS - 1)) - 1);
    assert!(is_canonical_vaddr(MMAP_BASE));
    assert!(is_canonical_vaddr(MMAP_END));
    assert!(is_canonical_vaddr(KERNEL_PROGRAM_BASE));
    assert!(is_canonical_vaddr(KERNEL_STACK_BOTTOM));
    assert!(is_canonical_vaddr(KERNEL_STACK_TOP));
    assert!(
        canonicalize_vpn(KERNEL_STACK_TOP >> PAGE_SIZE_BITS) == KERNEL_STACK_TOP >> PAGE_SIZE_BITS
    );
    assert!(
        KERNEL_STACK_TOP - KERNEL_STACK_BOTTOM == KERNEL_STACK_SLOT_SIZE * KERNEL_STACK_MAX_SLOTS
    );
    assert!(MMAP_BASE < KERNEL_PROGRAM_BASE);
    assert!(KERNEL_PROGRAM_BASE < KERNEL_PROGRAM_END);
    assert!(KERNEL_PROGRAM_END <= MMAP_END);
    assert!(KERNEL_PROGRAM_END & PAGE_TABLE_VA_MASK == KERNEL_STACK_BOTTOM & PAGE_TABLE_VA_MASK);
};

#[cfg(feature = "boot_la_uboot_dmw")]
const _: () = {
    assert!(MEMORY_SIZE == 0x1000_0000 + 0x7000_0000);
    assert!(MEMORY_END == 0x1_0000_0000);
    assert!(MEMORY_REGIONS_FALLBACK[0].1 <= MEMORY_REGIONS_FALLBACK[1].0);
    assert!(FIRMWARE_RESERVED_REGIONS_FALLBACK[0].0 % PAGE_SIZE == 0);
    assert!(FIRMWARE_RESERVED_REGIONS_FALLBACK[0].1 % PAGE_SIZE == 0);
    assert!(MEMORY_REGIONS_FALLBACK[0].0 <= FIRMWARE_RESERVED_REGIONS_FALLBACK[0].0);
    assert!(FIRMWARE_RESERVED_REGIONS_FALLBACK[0].1 <= MEMORY_REGIONS_FALLBACK[0].1);
    assert!(USABLE_MEMORY_SIZE == 0x7cbf_3000);
    assert!(KERNEL_STACK_TOP == 0xFFFF_FFFF_FFFE_F000);
    assert!(KERNEL_STACK_BOTTOM == 0xFFFF_FFFF_F7BE_F000);
    assert!(KERNEL_PROGRAM_END == KERNEL_STACK_BOTTOM);
};

// QEMU 将传统内存磁盘镜像放在 RAM 起点以上 256MiB 处。
#[cfg(feature = "boot_la_qemu")]
pub const DISK_IMAGE_BASE: usize = 0x1000_0000 + MEMORY_START;
// 2K1000 上板阶段不启用该旧内存根路径；将占位地址放到帧分配器管理范围之外，避免与
// 内核镜像发生冲突。
#[cfg(feature = "boot_la_uboot_dmw")]
pub const DISK_IMAGE_BASE: usize = MEMORY_END;
// 256
pub const BUFFER_CACHE_NUM: usize = 256 * 1024 * 1024 / 2048 * 4 / 2048;

/// CPU0 在释放 AP 前发布的 stable-counter 频率；运行期只读。
pub static CLOCK_FREQ: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

use core::arch::asm;

#[macro_export]
macro_rules! signal_type {
    () => {
        u128
    };
}
#[macro_export]
macro_rules! def_cpu_cfg {
    ($name:ident, $num: literal) => {
        pub struct $name {
            bits: u32,
        }

        impl $name {
            // 读取index对应字的内容
            pub fn read() -> Self {
                let mut bits;
                bits = $num;
                // Safety: `cpucfg` only reads the selected CPU configuration
                // word into `bits`.
                unsafe {
                    asm!("cpucfg {},{}",out(reg) bits,in(reg) bits);
                }
                Self { bits }
            }
            pub fn get_bit(&self, index: usize) -> bool {
                bit_field::BitField::get_bit(&self.bits, index)
            }
            pub fn get_bits(&self, start: usize, end: usize) -> u32 {
                bit_field::BitField::get_bits(&self.bits, start..=end)
            }
        }
    };
}
def_cpu_cfg!(CPUCfg0, 0);
def_cpu_cfg!(CPUCfg1, 1);
def_cpu_cfg!(CPUCfg4, 4);
def_cpu_cfg!(CPUCfg5, 5);
impl CPUCfg1 {
    /// CPUCFG1.VABITS 的第 19:12 位保存硬件实现位宽减一后的值。
    pub fn get_valen(&self) -> usize {
        (self.get_bits(12, 19) + 1) as usize
    }
    /// CPUCFG1.PABITS 的第 11:4 位保存硬件实现位宽减一后的值。
    pub fn get_palen(&self) -> usize {
        (self.get_bits(4, 11) + 1) as usize
    }
}
#[macro_export]
macro_rules! newline {
    () => {
        "\r\n"
    };
}

#[macro_export]
macro_rules! should_map_trampoline {
    () => {
        true
    };
}

#[macro_export]
macro_rules! read_tot_sec16 {
    ($name:expr) => {{
        /// *KEEP IT THIS WAY!*
        /// Some arch relies on this for their compilers implement misaligned read so wrongly.
        #[inline(never)]
        fn misaligned_rd(super_block: &BPB) -> u16 {
            let ret: u16;
            // Safety: the inline assembly performs two byte loads from a valid
            // BPB reference and combines them without requiring aligned u16 access.
            unsafe {
                core::arch::asm!(
                    "
ld.bu   $a1, $a0, 0x14
ld.bu   $a0, $a0, 0x13
slli.d  $a1, $a1, 0x8
or      $a0, $a1, $a0
",
                    in("$a0") super_block,
                    lateout("$a0") ret
                )
            };
            ret
        }
        misaligned_rd($name)
    }};
}

#[macro_export]
macro_rules! read_root_ent_cnt {
    ($name:expr) => {{
        /// *KEEP IT THIS WAY!*
        /// Some arch relies on this for their compilers implement misaligned read so wrongly.
        #[inline(never)]
        fn misaligned_rd(super_block: &BPB) -> u16 {
            let ret: u16;
            // Safety: the inline assembly performs two byte loads from a valid
            // BPB reference and combines them without requiring aligned u16 access.
            unsafe {
                core::arch::asm!(
                    "
ld.bu   $a1, $a0, 0x12
ld.bu   $a0, $a0, 0x11
slli.d  $a1, $a1, 0x8
or      $a0, $a1, $a0
",
                    in("$a0") super_block,
                    lateout("$a0") ret
                )
            };
            ret
        }
        misaligned_rd($name)
    }};
}

#[macro_export]
macro_rules! read_byts_per_sec {
    ($name:expr) => {{
        /// *KEEP IT THIS WAY!*
        /// Some arch relies on this for their compilers implement misaligned read so wrongly.
        #[inline(never)]
        fn misaligned_rd(super_block: &BPB) -> u16 {
            let ret: u16;
            // Safety: the inline assembly performs two byte loads from a valid
            // BPB reference and combines them without requiring aligned u16 access.
            unsafe {
                core::arch::asm!(
                    "
ld.bu   $a1, $a0, 0xc
ld.bu   $a0, $a0, 0xb
slli.d  $a1, $a1, 0x8
or      $a0, $a1, $a0
",
                    in("$a0") super_block,
                    lateout("$a0") ret
                )
            };
            ret
        }
        misaligned_rd($name)
    }};
}

#[macro_export]
macro_rules! misaligned_wr {
    ($name:expr,$val:expr) => {};
}

#[macro_export]
macro_rules! copy_from_name1 {
    ($dst:expr,$name1:expr) => {{
        // Safety: `addr_of!` obtains raw addresses without forming references
        // to packed BPB fields.
        let mut dst = unsafe { core::ptr::addr_of!($dst[0]) as usize };
        // Safety: same raw-address extraction contract as above.
        let mut src = unsafe { core::ptr::addr_of!($name1) as usize };
        let mut x = 0;
        // First of all, the increment should be placed after the access.
        for _ in 0..10 {
            // Safety: callers pass BPB-compatible byte ranges of at least 10
            // bytes; copying is byte-wise to avoid alignment requirements.
            unsafe {
                *((dst) as *mut u8) = *((src) as *const u8);
            }
            dst += 1;
            src += 1;
        }
    }};
}

#[macro_export]
macro_rules! copy_to_name1 {
    ($name1:expr,$src:expr) => {{
        let k: [u16; 5] = $src;
        // Safety: `addr_of!` obtains raw addresses without forming references
        // to packed BPB fields.
        let mut dst = unsafe { core::ptr::addr_of!($name1) as usize };
        // Safety: local `k` is valid for the byte-wise copy below.
        let mut src = unsafe { core::ptr::addr_of!(k) as usize };
        // First of all, the increment should be placed after the access.
        for _ in 0..10 {
            // Safety: callers pass BPB-compatible byte ranges of at least 10
            // bytes; copying is byte-wise to avoid alignment requirements.
            unsafe {
                *((dst) as *mut u8) = *((src) as *const u8);
            }
            dst += 1;
            src += 1;
        }
    }};
}
