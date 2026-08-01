---
title: "SysV IPC、POSIX MQ 与 IPC Namespace"
category: process
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [process, ipc, sysv, mq]
---

# SysV IPC、POSIX MQ 与 IPC Namespace

## 1. 源码位置

IPC syscall 位于 `os/src/syscall/process/ipc.rs`。namespace 对象位于 `os/src/task/ipc_namespace.rs`，PCB 通过 `ipc: Arc<IpcNamespace>` 持有当前 IPC namespace。

本文件实现三类接口：

| 类别 | syscall |
|------|---------|
| SysV shared memory | `shmget`, `shmat`, `shmdt`, `shmctl` |
| SysV semaphore | `semget`, `semctl`, `semop`, `semtimedop` |
| SysV message queue | `msgget`, `msgsnd`, `msgrcv`, `msgctl` |
| POSIX message queue | `mq_open`, `mq_unlink`, `mq_timedsend`, `mq_timedreceive`, `mq_getsetattr`, `mq_notify` |

IPC namespace 对象自身只保存 namespace id；SysV/POSIX IPC registry 使用该 id 或当前进程的 namespace 引用区分隔离域：

```rust
pub struct IpcNamespace {
    pub id: u64,
}

lazy_static! {
    pub static ref INIT_IPC_NAMESPACE: Arc<IpcNamespace> = Arc::new(IpcNamespace { id: 0 });
}

static NEXT_IPC_NS_ID: AtomicU64 = AtomicU64::new(1);

impl IpcNamespace {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            id: NEXT_IPC_NS_ID.fetch_add(1, Ordering::Relaxed),
        })
    }
}
```

## 2. 通用 IPC 常量

| 常量 | 值/语义 |
|------|---------|
| `IPC_PRIVATE` | key 0，创建私有对象 |
| `IPC_CREAT` | 不存在则创建 |
| `IPC_EXCL` | 配合 CREATE，已存在时报错 |
| `IPC_NOWAIT` | 不阻塞 |
| `IPC_RMID` | 删除对象 |
| `IPC_SET` | 设置属性 |
| `IPC_STAT` | 获取属性 |
| `IPC_INFO` | 获取系统限制 |
| `IPC_64` | Linux ABI 兼容 bit，内部用 `normalize_ipc_cmd()` 去掉 |

权限结构 `LinuxIpcPerm` 根据架构处理 mode/seq 字段宽度：rv64 使用 `u16`，其他架构使用 `u32`。

## 3. SysV shared memory

限制：

| 项 | 值 |
|----|----|
| `SHMMNI` | 4096 |
| `MAX_SHM_SIZE` | 16 MiB |
| `SHMLBA` | `PAGE_SIZE` |
| rv64 `ARCH_SHMLBA` | `PAGE_SIZE` |
| 非 rv64 `ARCH_SHMLBA` | `0x10000` |

shm VMA 通过 MM 的 `shm_mmap()` 映射一组预分配 `FrameTracker`。`SHM_RDONLY` 决定映射权限，`SHM_REMAP` 允许覆盖，`SHM_RND` 按 SHMLBA 对齐。

进程退出时 `task/mod.rs::finish_current_exit()` 只在当前线程消费最后一个 live token 后调用
`shm_detach_process(pid)`，释放该 pid 的 attachment。

shared memory 段和 attachment 的核心结构如下：

```rust
#[derive(Clone, Copy)]
struct ShmAttachment {
    pid: usize,
    addr: usize,
}

struct ShmSegment {
    ns_id: u64,
    key: isize,
    size: usize,
    uid: u32,
    gid: u32,
    cuid: u32,
    cgid: u32,
    mode: usize,
    cpid: i32,
    lpid: i32,
    atime: usize,
    dtime: usize,
    ctime: usize,
    removed: bool,
    locked: bool,
    frames: Vec<Arc<FrameTracker>>,
    attachments: Vec<ShmAttachment>,
}
```

`sys_shmget()` 负责对象查找和创建：

```rust
pub fn sys_shmget(key: isize, size: usize, shmflg: usize) -> isize {
    let ns_id = current_ipc_ns_id();
    let mut registry = SHM_REGISTRY.lock();
    if key != IPC_PRIVATE {
        if let Some((id, seg)) = registry.find_by_key(ns_id, key) {
            if shmflg & IPC_CREAT != 0 && shmflg & IPC_EXCL != 0 {
                return EEXIST;
            }
            if size > seg.size {
                return EINVAL;
            }
            if !has_shm_permission(seg, shmflg & (SHM_R | SHM_W)) {
                return EACCES;
            }
            return id as isize;
        }
        if shmflg & IPC_CREAT == 0 {
            return ENOENT;
        }
    }

    if size == 0 || size > MAX_SHM_SIZE {
        return EINVAL;
    }
    if registry.ns_segment_count(ns_id) >= SHMMNI {
        return ENOSPC;
    }
    let (uid, gid) = current_ipc_ids();
    let id = registry.alloc_id();
    registry.segments.insert(
        id,
        ShmSegment::new(ns_id, key, size, shmflg & 0o777, uid, gid),
    );
    id as isize
}
```

