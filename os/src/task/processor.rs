use super::{
    __switch, do_wake_expired, take_one_interruptible_zombie, take_one_ready_zombie,
    take_zombie_tasks,
};
use super::{fetch_task, TaskStatus};
use super::{TaskContext, TaskControlBlock};
use crate::hal::TrapContext;
use crate::net::config::NET_INTERFACE;
use crate::task::signal::Signals;
use alloc::sync::Arc;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicUsize, Ordering};
use lazy_static::*;
use log;
use spin::Mutex;

const BACKGROUND_NET_POLL_INTERVAL: usize = 64;
const IDLE_NET_POLL_INTERVAL: usize = 64;
const RV64_CONSOLE_POLL_INTERVAL: usize = 64;

/// 处理器对象
pub struct Processor {
    /// 当前正在运行的任务
    current: Option<Arc<TaskControlBlock>>,
    /// 空闲任务的上下文，用于在任务切换时保存和恢复状态
    idle_task_cx: TaskContext,
}

impl Processor {
    /// 构造函数
    pub fn new() -> Self {
        Self {
            // 初始化时处理器为空闲
            current: None,
            // 空闲任务的上下文
            idle_task_cx: TaskContext::zero_init(),
        }
    }
    /// 获取空闲任务的上下文指针
    fn get_idle_task_cx_ptr(&mut self) -> *mut TaskContext {
        &mut self.idle_task_cx as *mut _
    }
    /// 取出当前正在运行的任务
    pub fn take_current(&mut self) -> Option<Arc<TaskControlBlock>> {
        // 将current字段置空，并返回其中的值
        self.current.take()
    }
    /// 获取当前正在运行的任务的克隆
    pub fn current(&self) -> Option<Arc<TaskControlBlock>> {
        self.current.as_ref().map(Arc::clone)
    }
    /// 检查当前 Processor 是否为空闲
    pub fn is_vacant(&self) -> bool {
        self.current.is_none()
    }
}

/// 当前正在执行的系统调用 ID（用于 OOM 诊断追踪）。
///
/// MangoCore 当前是单核调度，syscall 入口用全局原子即可避免每次 syscall
/// 都竞争 PROCESSOR 锁；0 表示无记录，实际 syscall id 存为 id + 1。
static CURRENT_SYSCALL_ID: AtomicUsize = AtomicUsize::new(0);

lazy_static! {
    /// 全局的处理器对象
    /// 使用 Mutex 包装以确保多线程安全
    pub static ref PROCESSOR: Mutex<Processor> = Mutex::new(Processor::new());
}

