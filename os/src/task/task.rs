use super::pid::RecycleAllocator;
use super::process::ProcessControlBlock;
use super::registry;
use super::signal::*;
use super::threads::Futex;
use super::TaskContext;
use super::{
    tid_alloc, trap_cx_bottom_from_slot, ustack_bottom_from_slot, TidHandle,
};
use crate::config::MMAP_BASE;
use crate::fs::vfs;
use crate::fs::{vfs_lookup_absolute, vfs_root};
use crate::hal::TrapImpl;
use crate::hal::{kstack_alloc, KernelStack};
use crate::hal::{trap_handler, TrapContext};
use crate::mm::PageTableImpl;
use crate::mm::{AddressSpace, PhysPageNum, VirtAddr, KERNEL_SPACE};
use crate::syscall::CloneFlags;
use crate::syscall::errno::ENOMEM;
use crate::timer::{ITimerVal, TimeSpec, TimeVal};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::{self, Debug, Formatter};
use core::sync::atomic::AtomicBool;
use log::{trace, warn};
use spin::{Mutex, MutexGuard};

#[derive(Clone)]
/// 任务的文件系统状态
pub struct FsStatus {
    /// 当前工作目录的文件（新 VFS）
    pub working_inode: Arc<vfs::File>,
    /// 当前工作目录的绝对路径字符串（用于 getcwd，避免依赖 broken 的 absolute_path()）
    pub working_path: String,
}

/// 任务控制块
pub struct TaskControlBlock {
    // 不可变字段
    /// 用户可见线程 ID，即 gettid() 返回值
    pub tid: TidHandle,
    /// 同一地址空间内 trap context / 默认用户栈的资源槽位
    pub user_res_slot: usize,
    /// 所属用户可见进程
    pub process: Arc<ProcessControlBlock>,
    /// 内核栈
    pub kstack: KernelStack,
    /// 用户栈基址
    pub ustack_base: usize,
    /// 退出信号
    pub exit_signal: Signals,
    // 可变字段
    /// 任务内部状态，使用互斥锁保护
    inner: Mutex<TaskControlBlockInner>,
    // 可共享&可变字段
    /// I/O 等待定时器是否已挂入 KERNEL_TIMER_QUEUE。
    /// 为 true 时，wait_io_core_with_queue 不再添加第二个定时器（Option B），
    /// 防止在 log=off 的高频 loopback accept/connect 循环中 KERNEL_TIMER_QUEUE 无限增长。
    /// 定时器触发后，run_timer 会无条件清回 false（Option A）。
    pub wait_io_timer_pending: AtomicBool,
}

/// 任务控制块内部状态
pub struct TaskControlBlockInner {
    /// 信号掩码
    pub sigmask: Signals,
    /// sigsuspend 临时替换 mask 时保存的旧 mask，由 sigreturn 恢复。
    pub sigmask_to_restore: Option<Signals>,
    /// 待处理信号
    pub sigpending: SignalQueue,
    /// 备用信号栈，每线程独立
    pub signal_stack: SignalStack,
    /// 陷阱上下文的物理页号
    pub trap_cx_ppn: PhysPageNum,
    /// 任务上下文
    pub task_cx: TaskContext,
    /// 任务状态
    pub task_status: TaskStatus,
    /// 用于清理子进程的线程ID
    pub clear_child_tid: usize,
    /// 鲁棒列表，用于管理鲁棒互斥锁
    pub robust_list: RobustList,
    /// 资源使用情况
    pub rusage: Rusage,
    /// 任务的时钟信息
    pub clock: ProcClock,
    /// 定时器
    pub timer: [ITimerVal; 3],
    /// ITIMER_REAL 的真实时间到期点
    pub real_timer_deadline: Option<TimeSpec>,
    /// ITIMER_REAL 的版本号，用于让旧TimerQueue节点失效
    pub real_timer_generation: usize,
    /// OOM killer pending 标志：分配器已耗尽，本进程将在 trap_return 时被杀死
    pub pending_oom_kill: bool,
}

