---
title: "文件、fd 与事件 syscall"
category: syscall
status: stable
author: MangoCore Team
last_update: 2026-08-03
tags: [syscall, fs, fd, epoll, eventfd]
---

# 文件、fd 与事件 syscall

## 1. 概述

文件、fd 与事件类 syscall 的分发表位于 `os/src/syscall/mod.rs`，主要实现位于：

| 文件 | 覆盖范围 |
|------|----------|
| `os/src/syscall/fs.rs` | 路径、open/read/write/stat/mount/fcntl/pipe/splice/pselect/ppoll/xattr |
| `os/src/fs/eventpoll.rs` | epoll fd 和 `epoll_*` syscall |
| `os/src/fs/eventfd.rs` | eventfd fd 和 `eventfd2` syscall |
| `os/src/fs/timerfd.rs` | timerfd fd 和 `timerfd_*` syscall |
| `os/src/syscall/process/signal.rs` | signalfd、pidfd 相关 syscall 分支 |

所有 fd 都落在进程的 fd table 中。syscall 层一般先取得当前任务，再锁 fd table，clone 出 `Arc<File>` 或修改 fd table，随后释放锁并进入实际 I/O 或 VFS 操作。

## 2. 路径和 open

### 2.1 `sys_openat`

`sys_openat(dirfd, path, flags, mode)` 执行顺序：

```
current_task()
token = task.get_user_token()
UserCString(path).read(token)
validate_path_len()
OpenFlags::from_bits(flags)
open_proc_self_fd(path, flags)       [特殊路径]
apply_current_umask(mode)
open_file_at(dirfd, path, flags, create_mode)
fd_table.alloc_fd(file, O_CLOEXEC)
```

关键错误：

| 检查 | 失败 errno |
|------|------------|
| 用户路径指针不可读 | `EFAULT` |
| 路径总长度或单个 component 超限 | `ENAMETOOLONG` |
| flags 不能转成 `OpenFlags` | `EINVAL` |
| fd table 分配失败 | VFS/fd table 返回的 errno |

`SYSCALL_OPEN = 506` 不是独立实现，它包装为：

```rust
sys_openat(AT_FDCWD, path, flags, 0o777)
```

`sys_openat()` 核心源码如下：

```rust
pub fn sys_openat(dirfd: usize, path: *const u8, flags: u32, mode: u32) -> isize {
    let mode_bits = mode;
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path = match user_cstring(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }
    let flags = match OpenFlags::from_bits(flags) {
        Some(flags) => flags,
        None => {
            warn!("[sys_openat] unknown flags");
            return EINVAL;
        }
    };
    if let Some(result) = open_proc_self_fd(&path, flags) {
        let new_file = match result {
            Ok(file) => file,
            Err(errno) => return errno,
        };
        let files_ref = task.process.files();
        let mut fd_table = files_ref.lock();
        return match fd_table.alloc_fd(new_file, flags.contains(OpenFlags::O_CLOEXEC)) {
            Ok(fd) => fd as isize,
            Err(e) => -(e as isize),
        };
    }
    let create_mode = apply_current_umask(vfs::InodeMode::from_bits_truncate(mode_bits));
    let new_file = match open_file_at(dirfd, &path, flags, create_mode) {
        Ok(file) => file,
        Err(errno) => return errno,
    };

    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();
    let new_fd = match fd_table.alloc_fd(new_file, flags.contains(OpenFlags::O_CLOEXEC)) {
        Ok(fd) => fd,
        Err(e) => return -(e as isize),
    };
    new_fd as isize
}
```

该函数读取用户路径后立即校验 path 长度，再解析 flags。`/proc/self/fd/N` 分支会先重新取得目标文件，再分配新的 fd。

### 2.2 `open_file_at`

`open_file_at()` 是路径打开的核心函数：

| 分支 | 行为 |
|------|------|
| `path.is_empty()` | 直接从起点 inode 构造 `File::new_without_open` |
| 非 root 用户 | 先检查父路径 search 权限 |
| lookup 命中 | 检查 `O_NOFOLLOW`、`O_CREAT|O_EXCL`、目录写打开、`O_DIRECTORY`、写权限、`ETXTBSY`、`O_TRUNC` |
| lookup 返回 `ENOENT` | 仅在 `O_CREAT` 且非 `O_DIRECTORY` 时创建 |
| FIFO | 使用 `fs::dev::pipe::fifo_open()` 替换为 pipe-backed inode |

