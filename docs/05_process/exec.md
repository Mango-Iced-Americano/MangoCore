---
title: "execve 与 execveat"
category: process
status: stable
author: MangoCore Team
last_update: 2026-08-10
tags: [process, exec, elf, shebang]
---

# execve 与 execveat

## 1. 源码位置

exec syscall 参数解析在 `os/src/syscall/process/exec.rs`，地址空间替换在 `os/src/task/task.rs::TaskControlBlock::load_elf()`。

```
sys_execve()
  ├── 读取 pathname / argv / envp
  ├── open_exec()
  └── exec_opened_file()

sys_execveat()
  ├── 读取 dirfd/pathname / argv / envp / flags
  ├── open_exec_with_follow() 或 reopen_exec_fd()
  └── exec_opened_file()
        ├── ELF magic 或 shebang
        ├── validate_exec_stack_usage()
        ├── task.load_elf()
        └── mark_execed / set_exec_comm / set_exec_identity / complete_vfork
```

## 2. 路径与文件校验

执行路径校验：

| 条件 | 错误 |
|------|------|
| path 长度 >= `vfs::MAX_PATHLEN` | `ENAMETOOLONG` |
| 任一路径组件长度 > `vfs::NAME_MAX` | `ENAMETOOLONG` |
| `AT_SYMLINK_NOFOLLOW` 且最终 inode 是 symlink | `ELOOP` |
| `execveat` 空 path 但无 `AT_EMPTY_PATH` | `ENOENT` |
| `execveat` 空 path 且 dirfd 为 `AT_FDCWD` | `ENOENT` |

执行文件 metadata 校验：

| 条件 | 错误 |
|------|------|
| inode 不是普通文件且是目录 | `EISDIR` |
| inode 不是普通文件且不是目录 | `EACCES` |
| 根据 fsuid/fsgid/groups 无执行位 | `EACCES` |
| inode 当前被可写打开 | `ETXTBSY` |

root fsuid 仍要求文件至少有任意 execute bit。

## 3. argv/envp 读取

`read_exec_vectors(token, argv, envp)`：

1. 预留 argv/envp Vec 容量。
2. 逐个读取用户指针。
3. 用 `UserCString` 读取字符串。
4. 统计字符串字节数，包含 NUL。
5. argv 为空时补一个空字符串。
6. 总字符串字节数不能超过 `USER_STACK_INIT_SIZE / 2`。

`try_reserve()` 失败返回 `ENOMEM`；大小溢出或超过上限返回 `E2BIG`。

## 4. 栈占用校验

`validate_exec_stack_usage()` 计算最终初始栈占用：

| 内容 | 计入 |
|------|------|
| argv/envp 字符串和 NUL | 是 |
| 对齐 | 是 |
| `AT_RANDOM` 16 字节 | 是 |
| padding | 是 |
| auxv，固定 17 项 | 是 |
| argv/envp 指针数组和 NULL | 是 |
| argc | 是 |

总量必须不超过 `USER_STACK_INIT_SIZE - PAGE_SIZE`，否则 `E2BIG`。

## 5. ELF 与 shebang

`exec_opened_file()` 读取前 4 字节：

| magic | 行为 |
|-------|------|
| `\x7fELF` | 直接执行 |
| `#!` | 解析 shebang |
| 其他 | `ENOEXEC` |

`parse_shebang()` 只读取前 128 字节，取第一行 `#!` 后内容，允许一个可选参数。

shebang 成功时构造 argv：

```text
argv[0] = interpreter
argv[1] = shebang_arg   若存在
argv[next] = script_path
argv[next..] = 原 argv[1..]
```

如果解释器打开失败或不是有效 ELF，会尝试 shell fallback。

## 6. shell fallback

fallback 顺序：

1. `/bin/sh`
2. `/bin/bash`

成功条件是 `open_exec()` 成功且文件 magic 是 ELF。成功后把脚本路径插入 argv[0]，并返回 shell 文件。

`open_exec_with_follow()` 还有兼容分支：当请求 `/bin/sh` 或 `/bin/bash` 失败时，会尝试 `/bash`。

## 7. execveat

支持 flags：

| flag | 行为 |
|------|------|
| `AT_SYMLINK_NOFOLLOW` | 最终路径不跟随 symlink |
| `AT_EMPTY_PATH` | path 为空时执行 dirfd 指向文件 |