#[derive(Clone, Copy, Debug)]
/// 表示任务的鲁棒列表
/// 用于管理鲁棒互斥锁
pub struct RobustList {
    /// 链表头
    pub head: usize,
    /// 链表长度
    pub len: usize,
}

impl RobustList {
    // from strace
    // 默认的链表头大小
    pub const HEAD_SIZE: usize = 24;
}

impl Default for RobustList {
    /// 初始化方法
    fn default() -> Self {
        Self {
            // 链表头
            head: 0,
            // 链表长度
            len: Self::HEAD_SIZE,
        }
    }
}

#[repr(C)]
/// 进程时钟
/// 表示任务的时钟信息
pub struct ProcClock {
    /// 上次进入用户态的时间
    last_enter_u_mode: TimeVal,
    /// 上次进入内核态的时间
    last_enter_s_mode: TimeVal,
    //  上次更新real计时器的时间
    pub last_real_timer_update: TimeVal,
}

impl ProcClock {
    /// 构造函数
    pub fn new() -> Self {
        // 获取当前时间
        let now = TimeVal::now();
        Self {
            last_enter_u_mode: now,
            last_enter_s_mode: now,
            last_real_timer_update: now,
        }
    }
}

#[allow(unused)]
#[derive(Clone, Copy)]
#[repr(C)]
/// 资源使用情况
pub struct Rusage {
    /// 用户CPU时间
    pub ru_utime: TimeVal, /* user CPU time used */
    /// 系统CPU时间
    pub ru_stime: TimeVal, /* system CPU time used */
    /// 以下字段未实现，用于后续扩展
    ru_maxrss: isize, // NOT IMPLEMENTED /* maximum resident set size */
    ru_ixrss: isize,    // NOT IMPLEMENTED /* integral shared memory size */
    ru_idrss: isize,    // NOT IMPLEMENTED /* integral unshared data size */
    ru_isrss: isize,    // NOT IMPLEMENTED /* integral unshared stack size */
    ru_minflt: isize,   // NOT IMPLEMENTED /* page reclaims (soft page faults) */
    ru_majflt: isize,   // NOT IMPLEMENTED /* page faults (hard page faults) */
    ru_nswap: isize,    // NOT IMPLEMENTED /* swaps */
    ru_inblock: isize,  // NOT IMPLEMENTED /* block input operations */
    ru_oublock: isize,  // NOT IMPLEMENTED /* block output operations */
    ru_msgsnd: isize,   // NOT IMPLEMENTED /* IPC messages sent */
    ru_msgrcv: isize,   // NOT IMPLEMENTED /* IPC messages received */
    ru_nsignals: isize, // NOT IMPLEMENTED /* signals received */
    ru_nvcsw: isize,    // NOT IMPLEMENTED /* voluntary context switches */
    ru_nivcsw: isize,   // NOT IMPLEMENTED /* involuntary context switches */
}

impl Rusage {
    /// 构造函数
    pub fn new() -> Self {
        Self {
            // 初始化为0
            ru_utime: TimeVal::new(),
            // 初始化为0
            ru_stime: TimeVal::new(),
            ru_maxrss: 0,
            ru_ixrss: 0,
            ru_idrss: 0,
            ru_isrss: 0,
            ru_minflt: 0,
            ru_majflt: 0,
            ru_nswap: 0,
            ru_inblock: 0,
            ru_oublock: 0,
            ru_msgsnd: 0,
            ru_msgrcv: 0,
            ru_nsignals: 0,
            ru_nvcsw: 0,
            ru_nivcsw: 0,
        }
    }
}

impl Debug for Rusage {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!(
            "(ru_utime:{:?}, ru_stime:{:?})",
            self.ru_utime, self.ru_stime
        ))
    }
}