目录写打开语义中，`O_CREAT|O_DIRECTORY` 返回 `EINVAL`，普通写打开目录返回 `EISDIR`。非目录配合 `O_DIRECTORY` 返回 `ENOTDIR`。

### 2.3 `/proc/self/fd/N`

`open_proc_self_fd()` 处理 `/proc/self/fd/<fd>`：

| 步骤 | 行为 |
|------|------|
| 解析 fd 文本 | 为空或非数字返回 `ENOENT` |
| 查 fd table | fd 不存在返回对应 errno |
| 重新构造文件 | 使用原 inode 和新 flags 创建 `File` |
| memfd seals | 将原 memfd seals 复制到 reopened file |
| `O_TRUNC` | 目录返回 `EISDIR`，不可写返回 `EACCES`，seal 冲突返回 `EPERM` |

## 3. read/write

### 3.1 `sys_read`

`sys_read(fd, buf, count)`：

```
count = min(count, MAX_RW_COUNT)
file = fd_table.get_file(fd)
file.readable()
token = task.get_user_token()
if /dev/null -> 0
if /dev/zero -> read_zero_into_user()
if O_NONBLOCK -> read_into_user()
else if inode.read_wait_queue() -> WaitQueue::wait_until_interruptible(...)
else -> read_into_user()
```

阻塞路径中，闭包返回 `EAGAIN` 时继续等待；被信号打断返回 `ERESTART`；timeout 返回 `EAGAIN`。

`sys_read()` 源码主路径如下：

```rust
pub fn sys_read(fd: usize, buf: usize, count: usize) -> isize {
    let count = count.min(crate::hal::MAX_RW_COUNT);
    let task = current_task().unwrap();
    let file = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        match fd_table.get_file(fd) {
            Ok(fd_ref) => Arc::clone(&fd_ref),
            Err(e) => return -(e as isize),
        }
    };
    if file.readable().is_err() {
        return EBADF;
    }
    let token = task.get_user_token();
    if file.is_dev_null() {
        return 0;
    }
    if file.is_dev_zero() {
        return read_zero_into_user(token, buf, count);
    }
    let is_nonblock = file.is_nonblock();
    if is_nonblock {
        read_into_user(&file, token, buf, count)
    } else if let Some(wq) = file.inode.read_wait_queue() {
        match WaitQueue::wait_until_interruptible(wq, || {
            let ret = read_into_user(&file, token, buf, count);
            if ret == -(SyscallErr::EAGAIN as isize) { None } else { Some(ret) }
        }) {
            WaitResult::Ready(n) => n,
            WaitResult::Interrupted => -(SyscallErr::ERESTART as isize),
            WaitResult::TimedOut => -(SyscallErr::EAGAIN as isize),
        }
    } else {
        read_into_user(&file, token, buf, count)
    }
}
```

### 3.2 `read_into_user`

`read_into_user()` 有两条路径：

| 路径 | 条件 | 行为 |
|------|------|------|
| direct user buffer | `inode.supports_user_buffer_io()` | 构造可写前缀后调用 `file.read_user()` |
| kernel bounce buffer | 其他 inode | 按可写前缀读取到内核 buffer，再用同一个 writer 复制 |

读取前通过 `UserBufferWriter::new_writable_prefix()` 在一次 VM 临界区内确定当前连续可写前缀。
若首页尚不可写，它会 fault-in 首页；若后续页不可写，则立即返回已有前缀，而不会为了本次
可能发生的短读提前触发后续页的 lazy allocation、CoW 或 TLB shootdown。

关键顺序仍是“先限定本轮可交付长度，再让文件对象产生数据”。简化后的两条路径为：

```text
(writer, accessible) = new_writable_prefix(user_addr, want)

direct: file.read_user(writer.into_user_buffer())
bounce: n = file.read(kernel_buffer[..accessible])
        copied = writer.write_from(kernel_buffer[..n])
```

前缀扫描只返回 VA 描述符和长度，不保存 PTE、PA 或用户页 slice，也不跨文件 I/O 持有 VM 锁。
真正写入用户页时，`UserBuffer` 仍逐页重新取得 VM 锁并校验当前 PTE，因此并发
`munmap/mprotect/CoW` 不会让构造期翻译变成悬空引用。

