use super::{
    __switch, do_wake_expired, take_one_interruptible_zombie, take_one_ready_zombie,
    take_zombie_tasks,
};
use super::{fetch_task, TaskStatus};
use super::{TaskContext, TaskControlBlock};
use crate::hal::TrapContext;
use crate::net::config::NET_INTERFACE;
use alloc::sync::Arc;
use core::hint::spin_loop;
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
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
static CURRENT_TASK_PTR: AtomicPtr<TaskControlBlock> = AtomicPtr::new(ptr::null_mut());
static CURRENT_PID: AtomicUsize = AtomicUsize::new(0);
static CURRENT_TID: AtomicUsize = AtomicUsize::new(0);
static CURRENT_PARENT_PID: AtomicUsize = AtomicUsize::new(0);
static CURRENT_USER_TOKEN: AtomicUsize = AtomicUsize::new(0);
static CURRENT_UID: AtomicUsize = AtomicUsize::new(0);
static CURRENT_EUID: AtomicUsize = AtomicUsize::new(0);
static CURRENT_GID: AtomicUsize = AtomicUsize::new(0);
static CURRENT_EGID: AtomicUsize = AtomicUsize::new(0);

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
            CURRENT_TASK_PTR.store(Arc::as_ptr(&task) as *mut TaskControlBlock, Ordering::Relaxed);
            CURRENT_PID.store(task.pid(), Ordering::Relaxed);
            CURRENT_TID.store(task.gettid(), Ordering::Relaxed);
            CURRENT_PARENT_PID.store(task.process.parent_pid(), Ordering::Relaxed);
            CURRENT_USER_TOKEN.store(task.process.user_token(), Ordering::Relaxed);
            CURRENT_UID.store(task.uid() as usize, Ordering::Relaxed);
            CURRENT_EUID.store(task.euid() as usize, Ordering::Relaxed);
            CURRENT_GID.store(task.gid() as usize, Ordering::Relaxed);
            CURRENT_EGID.store(task.egid() as usize, Ordering::Relaxed);
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
    CURRENT_TASK_PTR.store(ptr::null_mut(), Ordering::Relaxed);
    CURRENT_PID.store(0, Ordering::Relaxed);
    CURRENT_TID.store(0, Ordering::Relaxed);
    CURRENT_PARENT_PID.store(0, Ordering::Relaxed);
    CURRENT_USER_TOKEN.store(0, Ordering::Relaxed);
    CURRENT_UID.store(0, Ordering::Relaxed);
    CURRENT_EUID.store(0, Ordering::Relaxed);
    CURRENT_GID.store(0, Ordering::Relaxed);
    CURRENT_EGID.store(0, Ordering::Relaxed);
    PROCESSOR.lock().take_current()
}

/// 获取当前正在运行的任务
pub fn current_task() -> Option<Arc<TaskControlBlock>> {
    PROCESSOR.lock().current()
}

/// 获取当前正在运行任务的短生命周期引用。
///
/// MangoCore 当前是单核；调度器在 `PROCESSOR.current` 持有 Arc 时同步发布这个指针，
/// `take_current_task()` 会在切走当前任务前清空它。调用者不能把引用跨调度点保存。
pub fn current_task_ref() -> Option<&'static TaskControlBlock> {
    let ptr = CURRENT_TASK_PTR.load(Ordering::Relaxed);
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { &*ptr })
    }
}

#[inline(always)]
pub fn current_pid() -> usize {
    CURRENT_PID.load(Ordering::Relaxed)
}

#[inline(always)]
pub fn current_tid() -> usize {
    CURRENT_TID.load(Ordering::Relaxed)
}

#[inline(always)]
pub fn current_parent_pid() -> usize {
    CURRENT_PARENT_PID.load(Ordering::Relaxed)
}

#[inline(always)]
pub fn current_uid() -> u32 {
    CURRENT_UID.load(Ordering::Relaxed) as u32
}

#[inline(always)]
pub fn current_euid() -> u32 {
    CURRENT_EUID.load(Ordering::Relaxed) as u32
}

#[inline(always)]
pub fn current_gid() -> u32 {
    CURRENT_GID.load(Ordering::Relaxed) as u32
}

#[inline(always)]
pub fn current_egid() -> u32 {
    CURRENT_EGID.load(Ordering::Relaxed) as u32
}

pub fn refresh_current_user_token_for_process(pid: usize, token: usize) {
    if CURRENT_PID.load(Ordering::Relaxed) == pid {
        CURRENT_USER_TOKEN.store(token, Ordering::Relaxed);
    }
}

pub fn refresh_current_identity_hints_for_task(
    tid: usize,
    uid: u32,
    euid: u32,
    gid: u32,
    egid: u32,
) {
    if CURRENT_TID.load(Ordering::Relaxed) == tid {
        CURRENT_UID.store(uid as usize, Ordering::Relaxed);
        CURRENT_EUID.store(euid as usize, Ordering::Relaxed);
        CURRENT_GID.store(gid as usize, Ordering::Relaxed);
        CURRENT_EGID.store(egid as usize, Ordering::Relaxed);
    }
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

/// 获取当前正在运行的任务的用户态页表令牌
pub fn current_user_token() -> usize {
    let token = CURRENT_USER_TOKEN.load(Ordering::Relaxed);
    if token != 0 {
        token
    } else {
        current_task_ref().unwrap().get_user_token()
    }
}

/// 获取当前正在运行的任务的陷阱上下文
pub fn current_trap_cx() -> &'static mut TrapContext {
    current_task_ref().unwrap().acquire_inner_lock().get_trap_cx()
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