impl TaskControlBlockInner {
    /// 获取陷阱上下文
    pub fn get_trap_cx(&self) -> &'static mut TrapContext {
        self.trap_cx_ppn.get_mut()
    }
    /// 获取任务状态
    fn get_status(&self) -> TaskStatus {
        self.task_status
    }
    /// 判断是否为僵尸态
    pub fn is_zombie(&self) -> bool {
        self.get_status() == TaskStatus::Zombie
    }
    /// 添加信号
    pub fn add_signal(&mut self, signal: Signals) {
        let _ = self.sigpending.enqueue_signal(signal, 0);
    }
    /// 在进入陷阱时更新进程时间
    pub fn update_process_times_enter_trap(&mut self) {
        // 获取当前时间
        let now = TimeVal::now();
        // 更新上次进入内核态的时间
        self.clock.last_enter_s_mode = now;
        // 计算时间差
        let diff = now - self.clock.last_enter_u_mode;
        // 更新用户CPU时间
        self.rusage.ru_utime = self.rusage.ru_utime + diff;
        // 更新虚拟定时器
        self.update_itimer_virtual_if_exists(diff);
        // 更新性能分析定时器
        self.update_itimer_prof_if_exists(diff);
    }
    /// 在离开陷阱时更新进程时间
    pub fn update_process_times_leave_trap(&mut self, trap_cause: TrapImpl) {
        let now = TimeVal::now();
        if trap_cause.is_timer() {
            let diff = now - self.clock.last_enter_s_mode;
            self.rusage.ru_stime = self.rusage.ru_stime + diff;
            self.update_itimer_prof_if_exists(diff);
        }
        self.clock.last_enter_u_mode = now;
    }
    /// 更新实时定时器
    pub fn update_itimer_real_if_exists(&mut self, diff: TimeVal) {
        // 如果当前定时器不为0
        if !self.timer[0].it_value.is_zero() {
            // 更新定时器
            self.timer[0].it_value = self.timer[0].it_value - diff;
            // 如果定时器为0
            if self.timer[0].it_value.is_zero() {
                // 添加信号
                self.add_signal(Signals::SIGALRM);
                log::info!("Task's real timer expired, sending SIGALRM");
                // 重置定时器
                self.timer[0].it_value = self.timer[0].it_interval;
            }
        }
    }
    /// 更新虚拟定时器
    /// 与上面的更新实时定时器类似
    /// 但是发送的信号是SIGVTALRM
    pub fn update_itimer_virtual_if_exists(&mut self, diff: TimeVal) {
        if !self.timer[1].it_value.is_zero() {
            self.timer[1].it_value = self.timer[1].it_value - diff;
            if self.timer[1].it_value.is_zero() {
                self.add_signal(Signals::SIGVTALRM);
                self.timer[1].it_value = self.timer[1].it_interval;
            }
        }
    }
    /// 更新性能分析定时器
    /// 与上面的更新实时定时器类似
    /// 但是发送的信号是SIGPROF
    pub fn update_itimer_prof_if_exists(&mut self, diff: TimeVal) {
        if !self.timer[2].it_value.is_zero() {
            self.timer[2].it_value = self.timer[2].it_value - diff;
            if self.timer[2].it_value.is_zero() {
                self.add_signal(Signals::SIGPROF);
                self.timer[2].it_value = self.timer[2].it_interval;
            }
        }
    }

    pub fn refresh_real_timer(&mut self) {
        let now = TimeVal::now();
        let diff = now - self.clock.last_real_timer_update;
        log::debug!("real_timer refreshing...");
        self.update_itimer_real_if_exists(diff);
        // 更新锚点，防止重复计算
        self.clock.last_real_timer_update = now;
    }
}

fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