`sys_shmat()` 在第一次 attach 时为段分配物理页，然后调用 MM 的 `shm_mmap()` 把这些 frame 映射进当前地址空间：

```rust
let (size, removed, frames) = {
    let mut registry = SHM_REGISTRY.lock();
    let Some(seg) = registry.segments.get_mut(&shmid) else {
        return EINVAL;
    };
    if seg.ns_id != ns_id {
        return EINVAL;
    }
    let requested = if shmflg & SHM_RDONLY != 0 {
        SHM_R
    } else {
        SHM_R | SHM_W
    };
    if !has_shm_permission(seg, requested) {
        return EACCES;
    }
    if seg.frames.is_empty() {
        let page_count = (seg.size + crate::config::PAGE_SIZE - 1) / crate::config::PAGE_SIZE;
        let Some(frames) = frames_alloc(page_count) else {
            return ENOMEM;
        };
        seg.frames = frames;
    }
    let mut frames = Vec::new();
    if frames.try_reserve(seg.frames.len()).is_err() {
        return ENOMEM;
    }
    frames.extend(seg.frames.iter().cloned());
    (seg.size, seg.removed, frames)
};

let mapped = task
    .process
    .vm()
    .lock()
    .shm_mmap(attach_addr, size, prot, flags, &frames, shmflg & SHM_RDONLY == 0);
```

这说明 shm 对象的 frame 保存在 IPC registry 中，VMA 只引用这些 frame；进程退出时必须通过 attachment 记录反向清理。

## 4. SysV message queue

限制：

| 项 | 值 |
|----|----|
| `MSGMNI` | 1024 |
| `MSGMAX` | 8192 |
| `MSGMNB` | 16384 |
| `MSGTQL` | 4096 |

`MsgQueue` 字段：

| 字段 | 说明 |
|------|------|
| `key` | IPC key |
| `uid/gid/cuid/cgid/mode` | 权限 |
| `qbytes` | 队列最大字节数 |
| `messages` | `VecDeque<Message>` |
| `cbytes` | 当前字节数 |
| `lspid/lrpid` | 最近 send/receive pid |
| `stime/rtime/ctime` | 时间戳 |

`MsgRegistry` 维护 `next_id`、queues、wait_queue 和 removed_ids。

message queue 的队列与 registry 定义为：

```rust
struct MsgQueue {
    key: isize,
    uid: u32,
    gid: u32,
    cuid: u32,
    cgid: u32,
    mode: usize,
    qbytes: usize,
    messages: VecDeque<Message>,
    cbytes: usize,
    lspid: i32,
    lrpid: i32,
    stime: usize,
    rtime: usize,
    ctime: usize,
}

struct MsgRegistry {
    next_id: i32,
    queues: BTreeMap<i32, MsgQueue>,
    wait_queue: WaitQueue,
    removed_ids: Vec<i32>,
}
```

`msgsnd` 在队列满时返回 `None`，由 WaitQueue 模板决定阻塞或 `EAGAIN`：

```rust
fn try_msgsnd_locked(
    registry: &mut MsgRegistry,
    msqid: i32,
    mtype: isize,
    data: &[u8],
) -> Option<isize> {
    let mut wake_waiters = false;
    let result = {
        let Some(queue) = registry.queues.get_mut(&msqid) else {
            return Some(if registry.was_removed(msqid) { EIDRM } else { EINVAL });
        };
        if !has_msg_permission(queue, MSG_W) {
            return Some(EACCES);
        }
        if queue.cbytes.saturating_add(data.len()) > queue.qbytes
            || queue.messages.len() >= MSGTQL
        {
            return None;
        }

        if queue.messages.try_reserve(1).is_err() {
            return Some(ENOMEM);
        }
        let mut payload = Vec::new();
        if payload.try_reserve_exact(data.len()).is_err() {
            return Some(ENOMEM);
        }
        payload.extend_from_slice(data);
        queue.messages.push_back(Message { mtype, data: payload });
        queue.cbytes = queue.cbytes.saturating_add(data.len());
        queue.lspid = current_pid_i32();
        queue.stime = now_sec();
        wake_waiters = true;
        Some(SUCCESS)
    };
    if wake_waiters {
        registry.wait_queue.wake_all();
    }
    result
}
```

