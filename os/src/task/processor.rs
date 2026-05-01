use super::{__switch, do_wake_expired};
use super::{fetch_task, TaskStatus};
use super::{TaskContext, TaskControlBlock};
use crate::hal::TrapContext;
use crate::net::config::NET_INTERFACE;
use alloc::sync::Arc;
use lazy_static::*;
use spin::Mutex;

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

lazy_static! {
    /// 全局的处理器对象
    /// 使用 Mutex 包装以确保多线程安全
    pub static ref PROCESSOR: Mutex<Processor> = Mutex::new(Processor::new());
}

/// 运行任务调度
/// # 作用
/// 运行任务调度器，不断从任务队列中取出任务并运行
pub fn run_tasks() {
    loop {
        crate::trace::try_dump_from("schedule");
        // 处理到期内核定时器（SIGALRM 等），防止忙等待/轮询任务阻塞定时器投递。
        // 此前 do_wake_expired 仅在就绪队列为空时调用，但 pselect/ppoll/wait_io_core
        // 使用 suspend_current_and_run_next() 忙等待，导致就绪队列永不为空，
        // 令 KERNEL_TIMER_QUEUE 中的定时器（如服务端 setitimer 的 SIGALRM）无法投递，
        // 造成长达 120 秒的死锁。现改为每次调度迭代都处理，确保定时器及时触发。
        do_wake_expired();
        NET_INTERFACE.try_poll();
        // 获取全局处理器对象
        let mut processor = PROCESSOR.lock();
        // 尝试从全局变量 TASK_MANAGER 中取出一个任务
        if let Some(task) = fetch_task() {
            // 获取当前空闲任务的上下文指针
            let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();
            // 独占地访问即将运行的任务的 TCB
            let next_task_cx_ptr = {
                let mut task_inner = task.acquire_inner_lock();
                task_inner.task_status = TaskStatus::Running;
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
            NET_INTERFACE.poll();
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