direct user buffer 分支用于能直接处理用户 buffer 的 inode；bounce buffer 分支用于 pipe、
socket、devfs、procfs 等 `File::read(&mut [u8])` 对象。两条路径都以实际完成字节数为准；首字节
前失败返回 errno，已有前缀完成后再失败则返回 partial count。

### 3.3 `sys_write`

`sys_write(fd, buf, count)`：

```
count = min(count, MAX_RW_COUNT)
file = fd_table.get_file(fd)
file.writable()
if /dev/null or /dev/zero -> count
apply_fsize_limit(file, count, write_start_offset, fsize_limit)
if O_NONBLOCK -> write_from_user()
else if inode.write_wait_queue() -> WaitQueue::wait_until_interruptible(...)
else -> write_from_user()
```

写路径会检查当前任务的 `fsize_limit_cur`。超出限制时按 `apply_fsize_limit()` 的返回决定可写字节数或 errno。

`sys_write()` 源码主路径如下：

```rust
pub fn sys_write(fd: usize, buf: usize, count: usize) -> isize {
    let mut count = count.min(crate::hal::MAX_RW_COUNT);
    let task = current_task().unwrap();
    let file = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        match fd_table.get_file(fd) {
            Ok(fd_ref) => Arc::clone(&fd_ref),
            Err(e) => return -(e as isize),
        }
    };
    if file.writable().is_err() {
        return EBADF;
    }
    if file.is_dev_null() || file.is_dev_zero() {
        return count as isize;
    }
    let fsize_limit = task.acquire_inner_lock().fsize_limit_cur;
    count = match apply_fsize_limit(&file, count, write_start_offset(&file), fsize_limit) {
        Ok(count) => count,
        Err(errno) => return errno,
    };
    let is_nonblock = file.is_nonblock();
    let token = task.get_user_token();
    if is_nonblock {
        write_from_user(&file, token, buf, count)
    } else if let Some(wq) = file.inode.write_wait_queue() {
        match WaitQueue::wait_until_interruptible(wq, || {
            let ret = write_from_user(&file, token, buf, count);
            if ret == -(SyscallErr::EAGAIN as isize) { None } else { Some(ret) }
        }) {
            WaitResult::Ready(n) => n,
            WaitResult::Interrupted => -(SyscallErr::ERESTART as isize),
            WaitResult::TimedOut => -(SyscallErr::EAGAIN as isize),
        }
    } else {
        write_from_user(&file, token, buf, count)
    }
}
```

### 3.4 向量 I/O

`sys_readv()` 和 `sys_writev()` 使用 `UserIoVec::read_user_iovecs()`：

| 检查 | 行为 |
|------|------|
| iovcnt > 1024 | `EINVAL` |
| iovec 数组读取失败 | uaccess errno |
| 总长度溢出或超过 `isize::MAX` | `EINVAL` |
| 总长度超过 `MAX_RW_COUNT` | 通过 `total_cap` 截断 |

`readv` 也区分 direct user buffer 和 kernel bounce buffer；`writev` 会先应用文件大小限制，再走 discard/direct/bounce 路径。

## 4. pread/pwrite 与偏移

| syscall | 特点 |
|---------|------|
| `pread`/`pwrite` | 显式 offset；offset 以 `usize` 传入但通过 `offset_is_negative()` 拦截超过 `isize::MAX` 的值 |
| `preadv`/`pwritev` | iovec + offset |
| `preadv2`/`pwritev2` | 增加 flags 参数，实际语义由对应实现处理 |
| `lseek` | 使用 `SeekFrom` 修改文件偏移 |

流式 fd 通过 `is_stream_file()` 判断，部分偏移操作会拒绝。

## 5. fd 管理

### 5.1 close

`sys_close(fd)`：

```
fd_table.drop_fd(fd)
record_flock_close()
drop(fd_table)
release_closed_flock_descriptions()
```

先记录需要释放的 flock description，再释放 fd table 锁后做 flock 清理，避免在 fd table 锁内执行更复杂逻辑。

### 5.2 close_range

支持 flags：

| flag | 行为 |
|------|------|
| `CLOSE_RANGE_UNSHARE` | 调用 `process.unshare_files()` 后操作新的 fd table |
| `CLOSE_RANGE_CLOEXEC` | 不关闭 fd，只设置范围内 CLOEXEC |