其他 flag 返回 `EINVAL`。

非空 path 的起始 inode：

| 条件 | 起点 |
|------|------|
| path 是绝对路径 | 当前工作目录 inode 作为 lookup 根 |
| dirfd 是 `AT_FDCWD` | 当前工作目录 inode |
| dirfd 指向目录 | 该目录 inode |
| dirfd 非目录 | `ENOTDIR` |

空 path 使用 `clone_fd_file(dirfd)` 和 `reopen_exec_fd()` 重新以只读方式打开执行文件。

## 8. load_elf 主流程

`TaskControlBlock::load_elf(elf, argv_vec, envp_vec)`：

1. 目录直接 `EISDIR`。
2. 保留旧 `AddressSpace`，但不提前回收其用户页。
3. 优先由 `AddressSpaceInner::from_elf_inode()` 解析 inode，只保留 PT_LOAD VMA 和不可变后备配方，不预读、分配或复制所有目标页。
4. 准备新 heap、用户栈、argv/envp/auxv、user resource 和 trap context。
5. 调用 `install_exec_image()` 建立临时 exec 会话，关闭同 PCB 的 clone 发布门。
6. 请求 sibling 在各自 owner CPU 的任务安全点退出，并等待 live-thread 计数收缩为 1。
7. 若永久 group exit 已覆盖本次 exec，则放弃尚未提交的新映像，转入统一退出路径。
8. 调用 `reset_exec_resources()`：必要时复制跨 PCB 共享的 fd table/sighand，
   再关闭 CLOEXEC、重置信号动作，并为新映像换上新的 private futex table。
9. 更新当前 TCB trap context、清 `clear_child_tid`、重置 robust list、禁用 alt signal stack。
10. 必要时从外部共享的旧 VM 撤销当前线程的 user-resource 映射。
11. 替换 exe 和 VM，结束临时会话并重新开放线程发布门。
12. 若调用者原本不是 leader，接管进程 PID/TGID 并同步 task registry 与本 CPU current TID。

这个顺序把“可失败的构造”和“不可回滚的提交”分开。ELF 解析、新地址空间和初始用户栈
全部在旧映像仍可运行时完成；只有构造成功后才关闭线程发布门并停止 sibling。旧 VM 不能
在门禁建立前按 `Arc::strong_count()` 提前回收，因为同一 PCB 的线程不会为 VM 各自持有
长期 `Arc`，引用计数不能证明没有并发线程或 late clone。

`load_elf()` 保留当前 TCB/PCB，不创建新进程。exec 改变的是执行映像：VM、trap context、
exe inode、部分 signal/futex/fd 状态；进程 PID、父子关系、进程组、会话和大多数进程
身份不变。这一点解释了为什么 shell fork 后 child exec，父进程 wait 的仍是同一个 child
pid。线程身份有一个 Linux 兼容例外：若调用者不是线程组 leader，成功提交后该 TCB
接管稳定的进程 PID，因此新的唯一线程满足 `gettid() == getpid()`。

PT_LOAD 首次 load/store/execute fault 会先分配一个尚未发布的私有页，再按 program-header
顺序覆盖与该页相交的文件字节；BSS 和页尾保持为零，重叠段权限按页取并集。
若源 PageCache 页尚未 resident，fault 先回滚未发布目标页，在释放 VM 锁后执行 I/O，
然后重试。主程序和 `PT_INTERP` 动态解释器均使用该路径；后备对象同时保持
inode/ETXTBSY 生命周期，并在 VMA 分裂、fork 和解释器映射中继续有效。仅文件系统无
PageCache 时返回 `ENOSYS`，回退到旧的内核临时映射 eager loader。PID1 `/init` 也优先使用同一按需路径。

## 9. 多线程 exec 语义

exec 成功后保留发起线程和当前 PCB，但 sibling 必须自行退出：

```text
begin_exec(owner_tid)
  -> 在线程组锁内安装 ExecSession，并关闭 clone 发布门
  -> 取得当时所有 live sibling 的 Arc 快照
request_sibling_exit()
  -> SIGKILL + wake + RESCHEDULE
  -> sibling 在所属 CPU 的 run_task_safe_point() 自行清理
  -> remove_thread() 在用户映射撤销和 TLB ack 后递减 live token
wait()
  -> live_threads == 1 时 Completion 唤醒 owner
install new image
finish()
  -> 清除临时会话并重新开放 clone
become_group_leader()
  -> 非 leader owner 接管 PID handle
  -> task registry 从旧 TID 重键为 PID
  -> 同步 Per-CPU current TID、OOM 索引、exit signal 与 thread quota
```