impl TaskControlBlock {
    /// 获取任务内部状态的互斥锁
    pub fn acquire_inner_lock(&self) -> MutexGuard<TaskControlBlockInner> {
        self.inner.lock()
    }
    /// 获取陷阱上下文的用户虚拟地址
    pub fn trap_cx_user_va(&self) -> usize {
        trap_cx_bottom_from_slot(self.user_res_slot)
    }
    /// 获取用户栈的用户虚拟地址
    pub fn ustack_bottom_va(&self) -> usize {
        ustack_bottom_from_slot(self.user_res_slot)
    }
    /// !!!!!!!!!!!!!!!!WARNING!!!!!!!!!!!!!!!!!!!!!
    /// 当前仅用于initproc加载。如果在其他地方使用，必须更改bin_path。
    /// 任务创建（仅用于initproc）
    pub fn new(elf: vfs::File) -> Arc<Self> {
        // 将ELF文件映射到内核空间
        let elf_data = elf.map_to_kernel_space(MMAP_BASE);
        log::debug!(
            "[TCB::new] elf_data.len() = {} (first 16 bytes: {:02X?})",
            elf_data.len(),
            &elf_data[..16.min(elf_data.len())]
        );
        // 带有ELF程序头/跳板的用户地址空间（AddressSpace）
        // 解析ELF文件，初始化内存映射
        let (mut memory_set, _user_heap, elf_info) =
            AddressSpace::<PageTableImpl>::from_elf(elf_data).unwrap();
        // 在内核空间中删除ELF区域
        crate::mm::KERNEL_SPACE
            .lock()
            .remove_area_with_start_vpn(VirtAddr::from(MMAP_BASE).floor())
            .unwrap();

        // 获取用户资源槽位分配器
        let user_res_slot_allocator = Arc::new(Mutex::new(RecycleAllocator::new()));
        // 在内核空间中分配一个用户可见 tid 和一个内核栈
        let tid_handle = tid_alloc();
        // 分配当前地址空间内的用户资源槽位
        let user_res_slot = user_res_slot_allocator.lock().alloc();
        // 初始进程的 pid/pgid 与主线程 tid 相同
        let pid = tid_handle.0;
        let pgid = tid_handle.0;
        // 分配内核栈
        let kstack = kstack_alloc();
        // 获取内核栈的顶部
        let kstack_top = kstack.get_top();

        // 为当前线程分配用户资源
        memory_set.alloc_user_res(user_res_slot, true);
        // 获取陷阱上下文的物理页号
        let trap_cx_ppn = memory_set
            .translate(VirtAddr::from(trap_cx_bottom_from_slot(user_res_slot)).into())
            .unwrap();
        log::trace!("[TCB::new]trap_cx_ppn{:?}", trap_cx_ppn);

        // 初始化新 VFS 文件描述符表
        let mut fd_table = vfs::FdTable::new();
        // 打开 /dev/tty 并分配 stdin/stdout/stderr（fd 0, 1, 2）
        let tty_inode = vfs_lookup_absolute("/dev/tty").unwrap();
        let tty_file = vfs::File::new(tty_inode, vfs::FileFlags::O_RDWR).unwrap();
        fd_table.alloc_fd(tty_file, false).unwrap();
        let tty_inode = vfs_lookup_absolute("/dev/tty").unwrap();
        let tty_file = vfs::File::new(tty_inode, vfs::FileFlags::O_RDWR).unwrap();
        fd_table.alloc_fd(tty_file, false).unwrap();
        let tty_inode = vfs_lookup_absolute("/dev/tty").unwrap();
        let tty_file = vfs::File::new(tty_inode, vfs::FileFlags::O_RDWR).unwrap();
        fd_table.alloc_fd(tty_file, false).unwrap();

        // 初始化工作目录为根目录
        let root_inode = vfs_root().mountpoint_root_inode();
        let cwd = vfs::File::new(
            root_inode,
            vfs::FileFlags::O_RDONLY | vfs::FileFlags::O_DIRECTORY,
        )
        .unwrap();

        let process = Arc::new(ProcessControlBlock::new(
            pid,
            tid_handle.0,
            pgid,
            None,
            Arc::new(Mutex::new(elf)),
            String::new(),
            Arc::new(Mutex::new(fd_table)),
            Arc::new(Mutex::new(FsStatus {
                working_inode: Arc::new(cwd),
                working_path: String::from("/"),
            })),
            Arc::new(Mutex::new(memory_set)),
            Arc::new(Mutex::new(Sighand::new())),
            Arc::new(Mutex::new(Futex::new())),
            user_res_slot_allocator,
        ));

        // 创建任务控制块
        let task_control_block = Arc::new(Self {
            tid: tid_handle,
            user_res_slot,
            process,
            kstack,
            ustack_base: ustack_bottom_from_slot(user_res_slot),
            exit_signal: Signals::empty(),
            wait_io_timer_pending: AtomicBool::new(false),
            inner: Mutex::new(TaskControlBlockInner {
                sigmask: Signals::empty(),
                sigmask_to_restore: None,
                sigpending: SignalQueue::empty(),
                signal_stack: SignalStack::disabled(),
                trap_cx_ppn,
                task_cx: TaskContext::goto_trap_return(kstack_top),
                task_status: TaskStatus::Ready,
                clear_child_tid: 0,
                robust_list: RobustList::default(),
                rusage: Rusage::new(),
                clock: ProcClock::new(),
                timer: [ITimerVal::new(); 3],
                real_timer_deadline: None,
                real_timer_generation: 0,
                pending_oom_kill: false,
            }),
        });
        task_control_block.process.add_thread(&task_control_block);
        registry::register_process(&task_control_block.process);
        registry::register_task(&task_control_block);
        // 准备用户空间的陷阱上下文
        let trap_cx = task_control_block.acquire_inner_lock().get_trap_cx();
        // 初始化陷阱上下文
        *trap_cx = TrapContext::app_init_context(
            elf_info.entry,
            ustack_bottom_from_slot(user_res_slot),
            KERNEL_SPACE.lock().token(),
            kstack_top,
            trap_handler as usize,
        );
        trace!("[new] trap_cx:{:?}", *trap_cx);
        task_control_block
    }

