//! 硬件抽象层入口。
//!
//! 统一导出架构相关的启动、陷阱、页表、TLB、时钟、控制台和内核栈接口。
//! 上层内核代码应通过本模块访问架构能力，避免直接依赖 `arch/*` 的实现细节。
//!
//! # TLB
//!
//! 修改 PTE 后必须通过本模块导出的 `tlb_invalidate` 或页表接口自带的
//! flush 路径刷新 TLB。RISC-V 使用 `sfence.vma`，LoongArch64 使用
//! `invtlb`。

pub mod arch;
pub use arch::__switch;
pub use arch::config;
pub use arch::enable_local_timer_interrupt;
pub use arch::kernel_tlb_invalidate;
pub use arch::kstack_alloc;
pub use arch::program_timer_delta;
pub use arch::quiesce_local_timer_interrupt;
pub use arch::reclaim_retired_kernel_stacks;
pub use arch::remote_user_tlb_invalidate_range;
pub use arch::tlb_invalidate;
pub use arch::user_tlb_invalidate;
pub use arch::user_tlb_invalidate_page;
pub use arch::user_tlb_invalidate_range;
#[cfg(feature = "loongarch64")]
pub use arch::LsxRegs;
pub use arch::{
    boot_cpu_park, bootstrap_init, cpu_local_ptr, enter_secondary_idle, install_cpu_local,
    machine_init, prepare_secondary_cpu_stop, secondary_cpu_stop, secondary_cpu_wait, send_ipi,
    start_secondary_cpu, user_hwcap,
};
pub use arch::{console_flush, console_getchar, console_putchar, console_write_bytes};
pub use arch::{get_bad_addr, get_bad_instruction, get_exception_cause};
pub use arch::{get_clock_freq, get_time};
pub use arch::{local_irq_restore, local_irq_save};
pub use arch::{trap_cx_bottom_from_tid, ustack_bottom_from_tid};
pub use arch::{trap_handler, trap_return};
pub use arch::{
    KernelPageTableImpl, KernelStack, MachineContext, PageTableImpl, TrapContext, TrapImpl,
    UserContext, UserSignalMask,
};
pub use arch::{BLOCK_SZ, BUFFER_CACHE_NUM, KERNEL_HEAP_SIZE, MEMORY_END};
pub use arch::{MMIO, TICKS_PER_SEC};

/// 动态内核栈保留映射的上限。
///
/// 命中缓存时 slot 与物理页映射都不改变，因此无需跨核 TLB shootdown；只有
/// 缓存溢出、真正清除 PTE 时，才进入同步撤映射协议。
pub(crate) const KERNEL_STACK_CACHE_LIMIT: usize = 128;

/// 无堆分配的内核栈退休队列。
///
/// `KernelStack::drop` 可能运行在低内存或持有进程锁的路径，不能为了登记
/// shootdown 工作再触发堆分配。容量与架构的系统任务上限一致；超过上限说明
/// TCB/slot 生命周期已经突破任务配额不变量，直接 fail-stop 比覆盖旧条目安全。
pub(crate) struct KernelStackRetireQueue<const N: usize> {
    slots: [usize; N],
    len: usize,
}

impl<const N: usize> KernelStackRetireQueue<N> {
    pub(crate) const fn new() -> Self {
        Self {
            slots: [0; N],
            len: 0,
        }
    }

    pub(crate) fn push(&mut self, slot: usize) {
        assert!(self.len < N, "kernel-stack retire queue overflow");
        self.slots[self.len] = slot;
        self.len += 1;
    }

    pub(crate) fn pop(&mut self) -> Option<usize> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        Some(self.slots[self.len])
    }
}

/// 在一个受控闭包内临时开放本 CPU 的全局中断。
///
/// 该边界只能从已关中断的完整内核上下文进入。正常返回或
/// unwind 时 guard 都会先再次关中断，然后恢复入口状态。如果闭包经
/// `exit` 等路径永不返回，`schedule()` 会在切离任务前接管关中断。
/// 闭包最后一条指令与 guard 关闭之间到达的 timer 仍只发布 pending，
/// 由随后的 `trap_return()` 安全点消费。
pub fn with_local_interrupts_enabled<R>(f: impl FnOnce() -> R) -> R {
    struct InterruptWindowGuard {
        restore_enabled: bool,
    }

    impl Drop for InterruptWindowGuard {
        fn drop(&mut self) {
            // `local_irq_restore(false)` 当前不会主动清位，因此先无条件
            // 关闭，再按入口快照决定是否重新开放。
            let _ = arch::local_irq_save();
            arch::local_irq_restore(self.restore_enabled);
        }
    }

    let restore_enabled = arch::local_irq_save();
    assert!(
        !restore_enabled,
        "controlled interrupt window requires an IRQ-off entry"
    );
    let guard = InterruptWindowGuard { restore_enabled };
    arch::local_irq_restore(true);
    let result = f();
    drop(guard);
    result
}

/// 统一停机入口：先冻结所有 online AP，再执行架构机器关机。
///
/// `online` 尚未发布时可能处于极早期 panic，不能读取尚未安装的 CPU-local
/// 寄存器；此时直接走机器级兜底。当前运行期只有 CPU0 执行共享子系统，
/// AP 上的致命异常同样直接关机，避免反向 STOP CPU0 所需的未实现安全点。
pub fn shutdown() -> ! {
    let _ = arch::local_irq_save();
    let online = crate::smp::online_cpu_mask();
    if online & (1usize << crate::smp::BOOT_CPU_ID) != 0
        && crate::smp::cpu_id() == crate::smp::BOOT_CPU_ID
    {
        let _ = crate::smp::stop_secondary_cpus();
    }
    arch::machine_shutdown()
}

/// Per-chunk bounce buffer size for I/O operations.
/// Computed as KERNEL_HEAP_SIZE / 128, bounded to [64KiB, 256KiB].
/// For 32MiB heap → 256KiB chunk.
pub const IO_CHUNK_SIZE: usize = {
    let heap = KERNEL_HEAP_SIZE;
    let raw = heap / 128;
    if raw < 64 * 1024 {
        64 * 1024
    } else if raw > 256 * 1024 {
        256 * 1024
    } else {
        raw
    }
};

/// Maximum user-visible read/write count (Linux-compatible).
/// Equals i32::MAX rounded down to page alignment.
pub const MAX_RW_COUNT: usize = (i32::MAX as usize) & !(crate::config::PAGE_SIZE as usize - 1);