`first > last` 或 flags 含未知位返回 `EINVAL`。关闭范围时遍历 `[first, last]`，遇到 fd 超过表长度可提前停止。

### 5.3 dup/dup3

| syscall | 行为 |
|---------|------|
| `dup` | 复制到最低可用 fd，CLOEXEC=false |
| `dup2` | 内部辅助；oldfd==newfd 时验证 oldfd 并返回 oldfd |
| `dup3` | oldfd==newfd 返回 `EINVAL`；flags 只能包含 `O_CLOEXEC` |

替换已有 fd 时，代码记录被替换 file 的 flock description 和引用计数，fd table 解锁后再决定是否释放 flock description。

### 5.4 pipe2

`sys_pipe2()` 只接受 `O_CLOEXEC | O_DIRECT | O_NONBLOCK`。创建 read/write 两端后写回用户数组 `[read_fd, write_fd]`；写回失败时关闭已分配 fd 并返回 `EFAULT`。

### 5.5 ioctl FIONBIO

`sys_ioctl(fd, FIONBIO, arg)` 从用户指针读取 `int`，非零时设置该 open file description 的 `O_NONBLOCK`，零时清除。`arg == NULL` 或用户内存不可读返回 `EFAULT`。该状态与 `fcntl(F_SETFL)` 共用 `File::set_nonblock()`，因此 `dup` 出的 fd 会观察到同一状态。

## 6. 目录、stat 与元数据

| syscall | 入口 | 要点 |
|---------|------|------|
| `getdents64` | `sys_getdents64` | 单次 count 截断到 128 KiB；每个 open file description 保留名称快照，`d_off` 使用稳定索引 cookie，已删除名称跳过但不移动后续 cookie |
| `fstatat`/`fstat` | `sys_fstatat`, `sys_fstat` | inode metadata 转 `Stat` |
| `statx` | `sys_statx` | `metadata_to_statx()` 填充 statx 字段 |
| `statfs`/`fstatfs` | `sys_statfs`, `sys_fstatfs` | 文件系统统计 |
| `readlinkat` | `sys_readlinkat` | 读取符号链接/特殊 proc 链接 |
| `utimensat` | `sys_utimensat` | 更新时间戳 |

`metadata_to_stat()` 根据 VFS metadata 填充 dev、ino、mode、nlink、uid/gid、size、block、atime/mtime/ctime 等字段。

## 7. 数据搬运

| syscall | 入口 | 说明 |
|---------|------|------|
| `sendfile` | `sys_sendfile` | 从输入 fd 读并写到输出 fd，可处理 offset 指针 |
| `copy_file_range` | `sys_copy_file_range` | fd 到 fd 的范围复制 |
| `splice` | `sys_splice` | pipe/file 之间搬运 |
| `vmsplice` | `sys_vmsplice` | 用户 iovec 到 pipe |

这些路径同样使用 `MAX_RW_COUNT`、`IO_CHUNK_SIZE`、`UserIoVec` 或用户指针辅助，具体错误优先级在 `fs.rs` 对应函数内实现。

## 8. 挂载与同步

| syscall | 入口 | 说明 |
|---------|------|------|
| `mount` | `sys_mount` | 解析 source/target/fstype/data，进入 VFS/MountFS 挂载 |
| `umount2` | `sys_umount2` | 卸载挂载点 |
| `sync` | `sys_sync` | 全局同步入口 |
| `syncfs` | `sys_syncfs` | fd 所属文件系统同步 |
| `fsync`/`fdatasync` | `sys_fsync`, `sys_fdatasync` | fd 级同步 |
| `truncate`/`ftruncate` | `sys_truncate`, `sys_ftruncate` | 文件长度修改 |
| `fallocate` | `sys_fallocate` | 文件空间预分配/打洞 |
| `fadvise64` | `sys_fadvise64` | 文件访问建议 |

memfd truncate 会检查 seals：`F_SEAL_SHRINK` 阻止缩小，`F_SEAL_GROW` 阻止增长。

## 9. 事件类 fd

### 9.1 epoll

| syscall | 文件 | 说明 |
|---------|------|------|
| `epoll_create1` | `fs/eventpoll.rs` | 创建 epoll fd |
| `epoll_ctl` | `fs/eventpoll.rs` | 添加、修改、删除被观察 fd |
| `epoll_pwait` | `fs/eventpoll.rs` | 等待事件并可临时替换 signal mask |
| `epoll_pwait2` | `fs/eventpoll.rs` | timeout 使用 `TimeSpec` 指针 |