    /// 加载ELF文件
    pub fn load_elf(
        &self,
        elf: vfs::File,
        argv_vec: &Vec<String>,
        envp_vec: &Vec<String>,
    ) -> Result<(), isize> {
        // 旧 VM 没有被其他 CLONE_VM 进程共享时，可以先释放用户数据页，
        // 避免新旧内存集同时存在导致双倍内存压力触发 OOM。
        // 如果旧 VM 被共享（典型是 CLONE_VM | CLONE_VFORK），exec 必须先
        // 构造新地址空间，提交时再让当前进程脱离共享 VM，不能破坏父进程。
        let current_vm = self.process.vm();
        if Arc::strong_count(&current_vm) <= 2 {
            current_vm.lock().recycle_data_pages();
        }

        // 将ELF文件映射到内核空间
        let elf_data = elf.map_to_kernel_space(MMAP_BASE);
        // 带有ELF程序头/跳板/陷阱上下文/用户栈的用户地址空间（AddressSpace）
        let load_result = AddressSpace::from_elf(elf_data);

        // 清除临时映射
        crate::mm::KERNEL_SPACE
            .lock()
            .remove_area_with_start_vpn(VirtAddr::from(MMAP_BASE).floor())
            .unwrap();

        let (mut memory_set, program_break, elf_info) = match load_result {
            Ok(result) => result,
            Err(e) => return Err(e),
        };
        log::trace!("[load_elf] ELF file mapped");

        // 为 glibc 分配用户 heap 空间（0x1c0000 ~ 0x1c4000）
        use crate::mm::{MapPermission, VirtAddr};

        let page_size = 0x1000;
        let heap_start = align_up(program_break, page_size);
        let heap_end = heap_start + 0x20000; // 64KiB
        memory_set.insert_framed_area(
            VirtAddr::from(heap_start),
            VirtAddr::from(heap_end),
            MapPermission::R | MapPermission::W | MapPermission::U,
        );
        log::info!(
            "[load_elf] mapped user heap from program_break: {:#x} ~ {:#x}",
            heap_start,
            heap_end
        );

        // 为当前线程分配用户资源
        memory_set.alloc_user_res(self.user_res_slot, true);
        // 创建ELF参数表
        let user_sp =
            memory_set.create_elf_tables(self.ustack_bottom_va(), argv_vec, envp_vec, &elf_info)?;
        log::trace!("[load_elf] user sp after pushing parameters: {:X}", user_sp);
        // 初始化陷阱上下文
        let trap_cx = TrapContext::app_init_context(
            if let Some(interp_entry) = elf_info.interp_entry {
                interp_entry
            } else {
                elf_info.entry
            },
            // 用户栈指针
            user_sp,
            // 内核页表令牌
            KERNEL_SPACE.lock().token(),
            // 内核栈顶
            self.kstack.get_top(),
            // 陷阱处理函数地址
            trap_handler as usize,
        );
        let other_threads: Vec<_> = self
            .process
            .threads()
            .into_iter()
            .filter(|task| task.tid.0 != self.tid.0)
            .collect();
        for task in &other_threads {
            // execve 会杀掉同线程组的其他线程，但保留当前 process。
            super::exit_thread(task.clone(), Signals::SIGKILL.to_signum().unwrap() as u32);
        }
        super::remove_tasks_from_queues(&other_threads);

        {
            // **** 保持当前PCB锁
            let mut inner = self.acquire_inner_lock();
            // 更新陷阱上下文的物理页号
            inner.trap_cx_ppn = (&memory_set)
                .translate(VirtAddr::from(self.trap_cx_user_va()).into())
                .unwrap();
            // 更新任务上下文
            *inner.get_trap_cx() = trap_cx;
            // 重置clear_child_tid
            inner.clear_child_tid = 0;
            // 重置robust_list
            inner.robust_list = RobustList::default();
            // execve disables the alternate signal stack.
            inner.signal_stack = SignalStack::disabled();
        }
        if Arc::strong_count(&current_vm) > 2 {
            // CLONE_VM/vfork 子进程 exec 后会脱离旧地址空间。这里仅从旧 VM
            // 中移除当前线程的 trap context/默认栈映射，不释放 slot 号本身；
            // 新 VM 仍使用同一个 user_res_slot，避免父 VM 留下孤儿映射。
            current_vm.lock().dealloc_user_res(self.user_res_slot);
        }
        // 更新可执行文件描述符
        self.process.replace_exe(elf);
        // 清理资源 — 关闭所有 CLOEXEC 文件描述符
        self.process.files().lock().close_cloexec();
        // 替换内存映射
        self.process.replace_vm(memory_set);
        // 清空信号处理函数表
        self.process.sighand().lock().reset();
        // 清空futex
        self.process.futex().lock().clear();
        Ok(())
        // **** 释放当前PCB锁
    }
    /// 创建新的任务控制块
    pub fn sys_clone(
        self: &Arc<TaskControlBlock>,
        flags: CloneFlags,
        stack: *const u8,
        tls: usize,
        exit_signal: Signals,
    ) -> Result<Arc<TaskControlBlock>, isize> {
        // ---- 保持父PCB锁
        let parent_inner = self.acquire_inner_lock();
        // 复制用户空间（包括陷阱上下文）
        let share_vm = flags.contains(CloneFlags::CLONE_VM);
        let parent_vm = self.process.vm();
        let memory_set = if share_vm {
            parent_vm.clone() // 共享虚拟内存空间（线程）
        } else {
            // 复制地址空间（进程）
            crate::mm::frame_reserve(16);
            Arc::new(Mutex::new(AddressSpace::from_existing_user(
                &mut parent_vm.lock(),
            )?))
        };

        // 共享地址空间时，trap context 的虚拟地址也共享，必须复用同一个用户资源槽位分配器。
        let user_res_slot_allocator = if share_vm {
            self.process.user_res_slot_allocator()
        } else {
            Arc::new(Mutex::new(RecycleAllocator::new()))
        };
        // 在内核空间分配一个用户可见 tid 和一个内核栈
        let tid_handle = tid_alloc();
        let user_res_slot = user_res_slot_allocator.lock().alloc();
        let process = if flags.contains(CloneFlags::CLONE_THREAD) {
            self.process.clone()
        } else {
            let parent_process = if flags.contains(CloneFlags::CLONE_PARENT) {
                self.process.parent()
            } else {
                Some(self.process.clone())
            };
            let files = if flags.contains(CloneFlags::CLONE_FILES) {
                self.process.files()
            } else {
                Arc::new(Mutex::new(
                    self.process
                        .files()
                        .lock()
                        .try_clone()
                        .map_err(|e| e as isize)?,
                ))
            };
            let fs = if flags.contains(CloneFlags::CLONE_FS) {
                self.process.fs()
            } else {
                Arc::new(Mutex::new(self.process.fs().lock().clone()))
            };
            let sighand = if flags.contains(CloneFlags::CLONE_SIGHAND) {
                self.process.sighand()
            } else {
                let sighand = self.process.sighand();
                let lock = sighand.lock();
                Arc::new(Mutex::new(Sighand::from_existing(&lock)))
            };
            let futex = if share_vm {
                self.process.futex()
            } else {
                Arc::new(Mutex::new(Futex::new()))
            };
            Arc::new(ProcessControlBlock::new(
                tid_handle.0,
                tid_handle.0,
                self.process.getpgid(),
                parent_process.as_ref().map(Arc::downgrade),
                self.process.exe(),
                self.process.exe_path(),
                files,
                fs,
                memory_set.clone(),
                sighand,
                futex,
                user_res_slot_allocator.clone(),
            ))
        };
        // 分配内核栈
        let kstack = kstack_alloc();
        let kstack_top = kstack.get_top();

        // 共享 VM 的任务需要独立 trap context；用户栈只在未指定 child stack 时分配。
        if share_vm {
            memory_set.lock().alloc_user_res(
                user_res_slot,
                stack.is_null() && !flags.contains(CloneFlags::CLONE_VFORK),
            );
        }
        // 获取陷阱上下文的物理页号
        let trap_cx_va = trap_cx_bottom_from_slot(user_res_slot);
        let trap_cx_ppn = match memory_set
            .lock()
            .translate(VirtAddr::from(trap_cx_va).into())
        {
            Some(ppn) => ppn,
            None => {
                warn!(
                    "[sys_clone] trap context is not mapped after alloc_user_res: slot={}, va={:#x}",
                    user_res_slot, trap_cx_va
                );
                memory_set.lock().dealloc_user_res(user_res_slot);
                user_res_slot_allocator.lock().dealloc(user_res_slot);
                return Err(ENOMEM);
            }
        };

        // 创建任务控制块
        let task_control_block = Arc::new(TaskControlBlock {
            // 基础标识信息
            tid: tid_handle,
            user_res_slot,
            process,
            kstack,
            ustack_base: if !stack.is_null() {
                stack as usize
            } else {
                ustack_bottom_from_slot(user_res_slot)
            },
            exit_signal,
            wait_io_timer_pending: AtomicBool::new(false),
            inner: Mutex::new(TaskControlBlockInner {
                // clone
                sigpending: SignalQueue::empty(),
                signal_stack: if share_vm {
                    SignalStack::disabled()
                } else {
                    parent_inner.signal_stack
                },
                // new
                rusage: Rusage::new(),
                clock: ProcClock::new(),
                clear_child_tid: 0,
                robust_list: RobustList::default(),
                timer: [ITimerVal::new(); 3],
                real_timer_deadline: None,
                real_timer_generation: 0,
                sigmask: Signals::empty(),
                sigmask_to_restore: None,
                // compute
                trap_cx_ppn,
                task_cx: TaskContext::goto_trap_return(kstack_top),
                // constants
                task_status: TaskStatus::Ready,
                pending_oom_kill: false,
            }),
        });
        // 初始化陷阱上下文
        let trap_cx = task_control_block.acquire_inner_lock().get_trap_cx();
        // 共享 VM 时新分配的 trap context 为空，需要从父任务当前上下文复制。
        if share_vm {
            *trap_cx = *parent_inner.get_trap_cx();
        }
        // we also do not need to prepare parameters on stack, musl has done it for us
        // 处理用户栈指针
        if !stack.is_null() {
            trap_cx.gp.sp = stack as usize;
        }
        // 设置线程寄存器
        if flags.contains(CloneFlags::CLONE_SETTLS) {
            // thread local storage
            // 线程局部存储
            trap_cx.gp.tp = tls;
        }
        // 对于子进程，fork返回0
        trap_cx.gp.a0 = 0;
        // 修改陷阱上下文中的内核栈指针
        trap_cx.kernel_sp = kstack_top;
        task_control_block.process.add_thread(&task_control_block);
        if !flags.contains(CloneFlags::CLONE_THREAD) {
            registry::register_process(&task_control_block.process);
        }
        registry::register_task(&task_control_block);
        // 返回
        Ok(task_control_block)
        // ---- 释放父PCB锁
    }
    /// Publish a successfully initialized clone into the waitable child tree.
    /// `CLONE_THREAD` tasks are not waitable children and are only scheduled.
    pub fn publish_clone_child(
        self: &Arc<TaskControlBlock>,
        child: Arc<TaskControlBlock>,
        flags: CloneFlags,
    ) -> Result<(), isize> {
        if flags.contains(CloneFlags::CLONE_THREAD) {
            return Ok(());
        }
        if flags.contains(CloneFlags::CLONE_PARENT) {
            let parent = child.process.parent();
            if let Some(parent) = parent {
                parent.add_child(child.process.clone())?;
            } else {
                warn!("[publish_clone_child] CLONE_PARENT target parent is gone");
            }
        } else {
            self.process.add_child(child.process.clone())?;
        }
        Ok(())
    }

