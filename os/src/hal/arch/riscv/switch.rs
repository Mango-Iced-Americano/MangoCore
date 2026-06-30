//! RISC-V 汇编上下文切换入口。
//!
//! `__switch` 保存当前任务 callee-saved 寄存器并恢复下一个任务上下文。

use crate::task::TaskContext;
use core::arch::global_asm;

global_asm!(include_str!("switch.S"));

extern "C" {
    pub fn __switch(current_task_cx_ptr: *mut TaskContext, next_task_cx_ptr: *const TaskContext);
}