epoll 观察的是 VFS `File::poll()` 语义，因此普通文件、pipe、socket、eventfd、timerfd、signalfd 等都通过 fd 层统一暴露 readiness。

`sys_epoll_create1()`、`sys_epoll_ctl()` 和 `sys_epoll_pwait()` 是 epoll syscall 的三条主入口：

```rust
pub fn sys_epoll_create1(flags: usize) -> isize {
    let cloexec_flag = FileFlags::O_CLOEXEC.bits() as usize;
    if flags & !cloexec_flag != 0 {
        return -(SyscallErr::EINVAL as isize);
    }

    let file_flags = FileFlags::O_RDWR
        | if flags & cloexec_flag != 0 {
            FileFlags::O_CLOEXEC
        } else {
            FileFlags::empty()
        };
    let inode = Arc::new(EventPollFile::new()) as Arc<dyn IndexNode>;
    let file = match File::new(inode, file_flags) {
        Ok(file) => file,
        Err(err) => return -(err as isize),
    };

    let task = current_task().unwrap();
    let files = task.process.files();
    let ret = match files
        .lock()
        .alloc_fd(file, flags & cloexec_flag != 0)
    {
        Ok(fd) => fd as isize,
        Err(err) => -(err as isize),
    };
    ret
}
```

创建阶段只接受 `O_CLOEXEC` 对应的 flag，并把 `EventPollFile` 包装成 VFS `File` 放入 fd table。`sys_epoll_ctl()` 负责校验 epoll fd、目标 fd、用户事件结构和嵌套 epoll：

```rust
pub fn sys_epoll_ctl(epfd: usize, op: usize, fd: usize, event: *const EpollUserEvent) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let files = task.process.files();
    let fd_table = files.lock();

    let epoll_file = match fd_table.get_file(epfd) {
        Ok(file) => file,
        Err(err) => return -(err as isize),
    };
    let epoll = match eventpoll_from_file(&*epoll_file) {
        Some(epoll) => epoll,
        None => return -(SyscallErr::EINVAL as isize),
    };

    if epfd == fd {
        return -(SyscallErr::EINVAL as isize);
    }

    if op != EPOLL_CTL_DEL {
        if event.is_null() {
            return -(SyscallErr::EFAULT as isize);
        }
    }

    let user_event = if op == EPOLL_CTL_DEL {
        None
    } else {
        match UserPtr::new(event).read(token) {
            Ok(event) => Some(event),
            Err(errno) => return errno,
        }
    };

    match op {
        EPOLL_CTL_ADD => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(err) => return -(err as isize),
            };
            let target_epoll = eventpoll_from_file(&*file);
            if let Some(target_epoll) = target_epoll.as_ref() {
                if let Err(err) = epoll.check_nested_epoll(target_epoll) {
                    return -(err as isize);
                }
            } else if !file.mode().contains(FileMode::FMODE_STREAM) {
                return -(SyscallErr::EPERM as isize);
            }
            let event = user_event.unwrap();
            let events = EPollEvent::from_bits_truncate(event.events as usize);
            match epoll.add(fd, file, events, event.data) {
                Ok(()) => SUCCESS,
                Err(err) => -(err as isize),
            }
        }
        EPOLL_CTL_MOD => {
            let event = user_event.unwrap();
            let events = EPollEvent::from_bits_truncate(event.events as usize);
            match epoll.modify(fd, events, event.data) {
                Ok(()) => SUCCESS,
                Err(err) => -(err as isize),
            }
        }
        EPOLL_CTL_DEL => match epoll.delete(fd) {
            Ok(()) => SUCCESS,
            Err(err) => -(err as isize),
        },
        _ => -(SyscallErr::EINVAL as isize),
    }
}
```

等待阶段会临时替换 signal mask，先检查可处理信号，再调用 epoll 对象的 `wait()`，最后把就绪事件数组写回用户空间：

