//! 任务上下文保存区。
//!
//! `TaskContext` 是调度器切换时保存的最小内核态上下文，布局必须与
//! 架构相关 `switch` 汇编保持一致：返回地址、内核栈指针以及 callee-saved
//! 通用寄存器。

use crate::hal::trap_return;

#[repr(C)]
/// 调度器保存和恢复的内核态任务上下文。
///
/// # Semantics
///
/// 该结构只保存内核线程切换所需的寄存器，不包含用户态 trap context。
/// 字段顺序属于汇编 ABI，修改时必须同步 `hal::arch::*::switch`。
pub struct TaskContext {
    ra: usize,
    sp: usize,
    s: [usize; 12],
}

impl TaskContext {
    /// 返回全零上下文，用于占位或初始化后立即覆盖的场景。
    pub fn zero_init() -> Self {
        Self {
            ra: 0,
            sp: 0,
            s: [0; 12],
        }
    }

    /// 构造首次被调度时跳转到 `trap_return` 的上下文。
    ///
    /// # Semantics
    ///
    /// `kstack_ptr` 必须是该任务内核栈的栈顶。首次恢复该上下文时，调度器
    /// 通过 `ra = trap_return` 进入统一的返回用户态路径。
    pub fn goto_trap_return(kstack_ptr: usize) -> Self {
        Self {
            ra: trap_return as usize,
            sp: kstack_ptr,
            s: [0; 12],
        }
    }
}