非阻塞和阻塞发送只差在 `None` 的处理方式：

```rust
if msgflg & IPC_NOWAIT != 0 {
    let mut registry = MSG_REGISTRY.lock();
    return try_msgsnd_locked(&mut registry, msqid, mtype, &data).unwrap_or(EAGAIN);
}

match WaitQueue::wait_event_interruptible_locked(
    &MSG_REGISTRY,
    |registry| &mut registry.wait_queue,
    |registry| try_msgsnd_locked(registry, msqid, mtype, &data),
) {
    WaitResult::Ready(value) => value,
    WaitResult::Interrupted => EINTR,
    WaitResult::TimedOut => EINTR,
}
```

## 5. 消息选择语义

`msgrcv` 的 `msgtyp` 和 flags 支持 Linux 常见语义：

| 条件 | 选择 |
|------|------|
| `msgtyp == 0` | 队首消息 |
| `msgtyp > 0` | 类型等于 msgtyp，`MSG_EXCEPT` 时取不等于 |
| `msgtyp < 0` | 类型小于等于 abs(msgtyp) 的最小类型 |
| `MSG_COPY` | 按序号复制，不移除 |
| `MSG_NOERROR` | 用户 buffer 小时截断 |

队列为空且无 `IPC_NOWAIT` 时，通过 WaitQueue 阻塞等待。

普通接收的线性化点是 `MSG_REGISTRY` 锁内的 `VecDeque::remove(idx)`：消息、`cbytes`、
`lrpid/rtime` 和 sender wake 在同一临界区完成，因此两个 CPU 不可能同时领取同一条消息。
`MSG_COPY` 例外地只复制内核快照，不改变队列。两种分支都在解锁后写用户 buffer：

```rust
pub fn sys_msgrcv(
    msqid: i32,
    msgp: usize,
    msgsz: usize,
    msgtyp: isize,
    msgflg: usize,
) -> isize {
    let allowed_flags = IPC_NOWAIT | MSG_NOERROR | MSG_EXCEPT | MSG_COPY;
    if msgsz > sysv_msgmax() || msgflg & !allowed_flags != 0 {
        return EINVAL;
    }
    if msgflg & MSG_COPY != 0
        && (msgflg & IPC_NOWAIT == 0 || msgflg & MSG_EXCEPT != 0 || msgtyp < 0)
    {
        return EINVAL;
    }

    loop {
        match receive_message(msqid, msgsz, msgtyp, msgflg) {
            Ok((mtype, data, copy_len)) => {
                if let Err(errno) = write_msg_to_user(msgp, mtype, &data, copy_len) {
                    return errno;
                }
                return copy_len as isize;
            }
            Err(errno) if errno == EAGAIN => {
                if msgflg & IPC_NOWAIT != 0 {
                    return ENOMSG;
                }
                let wait_result = wait_for_msg_recv(msqid, msgtyp, msgflg);
                if wait_result < 0 {
                    return wait_result;
                }
            }
            Err(errno) => return errno,
        }
    }
}
```

这个顺序避免持 `MSG_REGISTRY` 锁写用户 buffer。普通接收一旦在锁内摘取即已消费；后续
用户 copy 即使返回 `EFAULT` 也不回滚消息，与 Linux `msgrcv` 的所有权交接一致。

## 6. SysV semaphore

限制：

| 项 | 值 |
|----|----|
| `SEMMNI` | 1024 |
| `SEMMSL` | 32000 |
| `SEMOPM` | 500 |
| `SEMVMX` | 32767 |
| `SEMAEM` | 32767 |

支持命令包括：

| semctl cmd | 语义 |
|------------|------|
| `GETPID` | 最近操作 pid |
| `GETVAL` | 单个 semaphore 值 |
| `GETALL` | 全部值 |
| `GETNCNT/GETZCNT` | 等待计数 |
| `SETVAL/SETALL` | 设置值 |
| `IPC_STAT/IPC_SET/IPC_RMID` | 元数据/删除 |
| `SEM_STAT/SEM_INFO/SEM_STAT_ANY` | Linux 兼容查询 |

`semop/semtimedop` 对 `sem_op` 的正负零分别执行加、等待减、等待零。`IPC_NOWAIT` 不满足时返回 `EAGAIN`。

semaphore set 与 registry 结构如下：