```rust
pub fn sys_epoll_pwait(
    epfd: usize,
    events: *mut EpollUserEvent,
    maxevents: isize,
    timeout: isize,
    sigmask: *const Signals,
) -> isize {
    if maxevents <= 0 {
        return -(SyscallErr::EINVAL as isize);
    }
    if events.is_null() {
        return -(SyscallErr::EFAULT as isize);
    }

    let task = current_task().unwrap();
    let token = task.get_user_token();
    let files = task.process.files();
    let epoll = {
        let fd_table = files.lock();
        let epoll_file = match fd_table.get_file(epfd) {
            Ok(file) => file,
            Err(err) => return -(err as isize),
        };
        match eventpoll_from_file(&*epoll_file) {
            Some(epoll) => epoll,
            None => return -(SyscallErr::EINVAL as isize),
        }
    };
    drop(task);

    let old_mask = match apply_temporary_sigmask(sigmask) {
        Ok(old_mask) => old_mask,
        Err(errno) => return errno,
    };

    if let Some(task) = current_task() {
        if has_actionable_signal(&task) {
            restore_sigmask(old_mask);
            return -(SyscallErr::EINTR as isize);
        }
    }

    let ready = epoll.wait(maxevents as usize, timeout);
    restore_sigmask(old_mask);

    let ready = match ready {
        Ok(events) => events,
        Err(errno) => return errno,
    };

    let mut out = Vec::new();
    if out.try_reserve(ready.len()).is_err() {
        return -(SyscallErr::ENOMEM as isize);
    }
    for event in ready {
        out.push(EpollUserEvent {
            events: event.events.bits() as u32,
            data: event.data,
        });
    }

    if UserSlice::new(events as *const EpollUserEvent, out.len())
        .write_array_from(token, &out)
        .is_err()
    {
        return -(SyscallErr::EFAULT as isize);
    }

    out.len() as isize
}
```

`sys_epoll_pwait2()` 只是在入口处把用户 `TimeSpec` 转换成毫秒 timeout，再复用 `sys_epoll_pwait()`。

### 9.2 eventfd

`sys_eventfd2(initval, flags)` 创建计数 fd。事件读写由 `fs/eventfd.rs` 的 `File` 实现维护，epoll 可观察其可读/可写状态。

eventfd 的核心状态是一个 `u64` counter，读写逻辑在 inode 的 `read_at()`/`write_at()` 中实现：

```rust
impl IndexNode for EventFd {
    fn read_at(
        &self,
        _offset: usize,
        len: usize,
        buf: &mut [u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        if len < core::mem::size_of::<u64>() || buf.len() < core::mem::size_of::<u64>() {
            return Err(SyscallErr::EINVAL);
        }

        let value = {
            let mut inner = self.inner.lock();
            if inner.counter == 0 {
                return Err(SyscallErr::EAGAIN);
            }

            if self.semaphore {
                inner.counter -= 1;
                1
            } else {
                let value = inner.counter;
                inner.counter = 0;
                value
            }
        };

        buf[..8].copy_from_slice(&value.to_ne_bytes());
        self.notify_writable();
        Ok(8)
    }

    fn write_at(
        &self,
        _offset: usize,
        len: usize,
        buf: &[u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        if len < core::mem::size_of::<u64>() || buf.len() < core::mem::size_of::<u64>() {
            return Err(SyscallErr::EINVAL);
        }

        let value = u64::from_ne_bytes(buf[..8].try_into().unwrap());
        if value == u64::MAX {
            return Err(SyscallErr::EINVAL);
        }

        {
            let mut inner = self.inner.lock();
            if EVENTFD_COUNTER_MAX.saturating_sub(inner.counter) < value {
                return Err(SyscallErr::EAGAIN);
            }
            inner.counter += value;
        }

        self.notify_readable();
        Ok(8)
    }
}
```

`EFD_SEMAPHORE` 使每次 read 只取出 1；普通模式一次读出当前 counter 并清零。写入 `u64::MAX` 返回 `EINVAL`，写入会导致 counter 超过 `u64::MAX - 1` 时返回 `EAGAIN`。