    /// Drop resources allocated for a clone that has not been published.
    pub fn cleanup_unpublished_clone(&self, shared_vm: bool) {
        if shared_vm {
            self.process.vm().lock().dealloc_user_res(self.user_res_slot);
        }
    }

    /// 获取用户可见线程 ID。
    pub fn gettid(&self) -> usize {
        self.tid.0
    }

    /// 获取用户可见进程 ID。
    pub fn getpid(&self) -> usize {
        self.process.pid
    }
    /// 获取用户可见进程 ID。
    pub fn pid(&self) -> usize {
        self.process.pid
    }
    /// 设置进程组ID
    pub fn setpgid(&self, pgid: usize) -> isize {
        self.process.setpgid(pgid)
    }
    // 获取进程组ID
    pub fn getpgid(&self) -> usize {
        self.process.getpgid()
    }
    /// 获取用户空间的token
    pub fn get_user_token(&self) -> usize {
        self.process.vm().lock().token()
    }
}

impl Drop for TaskControlBlock {
    /// 当任务控制块被销毁时，释放用户资源槽位
    fn drop(&mut self) {
        registry::unregister_task(self.tid.0);
        self.process.remove_thread(self.tid.0);
        self.process
            .user_res_slot_allocator()
            .lock()
            .dealloc(self.user_res_slot);
    }
}

#[derive(Copy, Clone, PartialEq, Debug)]
/// 任务状态
pub enum TaskStatus {
    /// 就绪态
    Ready,
    /// 运行态
    Running,
    /// 僵尸态
    Zombie,
    /// 可中断态
    Interruptible,
}