/// 运行任务调度
/// # 作用
/// 运行任务调度器，不断从任务队列中取出任务并运行
pub fn run_tasks() {
    let mut schedule_tick = 0usize;
    loop {
        schedule_tick = schedule_tick.wrapping_add(1);
        // Read one character from UART per iteration. Handle in priority order:
        // 1. Magic key (Ctrl+T) → trace dump + shutdown
        // 2. VINTR (Ctrl+C) → SIGINT to foreground/blocked task
        // 3. Normal character → stash for TTY
        //
        // On rv64 this is an SBI ecall, so do not pay it on every context
        // switch. TTY read paths still poll the console directly.
        #[cfg(target_arch = "riscv64")]
        let should_poll_console = schedule_tick % RV64_CONSOLE_POLL_INTERVAL == 0;
        #[cfg(not(target_arch = "riscv64"))]
        let should_poll_console = true;
        if should_poll_console {
            let ch = crate::hal::console_getchar() as u8;
            if ch != 0xFF {
                if crate::trace::check_magic_key(ch, "schedule") {
                    // check_magic_key → dump_from → shutdown, never returns.
                } else if crate::fs::dev::tty::Teletype::handle_vintr(ch) {
                    log::info!("[vintr-poll] SIGINT sent! ch={:#x}", ch);
                } else {
                    crate::trace::stash_char(ch);
                    crate::fs::dev::tty::Teletype::wake_readers();
                }
            }
        }
        // 处理到期内核定时器（SIGALRM 等），防止忙等待/轮询任务阻塞定时器投递。
        do_wake_expired();
        if schedule_tick % BACKGROUND_NET_POLL_INTERVAL == 0 {
            NET_INTERFACE.try_poll();
        }
        crate::fs::reclaim::maybe_reclaim_fs_caches();
        // 当前任务退出后先进入专用 zombie 队列；切回 idle 后即可安全 drop。
        // 这样避免把不可运行的 TCB 塞进 ready_queue 再扫描剔除。
        let zombies = take_zombie_tasks(64);
        let drained_zombies = zombies.len();
        drop(zombies);
        super::perf::record_zombie_drain(drained_zombies);
        // 兜底清理旧队列中的 zombie，避免异常路径留下不可运行任务。
        if schedule_tick % 64 == 0 {
            for _ in 0..8 {
                let a = take_one_ready_zombie();
                let b = take_one_interruptible_zombie();
                if a.is_none() && b.is_none() {
                    break;
                }
                drop(a);
                drop(b);
            }
        }
        // 降频清理 PROCESS_SHARED_FUTEX 空 WaitQueue 键
        super::threads::compact_shared_futex();
        let mut processor = PROCESSOR.lock();
        let next_task = fetch_task();
        super::perf::record_schedule_loop(next_task.is_some());
        if let Some(task) = next_task {
            let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();
            // 独占地访问即将运行的任务的 TCB
            let next_task_cx_ptr = {
                let mut task_inner = task.acquire_inner_lock();
                if task_inner.task_status == TaskStatus::Zombie {
                    drop(task_inner);
                    continue;
                }
                task_inner.task_status = TaskStatus::Running;
                task_inner.update_process_times_schedule_in();
                &task_inner.task_cx as *const TaskContext
            };
            // 设置当前正在运行的任务
            processor.current = Some(task);
            // 手动释放处理器
            drop(processor);
            unsafe {
                // 调用__switch 函数(汇编)切换任务
                __switch(idle_task_cx_ptr, next_task_cx_ptr);
            }
        } else {
            // 没有就绪的任务 → CPU idle
            drop(processor);
            if schedule_tick % IDLE_NET_POLL_INTERVAL == 0 {
                NET_INTERFACE.poll();
            } else {
                spin_loop();
            }
        }
    }
}

/// 取出当前正在运行的任务
pub fn take_current_task() -> Option<Arc<TaskControlBlock>> {
    PROCESSOR.lock().take_current()
}

/// 获取当前正在运行的任务
pub fn current_task() -> Option<Arc<TaskControlBlock>> {
    PROCESSOR.lock().current()
}

/// 获取当前系统调用名称（用于 OOM 诊断）
pub fn current_syscall_name() -> &'static str {
    match CURRENT_SYSCALL_ID.load(Ordering::Relaxed) {
        0 => "<none>",
        id => crate::syscall::syscall_name(id - 1),
    }
}

/// 设置当前系统调用 ID
pub fn set_current_syscall_id(id: Option<usize>) {
    CURRENT_SYSCALL_ID.store(id.map(|id| id + 1).unwrap_or(0), Ordering::Relaxed);
}

/// 检查当前任务是否有 OOM kill pending 标志。
/// 若有，立即发送 SIGKILL 并清除标志。
/// 这个函数在 trap_return() 中、do_signal() 之前调用，
/// 确保在当前上下文中无锁（除 task inner lock 外）的安全点处理 OOM。
pub fn check_oom_kill() {
    if let Some(task) = current_task() {
        let mut inner = task.acquire_inner_lock();
        if inner.pending_oom_kill {
            inner.pending_oom_kill = false;
            inner.add_signal(Signals::SIGKILL);
            log::warn!(
                "[OOM killer] tid {} pid {} marked for OOM kill, sending SIGKILL",
                task.tid.0,
                task.pid()
            );
        }
    }
}

/// 获取当前正在运行的任务的用户态页表令牌
pub fn current_user_token() -> usize {
    current_task().unwrap().get_user_token()
}

/// 获取当前正在运行的任务的陷阱上下文
pub fn current_trap_cx() -> &'static mut TrapContext {
    current_task().unwrap().acquire_inner_lock().get_trap_cx()
}

/// 切换到空闲任务上下文
pub fn schedule(switched_task_cx_ptr: *mut TaskContext) {
    // 获取空闲任务的上下文指针
    let idle_task_cx_ptr = PROCESSOR.lock().get_idle_task_cx_ptr();
    unsafe {
        // 调用__switch 函数(汇编)切换任务
        __switch(switched_task_cx_ptr, idle_task_cx_ptr);
    }
}