```rust
pub fn sys_eventfd2(initval: u32, flags: u32) -> isize {
    if (flags & !EFD_VALID_FLAGS) != 0 {
        return -(SyscallErr::EINVAL as isize);
    }

    let mut file_flags = FileFlags::O_RDWR;
    if (flags & EFD_NONBLOCK) != 0 {
        file_flags |= FileFlags::O_NONBLOCK;
    }
    if (flags & EFD_CLOEXEC) != 0 {
        file_flags |= FileFlags::O_CLOEXEC;
    }

    let inode = Arc::new(EventFd::new(initval, flags)) as Arc<dyn IndexNode>;
    let file = match File::new(inode, file_flags) {
        Ok(file) => file,
        Err(err) => return -(err as isize),
    };

    let task = current_task().unwrap();
    let files = task.process.files();
    let ret = match files.lock().alloc_fd(file, (flags & EFD_CLOEXEC) != 0) {
        Ok(fd) => fd as isize,
        Err(err) => -(err as isize),
    };
    ret
}
```

### 9.3 timerfd

| syscall | 行为 |
|---------|------|
| `timerfd_create(clock_id, flags)` | 创建 timerfd |
| `timerfd_settime(fd, flags, new, old)` | 设置定时器，可写回旧值 |
| `timerfd_gettime(fd, curr)` | 读取当前 timerfd 配置 |

timerfd 依赖 task timer 设施。

### 9.4 signalfd、pidfd、memfd

| fd 类型 | syscall | 文件 |
|---------|---------|------|
| signalfd | `signalfd4` | `syscall/process/signal.rs` |
| pidfd | `pidfd_open`, `pidfd_getfd`, `pidfd_send_signal` | `fs/pidfd.rs`, `signal.rs` |
| memfd | `memfd_create` | `syscall/fs.rs` 中 `sys_memfd_create()`，文件位于 `/dev/shm/.memfd-*` |

`sys_memfd_create()` 检查 flags、读取用户 name、限制 name 长度为 249，并尝试在 `/dev/shm/.memfd-{tid}-{id}` 创建匿名文件对象。

## 10. 扩展属性

| 类别 | syscall |
|------|---------|
| set | `setxattr`, `lsetxattr`, `fsetxattr` |
| get | `getxattr`, `lgetxattr`, `fgetxattr` |
| list | `listxattr`, `llistxattr`, `flistxattr` |
| remove | `removexattr`, `lremovexattr`, `fremovexattr` |

这些分支均注册在 `syscall/mod.rs`，实现位于 `syscall/fs.rs`。

## 11. 错误码边界

| 场景 | errno |
|------|-------|
| fd 不存在 | fd table 返回值转负 errno，通常 `EBADF` |
| fd 不可读/不可写 | `EBADF` |
| 用户 buffer 不可访问 | `EFAULT` |
| `dup3(oldfd == newfd)` | `EINVAL` |
| `pipe2` flags 非法 | `EINVAL` |
| `close_range(first > last)` | `EINVAL` |
| open 路径过长 | `ENAMETOOLONG` |
| `O_NOFOLLOW` 命中符号链接且不是 `O_PATH` | `ELOOP` |
| 写打开正在执行的 inode | `ETXTBSY` |
| 写打开目录 | `EISDIR` 或 `O_CREAT|O_DIRECTORY` 的 `EINVAL` |
| 非目录配合 `O_DIRECTORY` | `ENOTDIR` |
| memfd seal 阻止 truncate | `EPERM` |

## 12. 测试映射

| 功能 | 代表测试 |
|------|----------|
| open/path/stat | LTP `open*`, `openat*`, `stat*`, `statx*` |
| read/write | LTP `read*`, `write*`, `pread*`, `pwrite*`, `readv*`, `writev*` |
| fd 管理 | LTP `dup*`, `fcntl*`, `close_range*`, `pipe*` |
| event | LTP `epoll*`, `eventfd*`, `timerfd*`, `signalfd*` |
| mount/sync | mount/fsync/syncfs 相关测试 |
| 数据搬运 | `sendfile*`, `copy_file_range*`, `splice*`, `vmsplice*` |
| memfd | `memfd_create*`, mmap seal 相关用例 |

## 13. 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/syscall/fs.rs` | 文件、fd、挂载、stat、xattr、pselect/ppoll 等 syscall |
| `os/src/fs/eventpoll.rs` | epoll fd |
| `os/src/fs/eventfd.rs` | eventfd fd |
| `os/src/fs/timerfd.rs` | timerfd fd |
| `os/src/fs/pidfd.rs` | pidfd 文件对象 |
| `os/src/fs/vfs/` | VFS File/IndexNode/FileSystem/MountFS |
| `os/src/mm/uaccess.rs` | 用户 buffer、iovec、字符串访问 |
| `os/src/task/manager.rs` | WaitQueue |