这不是 `exit_group()`；进程仍继续运行，只是线程组收缩到执行 exec 的线程。
永久 group exit 优先于临时 exec：owner 在等待返回后若观察到 group-exit 码，就先结束
临时会话而不提交新 VM，随后由统一安全点退出。门禁还会拒绝并发 exec 和尚未首次发布的
`CLONE_THREAD`，避免新 sibling 落在停止快照之外。

身份接管放在 `ExecSession::finish()` 之后：此时 live count 已经证明 owner 是同 PCB
唯一线程，不再需要改写线程组成员关系。registry 重键仍是一个独立短临界区，锁内不执行
TCB 或 TID handle 析构；Per-CPU 与 OOM 派生索引只在解锁后更新。旧 leader 即使仍由
zombie queue 持有，迟到析构也无法删除新 leader 的 PID 注册项。

## 10. VM 共享场景

`live_threads == 1` 是替换旧地址空间的权威条件，不能用 sibling 快照是否为空或 VM
引用计数代替。该计数只在 sibling 已完成 clear-child-tid、robust-list、用户映射撤销和
TLB shootdown 后递减，因此 Completion 返回意味着没有同 PCB 线程仍执行旧用户映像。

如果旧 VM 还被另一个 PCB 共享，例如 `CLONE_VM | CLONE_VFORK`，exec 也不能破坏对方 VM。
代码在提交时只撤销当前线程留在旧 VM 中的 trap context/默认栈映射，再让当前 PCB
`replace_vm(new_memory_set)`；旧 VM 由其他 PCB 的 `Arc` 保持存活。

```text
current_vm.dealloc_user_res_with_stack(current_slot, current_user_stack_allocated)
process.replace_vm(new_memory_set)
```

这样 vfork child exec 后脱离父 VM，父 VM 中不留下该 child 的 trap context/默认栈映射。

## 11. CLOEXEC、sighand、futex

exec 成功后：

| 资源 | 行为 |
|------|------|
| fd table | 引用唯一时原地关闭 `O_CLOEXEC` fd；被其它 PCB 共享时先复制，再只修改当前 PCB 的副本 |
| sighand | 引用唯一时原地重置；被其它 PCB 共享时先复制；用户 handler 恢复默认，显式 `SIG_IGN` 保留 |
| futex | 无条件换成新的 private table；旧表属于旧地址空间，且可能仍被其它 PCB 使用 |
| alt signal stack | disabled |
| robust list | reset |
| clear_child_tid | 清 0 |

`Arc::strong_count()` 必须在取得临时 `Arc` 副本之前读取，否则辅助函数自己的 clone
会把唯一对象误判为共享对象。旧 futex table 的析构会释放 waiter、Weak 与容器存储，因此代码
先把它移出 `ProcessInner`，释放 `process.inner` 锁后再 drop，避免析构链回入进程锁。

## 12. vfork 完成

`exec_opened_file()` 成功调用 `task.load_elf()` 后：

```rust
task.process.mark_execed();
task.set_exec_comm(&abs_path);
task.process.set_exec_identity(abs_path, &argv_vec);
task.process.complete_vfork();
```

因此 vfork 父线程在 child exec 成功后被唤醒。

## 13. 失败边界

如果 `load_elf()` 返回错误，该路径会：

```rust
exit_current_and_run_next(127);
```

这意味着 ELF 加载阶段失败不是简单返回 errno 给用户态，而是让当前任务以 127 退出。执行前的 open/metadata/argv/envp 校验失败才直接返回 errno。

## 14. 调试核对点

| 现象 | 检查 |
|------|------|
| 脚本不能执行 | shebang 前 128 字节、解释器 ELF、shell fallback |
| exec 大参数返回 E2BIG | 字符串总量和最终栈占用两层限制 |
| exec 后旧 fd 未关闭 | `close_cloexec_and_release_fcntl_locks()` |
| exec 后 signal handler 仍旧 | `reset_exec_resources()` 是否调用 `sighand.reset_for_exec()` |
| vfork 父线程卡住 | `complete_vfork()` 是否在成功路径调用 |
