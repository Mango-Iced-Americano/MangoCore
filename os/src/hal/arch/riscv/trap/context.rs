//! RISC-V trap context 和用户信号上下文布局。
//!
//! 该文件定义从 trap 入口保存的通用寄存器、状态寄存器和 signal frame 需要的
//! 用户可见上下文结构。

use core::arch::asm;

use crate::task::{SignalStack, Signals};

const USER_UCONTEXT_SIGSET_SIZE: usize = 128;
const USER_CONTEXT_SIGMASK_PADDING: usize =
    USER_UCONTEXT_SIGSET_SIZE - core::mem::size_of::<UserSignalMask>();

/// General registers
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct GeneralRegs {
    pub pc: usize,
    pub ra: usize,
    pub sp: usize,
    pub gp: usize,
    pub tp: usize,
    pub t0: usize,
    pub t1: usize,
    pub t2: usize,
    pub s0: usize,
    pub s1: usize,
    pub a0: usize,
    pub a1: usize,
    pub a2: usize,
    pub a3: usize,
    pub a4: usize,
    pub a5: usize,
    pub a6: usize,
    pub a7: usize,
    pub s2: usize,
    pub s3: usize,
    pub s4: usize,
    pub s5: usize,
    pub s6: usize,
    pub s7: usize,
    pub s8: usize,
    pub s9: usize,
    pub s10: usize,
    pub s11: usize,
    pub t3: usize,
    pub t4: usize,
    pub t5: usize,
    pub t6: usize,
}

/// FP registers
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct FloatRegs {
    pub f: [usize; 32],
    pub fcsr: u32,
}

#[repr(C)]
#[derive(Default, Debug, Clone, Copy)]
pub struct MachineContext {
    gp: GeneralRegs,
    fp: FloatRegs,
}

#[repr(C)]
#[derive(Default, Debug, Clone, Copy)]
pub struct UserSignalMask {
    bits: [usize; 1],
}

impl UserSignalMask {
    pub fn from_signals(sigmask: Signals) -> Self {
        Self {
            bits: [sigmask.bits()],
        }
    }

    pub fn to_signals(self) -> Signals {
        Signals::from_bits_truncate(self.bits[0])
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct UserContext {
    pub flags: usize,
    pub link: usize,
    pub stack: SignalStack,
    pub sigmask: UserSignalMask,
    pub __pad: [u8; USER_CONTEXT_SIGMASK_PADDING],
    pub mcontext: MachineContext,
}

impl UserContext {
    pub const PADDING_SIZE: usize = USER_CONTEXT_SIGMASK_PADDING;
    pub const MCONTEXT_OFFSET: usize = core::mem::offset_of!(Self, mcontext);

    pub fn new(
        flags: usize,
        link: usize,
        stack: SignalStack,
        sigmask: Signals,
        mcontext: MachineContext,
    ) -> Self {
        Self {
            flags,
            link,
            stack,
            sigmask: UserSignalMask::from_signals(sigmask),
            __pad: [0; Self::PADDING_SIZE],
            mcontext,
        }
    }

    pub fn encode_sigmask(sigmask: Signals) -> UserSignalMask {
        UserSignalMask::from_signals(sigmask)
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
/// The trap cotext containing the user context and the supervisor level
pub struct TrapContext {
    /// The registers to be preserved.
    pub gp: GeneralRegs,
    pub fp: FloatRegs,
    /// A copy of register a0, useful when we need to restart syscall
    pub origin_a0: usize,
    /// trap context 保存的 `sstatus` 原始位模式，由返回汇编直接写回 CSR。
    pub sstatus: usize,
    /// Supervisor Address Translation and Protection
    pub kernel_satp: usize,
    /// The pointer to trap_handler
    pub trap_handler: usize,
    /// The current sp to be recovered on next entry into kernel space.
    pub kernel_sp: usize,
    /// PerCpu pointer reinstalled in `tp` after the user's TLS value is saved.
    pub kernel_cpu_local: usize,
}

// trap.S 直接按固定槽位读写这两个字段；布局变化必须在编译期失败。
const _: () = assert!(core::mem::offset_of!(TrapContext, sstatus) == 66 * 8);
const _: () = assert!(core::mem::offset_of!(TrapContext, kernel_cpu_local) == 70 * 8);

impl TrapContext {
    const SSTATUS_SIE: usize = 1 << 1;
    const SSTATUS_SPIE: usize = 1 << 5;
    const SSTATUS_SPP: usize = 1 << 8;

    /// 按值取得信号 ABI 需要保存的用户寄存器。
    ///
    /// 通过字段复制表达 `TrapContext -> MachineContext`，避免调用方依赖两种
    /// 结构当前恰好具有相同前缀布局。
    pub fn machine_context(&self) -> MachineContext {
        MachineContext {
            gp: self.gp,
            fp: self.fp,
        }
    }

    /// 恢复信号 ABI 中的用户寄存器，不覆盖内核私有 trap 元数据。
    pub fn set_machine_context(&mut self, context: MachineContext) {
        self.gp = context.gp;
        self.fp = context.fp;
    }

    /// 把保存态规范为“由 SRET 原子地返回用户态并重新开中断”。
    ///
    /// syscall 执行期间允许本地中断，因此 exec 新建上下文时读到的 live
    /// `sstatus.SIE` 可能为 1。恢复汇编仍运行在 S-mode；若提前写回该位，
    /// timer 可在通用寄存器只恢复一半时嵌套进入，破坏用户 `sp` 等现场。
    pub fn prepare_return(&mut self) {
        // SIE 必须保持关闭，直到最后一条 SRET 从 SPIE 原子恢复它。
        self.sstatus &= !Self::SSTATUS_SIE;
        // SPP=User 决定 SRET 的目标特权级。
        self.sstatus &= !Self::SSTATUS_SPP;
        // SPIE=1 使用户态重新获得正常的 supervisor interrupt 响应。
        self.sstatus |= Self::SSTATUS_SPIE;
    }

    pub fn set_sp(&mut self, sp: usize) {
        self.gp.sp = sp;
    }
    pub fn app_init_context(
        entry: usize,
        sp: usize,
        kernel_satp: usize,
        kernel_sp: usize,
        trap_handler: usize,
    ) -> Self {
        let sstatus: usize;
        // 保存当前 CSR 的其余字段（尤其是浮点状态），随后只规范返回相关位。
        unsafe { asm!("csrr {value}, sstatus", value = out(reg) sstatus) };
        let mut cx = Self {
            gp: GeneralRegs::default(),
            fp: FloatRegs::default(),
            origin_a0: 0,
            sstatus,
            kernel_satp,
            trap_handler,
            kernel_sp,
            kernel_cpu_local: 0,
        };
        cx.prepare_return();
        cx.gp.pc = entry;
        cx.set_sp(sp);
        cx
    }
}