```rust
struct SemSet {
    key: isize,
    uid: u32,
    gid: u32,
    cuid: u32,
    cgid: u32,
    mode: usize,
    semaphores: Vec<Semaphore>,
    otime: usize,
    ctime: usize,
}

struct SemRegistry {
    next_id: i32,
    sets: BTreeMap<i32, SemSet>,
    wait_queue: WaitQueue,
    removed_ids: Vec<i32>,
}
```

`sys_semtimedop()` 先尝试一次原子应用；不满足时进入带锁等待模板：

```rust
pub fn sys_semtimedop(semid: i32, sops: usize, nsops: usize, timeout: usize) -> isize {
    let ops = match read_sem_ops(sops, nsops) {
        Ok(ops) => ops,
        Err(errno) => return errno,
    };

    {
        let mut registry = SEM_REGISTRY.lock();
        let mut wake_waiters = false;
        let result = {
            let Some(set) = registry.sets.get_mut(&semid) else {
                return EINVAL;
            };
            match try_apply_sem_ops(set, &ops) {
                Ok(SemApplyResult::Applied) => {
                    wake_waiters = true;
                    Some(SUCCESS)
                }
                Ok(SemApplyResult::Blocked { .. }) => None,
                Err(errno) => Some(errno),
            }
        };
        if wake_waiters {
            registry.wait_queue.wake_all();
        }
        if let Some(errno) = result {
            return errno;
        }
    }

    let deadline = match sem_block_deadline(timeout) {
        Ok(deadline) => deadline,
        Err(errno) => return errno,
    };
    let mut registered = None;
    let wait_result = if let Some(deadline) = deadline {
        WaitQueue::wait_event_interruptible_timeout_locked(
            &SEM_REGISTRY,
            |registry| &mut registry.wait_queue,
            |registry| sem_wait_condition(registry, semid, &ops, &mut registered),
            deadline,
        )
    } else {
        WaitQueue::wait_event_interruptible_locked(
            &SEM_REGISTRY,
            |registry| &mut registry.wait_queue,
            |registry| sem_wait_condition(registry, semid, &ops, &mut registered),
        )
    };

    let mut registry = SEM_REGISTRY.lock();
    if let Some(set) = registry.sets.get_mut(&semid) {
        cleanup_sem_wait(set, &mut registered);
    }
    match wait_result {
        WaitResult::Ready(value) => value,
        WaitResult::Interrupted => EINTR,
        WaitResult::TimedOut => EAGAIN,
    }
}
```

## 7. POSIX MQ

限制：

| 项 | 默认/上限 |
|----|-----------|
| queues max | 默认 256，硬上限 4096 |
| default maxmsg | 10 |
| default msgsize | 8192 |
| max maxmsg | 1024 |
| max msgsize | 65536 |

`mq_open` 支持 flags：

| flag | 行为 |
|------|------|
| `O_CREAT` | 创建队列 |
| `O_EXCL` | 已存在时报错 |
| `O_NONBLOCK` | 非阻塞收发 |
| `O_CLOEXEC` | fd close-on-exec |
| access mode | read/write/readwrite |

POSIX MQ 在 VFS 中表现为 `File`/`IndexNode`，可被 epoll poll，内部使用 EventWaitQueue/WaitQueue 唤醒读写者。

`mq_open` 与 SysV IPC 不同，它以名称找到队列并返回 fd：

```rust
pub fn sys_mq_open(name: *const u8, oflag: u32, _mode: u32, attr: usize) -> isize {
    let name = match mq_name_from_user(name) {
        Ok(name) => name,
        Err(errno) => return errno,
    };
    let file_flags = match mq_file_flags(oflag) {
        Ok(flags) => flags,
        Err(errno) => return errno,
    };

    let mut created = false;
    let queues_max = posix_mq_queues_max();
    let queue = {
        let mut registry = MQ_REGISTRY.lock();
        if let Some(queue) = registry.queues.get(&name) {
            if (oflag & (MQ_O_CREAT | MQ_O_EXCL)) == (MQ_O_CREAT | MQ_O_EXCL) {
                return EEXIST;
            }
            if !has_mq_permission(&queue.inner.lock(), mq_requested_access(oflag)) {
                return EACCES;
            }
            queue.clone()
        } else {
            if (oflag & MQ_O_CREAT) == 0 {
                return ENOENT;
            }
            if registry.queues.len() >= queues_max {
                return ENOSPC;
            }
            let attr = match mq_attr_from_user(attr) {
                Ok(attr) => attr,
                Err(errno) => return errno,
            };
            let (uid, gid) = current_ipc_ids();
            let queue = Arc::new(MqQueue::new(attr, _mode, uid, gid));
            registry.queues.insert(name.clone(), queue.clone());
            created = true;
            queue
        }
    };

    let inode = Arc::new(MqDescriptor {
        queue: queue.clone(),
    }) as Arc<dyn IndexNode>;
    let file = match File::new(inode, file_flags) {
        Ok(file) => file,
        Err(err) => {
            if created {
                MQ_REGISTRY.lock().queues.remove(&name);
            }
            return -(err as isize);
        }
    };

    let task = current_task().unwrap();
    match task
        .process
        .files()
        .lock()
        .alloc_fd(file, (oflag & MQ_O_CLOEXEC) != 0)
    {
        Ok(fd) => fd as isize,
        Err(err) => {
            if created {
                MQ_REGISTRY.lock().queues.remove(&name);
            }
            -(err as isize)
        }
    }
}
```

`mq_timedsend` 在队列从空变非空时取出 notification，并在释放队列锁后投递：

```rust
let notification = loop {
    let mut inner = queue.inner.lock();
    if inner.messages.len() >= inner.attr.mq_maxmsg as usize {
        if file.is_nonblock() {
            return EAGAIN;
        }
        drop(inner);
        let errno = mq_wait_send_ready(&queue, abs_timeout);
        if errno != SUCCESS {
            return errno;
        }
        continue;
    }
    let pos = inner
        .messages
        .iter()
        .position(|message| message.prio < msg_prio)
        .unwrap_or(inner.messages.len());
    let was_empty = inner.messages.is_empty();
    inner.messages.insert(pos, MqMessage { prio: msg_prio, data });
    break if was_empty {
        inner.notification.take()
    } else {
        None
    };
};

queue.notify_readable();
if let Some(notification) = notification {
    mq_deliver_notification(notification);
}
```

## 8. mq_notify

支持 `SIGEV_SIGNAL`、`SIGEV_NONE` 和有限的 `SIGEV_THREAD` 兼容路径。常量：

| 常量 | 说明 |
|------|------|
| `MQ_NOTIFY_COOKIE_LEN` | 32 |
| `MQ_NOTIFY_WOKENUP` | notify wake cookie |
| `MQ_NOTIFY_REMOVED` | notify removed cookie |

消息到达时可向注册进程投递信号，siginfo 通过 signal 模块发送。

## 9. IPC namespace

PCB 中的 `ipc: Arc<IpcNamespace>` 决定当前进程使用的 IPC namespace。

| 操作 | 行为 |
|------|------|
| clone 无 `CLONE_NEWIPC` | 共享 parent ipc namespace |
| clone 有 `CLONE_NEWIPC` | 创建新 `IpcNamespace` |
| unshare `CLONE_NEWIPC` | euid 0 且单线程，替换当前 ipc namespace |
| setns ipc | 通过 `ProcNsIpcInode` 替换 |

SysV IPC 注册表通过当前 `IpcNamespace` 访问；mount/net namespace 的切换由各自 namespace 对象处理。

## 10. 用户内存访问

IPC 层大量使用：

| 接口 | 用途 |
|------|------|
| `copy_from_user` | 读取用户结构体 |
| `copy_from_user_array` | 读取数组，如 sembuf |
| `copy_to_user` | 写回 ds/info |
| `translated_str` | POSIX MQ 名称 |
| `frames_alloc` | shm frame 分配 |

用户 buffer 错误返回 `EFAULT`；内核 `try_reserve` 失败返回 `ENOMEM`。

IPC syscall 的状态通常跨进程存在，不能只看当前 task。SysV msg/sem/shm 对象保存在当前 `IpcNamespace` 的 registry 中，fd table 不参与对象查找；POSIX MQ 以名称打开并返回 fd，后续读写通过文件对象进入队列。shared memory 还会把 IPC 对象和 MM 连接起来：`shmat` 创建 VMA 并引用预分配 frame，进程退出时 `shm_detach_process(pid)` 清理 attachment。

调试 IPC 阻塞时要看 flags 和 WaitQueue。`IPC_NOWAIT` 应直接返回对应 errno；没有 NOWAIT 的 send/receive/semop 可能睡在 registry 的等待队列上，队列删除、消息到达或 semaphore 值变化都应唤醒等待者。

## 11. 调试核对点

| 现象 | 检查 |
|------|------|
| shmat 地址对齐错误 | `SHM_RND`、`ARCH_SHMLBA` |
| 进程退出后 shm 仍 attach | 最后线程退出是否调用 `shm_detach_process` |
| msgrcv 类型选择错误 | `msgtyp` 正负零、`MSG_EXCEPT/MSG_COPY` |
| semop 阻塞不醒 | WaitQueue 和 SEM value 更新 wake |
| mq_notify 不触发 | 注册状态、SIGEV 类型、目标进程信号权限 |
