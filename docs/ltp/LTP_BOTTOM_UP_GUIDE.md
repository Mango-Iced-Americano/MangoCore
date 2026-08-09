# LTP 自底向上适配指导方针

本文档用于指导 MangoCore 后续 LTP 适配工作。核心原则是：先稳住底层机制，再批量补表层兼容。越接近 VM、trap、task、VFS、signal、futex 等机制链路，越需要人工先确定不变量和错误语义；越接近 syscall stub、常量、虚拟文件、字段填充，越适合 AI 批量推进。

## 总体判断

当前内核已经具备运行基础用户程序和部分比赛 workload 的能力：

- 进程基础路径可用：`clone`、`execve`、`wait4`、`exit`。
- 基础 VFS/fd 路径可用：`open`、`read`、`write`、`lseek`、`getdents64`、`stat/fstat`。
- 基础 VM 路径可用：`mmap`、`brk/sbrk`、`munmap`、`mprotect` 主路径。
- 基础网络可用：TCP/UDP 可支撑 netperf-musl 一类测试。
- initproc runner 已支持 mask、order、timeout、LTP script/inline runner、include/exclude/from 过滤，适合做批量实验。

但当前还不适合直接无分层地全量铺 LTP。LTP 会大量喂非法参数、半合法内存区间、异常 fd、边界权限、信号中断、线程退出和文件系统异常状态。内核首先要做到“测例可以失败，但不能打崩内核”。

## 基本工作原则

1. 用户可触达路径不得 `panic!`、`todo!`、`unimplemented!` 或无保护 `unwrap()`。
2. syscall 参数错误优先返回 Linux 风格 errno，不应演变成 kernel trap。
3. 表层 syscall 适配必须服从底层机制不变量，不能为了单个 case 绕过 VM/VFS/task 规则。
4. 每次只推进一个子系统或一个 case 家族，避免 VM、VFS、signal、futex 同时变动。
5. LTP case 先分类，再修复：缺 syscall、errno 不符、runner/environment 问题、底层机制问题、应长期跳过。
6. LTP 始终最后跑；交互式、硬件相关、长期阻塞 case 先通过 runner 过滤。

## 当前代码审计基线

本节记录 2026-05-11 对当前 MangoCore 代码做静态审计后确认的具体风险点。它不是完整 bug 列表，而是 LTP 全面铺开前最值得先处理的底层阻塞项。每一项都给出代码证据、可能触发方式、影响的 LTP 家族和建议处理方向。

### 0. 审计结论

当前内核可以支撑一批浅层 POSIX/syscall case 的快速适配，但还不适合直接无分层铺全量 LTP。主要风险不是缺少几个 syscall stub，而是 VM、user-copy、VFS rename/unlink、clone/futex、signal frame 等底层路径仍存在以下问题：

- 用户态输入可触发 kernel panic、`todo!()`、`unwrap()`。
- user-copy 可能绕过 PTE 权限。
- 部分非法 syscall 参数会进入未定义行为或状态污染路径。
- VFS/ext4 某些路径可能导致内存中目录树与磁盘目录项不一致。
- 线程退出、futex wake、signal frame 损坏等场景仍需要机制级确认。

因此推进顺序应是：先消灭底层 panic 和权限绕过，再批量补 syscall、errno、procfs/devfs 字段和 runner 白名单。

### 1. P0/P1: user-copy 没有完整权限检查

> 状态更新（2026-08-01）：以下是历史 RED 代码证据。权限后验检查已完成；B57 又删除
> `translated_ref*` 并让固定对象/数组在逐页 VM 锁内复制。B58 又删除 `trans_ref!`/
> `trans_refmut!`，并收口字符串与 sockaddr 绕过路径。当前剩余风险是
> `translated_byte_buffer`/`UserBuffer` 的锁外物理页视图。

代码证据：

- `os/src/mm/page_table.rs:116` 的 `translated_byte_buffer` 只通过 `page_table.translate(vpn)` 检查页是否存在，然后返回 `&mut [u8]`。
- `os/src/mm/page_table.rs:202` 的 `translated_refmut` 只检查 `va < USER_VA_END`，随后 `translate_va` 并返回物理地址可变引用。
- `os/src/hal/arch/riscv/sv39.rs:251` 的 `translate_va` 只从 PTE 取 PPN 和 offset，没有检查 `U/R/W/X`。

具体风险：

- syscall 输出参数可能写穿 `PROT_READ`、COW write-protect 或非用户页。
- `copy_from_user` 和 `copy_to_user` 没有明确区分读用户内存与写用户内存所需权限。
- `translated_byte_buffer` 的 `start + len` 缺少溢出保护，极端参数可能绕过范围判断。
- LTP 的非法指针、跨页 buffer、只读 mmap buffer、signal stack 相关 case 会批量打到这里。

建议验证：

- 用户程序 `mmap` 一页，`mprotect(PROT_READ)` 后把该地址作为 `read()`、`getcwd()` 或 `clock_gettime()` 的输出缓冲区。
- Linux 语义应为 `EFAULT` 或向当前用户进程发保护类 signal；内核不应直接写成功，更不能 panic。

建议处理：

- 给 user-copy 层增加访问意图：`UserRead`、`UserWrite`、`UserReadWrite`。
- 翻译每页时检查 PTE 的 `U` 和对应 `R/W` 权限。
- 所有 user-copy API 先做地址加法溢出检查和 `USER_VA_END` 范围检查。
- 对非法用户地址统一返回 `EFAULT` 或由 trap 路径杀死当前进程，禁止 kernel panic。

### 2. P1: page fault 缺少访问类型，无法精确实现 mmap/mprotect 语义

代码证据：

- `os/src/mm/memory_set.rs:287` 的 `do_page_fault` 只接收 fault address，不接收 load/store/exec fault 类型。
- `os/src/mm/memory_set.rs:289` 用 `R | U` 寻找区域，`PROT_NONE`、exec-only、write-only 等场景无法精确区分。
- `os/src/mm/memory_set.rs:376` 后续根据 area 是否 `W`、PTE 是否已映射推断 COW 或权限错误。

具体风险：

- load fault、store fault、instruction fault 会被混在一起处理。
- `mprotect(PROT_NONE)`、只读页写 fault、exec-only 映射、file-backed fault 超 EOF 等 case 可能返回错误信号或错误 errno。
- user-copy 触发的 fault 与用户指令触发的 fault 语义边界不清晰。

影响 LTP 家族：

- `mmap*`
- `mprotect*`
- `munmap*`
- `fork/COW`
- `sigsegv/sigbus`

建议处理：

- trap 层把 fault cause 传入 `do_page_fault`，至少区分 read/write/exec。
- VMA 查询时按访问类型检查 `MapPermission`。
- 对 permission fault、not mapped fault、file EOF fault 分别映射到 Linux 风格 `SIGSEGV` 或 `SIGBUS`。

### 3. P1: mmap/munmap/mprotect 存在 panic 和 UB 路径

代码证据：

- `os/src/mm/memory_set.rs:856` 对 `MAP_FIXED` 使用 `unsafe { self.munmap(start, len).unwrap_unchecked() }`。
- `os/src/syscall/process.rs:1274` 对 mmap flags/prot 使用 `from_bits(...).unwrap()`。
- `os/src/mm/memory_set.rs:1022`、`os/src/mm/memory_set.rs:1068`、`os/src/mm/memory_set.rs:1074`、`os/src/mm/memory_set.rs:1079` 在 VMA split 上存在 `unwrap()`。
- `os/src/mm/map_area.rs:873` 的 `MapArea::into_three` 直接拒绝 file-backed area，但调用侧可能 unwrap。

具体风险：

- Linux 允许 `MAP_FIXED` 覆盖未映射区；当前若 `munmap` 返回 `Err`，`unwrap_unchecked` 是未定义行为。
- 非法 `prot/flags` 是 LTP 常见输入，当前路径可能 kernel panic。
- file-backed mapping 的部分 `munmap/mprotect` 可能因为 split 不支持而 panic。

影响 LTP 家族：

- `mmap05/mmap08/mmap09/mmap19`
- `mprotect*`
- `munmap*`
- file-backed mmap
- libc malloc 压力测试

建议处理：

- 删除 `unwrap_unchecked`，`MAP_FIXED` 应能处理覆盖未映射区、覆盖部分映射区、覆盖多个 VMA。
- 所有 flags/prot 从 `unwrap()` 改成显式校验并返回 `EINVAL`。
- `munmap/mprotect` 对 file-backed area 支持 split，或者明确返回 errno，不能 panic。

### 4. P2: OOM、lazy allocation、COW 中仍有直接 unwrap

代码证据：

- `os/src/mm/map_area.rs:539` lazy zero page 分配 `frame_alloc().unwrap()`。
- `os/src/mm/map_area.rs:730` COW 分配 `frame_alloc_uninit().unwrap()`。
- `os/src/mm/memory_set.rs:355`、`os/src/mm/memory_set.rs:366` swap/compressed page fault 路径存在 unwrap。

具体风险：

- LTP 内存压力 case 或多 case 连续运行时，frame allocation 失败会导致 kernel panic。
- fork+COW 压力下失败语义不稳定。

影响 LTP 家族：

- `mmap`
- `brk/sbrk`
- `fork`
- malloc/stress 类 case

建议处理：

- frame allocation 失败向上返回 `ENOMEM` 或杀死当前用户进程。
- page fault 内部避免 panic，把错误收敛到当前 task，而不是全局 kernel crash。

### 5. P2: brk/sbrk shrink 边界风险

代码证据：

- `os/src/mm/memory_set.rs:785` 的 `sbrk` 用 `old_pt + increment as usize` 计算新 break。
- `os/src/mm/memory_set.rs:817` 后续 shrink 计算区间并调用 `munmap`。

具体风险：

- 负数 increment 先转 `usize`，存在 wrap 风险。
- shrink 到异常地址后可能误删映射或触发 panic。

影响 LTP 家族：

- `sbrk01/sbrk02/sbrk03`
- libc malloc/free 压力测试

建议处理：

- 用 signed checked arithmetic 计算 new brk。
- 明确限制 heap 下界、用户地址上界和页对齐行为。
- shrink 失败返回 errno，不 panic。

### 6. P3: clone/thread/futex 有状态污染和丢唤醒风险

代码证据：

- `os/src/syscall/process.rs:659` 在创建 child 之后才处理 `CLONE_PARENT_SETTID` 写用户指针。
- `os/src/task/task.rs:700` 的 `TaskControlBlock::sys_clone` 已经把 child 挂入父子关系。
- 如果 `CLONE_PARENT_SETTID` 指针非法，`os/src/syscall/process.rs:668` 会提前返回错误，child 不会走到 `add_task`。
- `os/src/task/task.rs:660` 只有 `CLONE_SYSVSEM` 时共享 `task.futex`。
- `os/src/task/mod.rs:197` 的 clear_child_tid wake 使用翻译后的内核引用地址作为 key。

具体风险：

- clone 返回错误但系统内部留下一个未调度、非 zombie 的 child。
- futex wait/wake 的 key 如果与 clear_child_tid 使用的 key 不一致，pthread join/exit 可能丢唤醒。
- private futex 与 shared futex 语义依赖 clone flag 组合，后续遇到 musl/glibc 差异时容易不稳定。

影响 LTP 家族：

- `clone*`
- `pthread`
- `futex_wait*`
- `futex_wake*`
- robust futex

建议处理：

- clone 前先校验所有需要写回用户态的指针。
- child 进入全局 task 关系和 ready queue 应作为一个一致性事务。
- 统一 futex key 规则：private 使用 `(mm, va)` 或 task-local key，shared 使用物理页/inode-backed key。
- clear_child_tid 使用与 futex wait 完全一致的 key。

### 7. P4: signal frame 和 wait 状态编码不稳

代码证据：

- `os/src/task/signal.rs:440`、`os/src/task/signal.rs:484` 构造 signal frame 时 `copy_to_user(...).unwrap()`。
- `os/src/syscall/process.rs:1409`、`os/src/syscall/process.rs:1416`、`os/src/syscall/process.rs:1423` 的 `sigreturn` 从用户 frame 读取时存在 unwrap。
- `os/src/syscall/process.rs:817` 的 `wait4` 对 option 使用 `WaitOption::from_bits(option).unwrap()`。
- `os/src/syscall/process.rs:883` 写回 wait status 时使用 raw exit code。

具体风险：

- 用户栈坏、altstack 不足或 sigframe 被破坏时可能 kernel panic。
- 非法 wait option 应返回 `EINVAL`，当前可能 panic。
- `wait` status 不是完整 Linux 编码，会影响 `WIFEXITED/WEXITSTATUS/WIFSIGNALED`。

影响 LTP 家族：

- `sigaction`
- `sigsuspend`
- `sigaltstack`
- `sigreturn`
- `wait/waitpid/waitid`

建议处理：

- signal frame copy 失败时杀当前用户进程，不 panic。
- `sigreturn` 对坏 frame 返回用户态异常处理路径，而不是 unwrap。
- wait status 按 Linux wait status 编码写入。

### 8. P5/P6: VFS rename/unlink 存在明确语义错误

代码证据：

- `os/src/fs/directory_tree.rs:549` 对 rename 的 old/new path 使用 `assert!(starts_with('/'))`。
- `os/src/fs/directory_tree.rs:631` rename 创建新目录项时使用 `old_last_comp`，而不是 `new_last_comp`。
- `os/src/fs/directory_tree.rs:620` 替换已有目标时对 `new_par_inode.file.unlink(true)` 操作，疑似 unlink 父目录而不是目标 inode。

具体风险：

- `renameat/renameat2` 传相对路径会 kernel panic。
- VFS cache 中新路径存在，但 ext4 磁盘目录项可能仍是旧名字。
- 替换已有目标可能破坏父目录状态。

影响 LTP 家族：

- `rename*`
- `renameat2*`
- `unlink*`
- `rmdir*`
- `openat*`
- `getdents*`
- `stat*`

建议处理：

- rename 路径统一通过 dirfd + relative path resolver，移除 assert。
- 明确 old parent、old name、new parent、new name 四元组。
- 替换目标时 unlink 目标 inode/dirent，不操作父目录 inode 本身。
- rename 后校验 VFS cache、目录项、nlink、stat 结果一致。

### 9. P6: ext4 用户可触达路径还有 panic/stub

代码证据：

- `os/src/fs/ext4/layout.rs:524` 对 regular/dir/socket 之外类型 `todo!()`。
- `os/src/fs/ext4/layout.rs:562`、`os/src/fs/ext4/layout.rs:590` create 失败后 panic。
- `os/src/fs/ext4/layout.rs:799` truncate 失败后 panic。
- `os/src/fs/ext4/ext4_inode.rs:1104` inode allocation 失败后 panic。
- `os/src/fs/ext4/ext4_inode.rs:1114` dir entry type 转 mode 使用 unwrap。

具体风险：

- `mkfifo`、`symlinkat`、设备节点、特殊 inode 类型会触发未实现路径。
- inode/extent/dirent 内部错误会从单 case failure 升级成 kernel crash。

影响 LTP 家族：

- `creat`
- `ftruncate`
- `fallocate`
- `getdents`
- `mkfifo`
- `symlink/readlink`
- file mmap

建议处理：

- ext4 层所有用户可触达失败返回 `SyscallErr`。
- 特殊文件类型即使暂不完整支持，也应返回 `EOPNOTSUPP` 或 `EINVAL`。
- truncate/create/inode allocation 不允许 panic。

### 10. P7: proc/dev/tty 只能支撑浅层 case

代码证据：

- `os/src/fs/directory_tree.rs:777` 只初始化 `/proc/meminfo` 和 `/proc/mounts`。
- `os/src/fs/directory_tree.rs:699` 初始化的 `/dev` 仅有少量节点。
- `os/src/fs/dev/tty.rs:230` 的 `TTY::read_user` 持锁进入阻塞读取和调度。
- `os/src/fs/dev/tty.rs:304`、`os/src/fs/dev/tty.rs:348`、`os/src/fs/dev/tty.rs:449` 等 TTY trait/ioctl 路径存在 `todo!()`。

具体风险：

- `/proc/self`、`/proc/cpuinfo`、`/proc/stat`、`/proc/filesystems` 缺失会导致大量环境探测类 LTP 失败。
- TTY read 持锁跨调度点，交互类脚本容易卡死。
- 未支持 ioctl 不应 `todo!()`，应返回 `ENOTTY` 或 `EINVAL`。

影响 LTP 家族：

- procfs/devfs 探测
- termios/tty/ioctl
- shell/交互脚本

建议处理：

- 先补只读 `/proc` 最小兼容文件。
- TTY read 解锁后阻塞，避免持锁跨调度。
- TTY ioctl 未实现分支统一返回 errno。

### 11. P8: socket/message 和 Unix socket 深语义仍是后续专项

代码证据：

- `os/src/net/syscall/sendmsg.rs:38` 把 iov 全量聚合到一个 `Vec`，上限可达 64MB。
- `os/src/net/syscall/recvmsg.rs:109` 固定清空 `msg_controllen` 和 `msg_flags`。
- `os/src/net/socket/unix/mod.rs` 对不支持的 `socketpair` 类型返回 `ESOCKTNOSUPPORT`；`SOCK_SEQPACKET` 暂复用 stream 字节流，尚无记录边界。
- `os/src/net/socket/unix/datagram/mod.rs:168` 的 datagram `try_send` 仍为 `todo!()`。

具体风险：

- message 类测试会触发内存压力、control message 缺失、flag 不匹配。
- Unix `SOCK_SEQPACKET` 尚缺少记录边界、`MSG_EOR` 等深语义。

影响 LTP 家族：

- `sendmsg/recvmsg`
- `socketpair`
- Unix socket
- socket option/poll readiness

建议处理：

- `sendmsg` 改为分段处理 iov，避免大 Vec。
- control message 暂不支持时返回稳定简化语义或 errno。
- Unix socket 未支持类型返回 `EOPNOTSUPP/EINVAL`，不能 panic。

### 12. 表层 syscall/stub 适合后置批量处理

代码证据：

- `os/src/syscall/fs.rs:758` 的 `readlinkat` 只处理 `/proc/self/exe`，且 `bufsiz == 0` 时 `bufsiz - 1` 有下溢风险。
- `os/src/syscall/fs.rs:978` 的 `fchmodat/fchownat` 返回成功但不修改状态。
- `os/src/syscall/fs.rs:1458` 的 `fcntl(F_GETFL)` 硬编码 `O_RDWR`。
- `getrandom` 和 `/dev/{u,}random` 的全零/弱时间种子问题已于 2026-07-13
  由统一 ChaCha20 CSPRNG 与平台硬件熵源修复，后续随机数回归应使用
  `/bin/rng_test` 和 LTP `getrandom*`，不再按 stub 处理。

具体判断：

这些问题会导致 LTP fail，但大多不会破坏底层状态，适合在 P0-P6 稳住后由 AI 批量推进。优先级应低于 VM/user-copy/VFS/thread/signal 的 panic 和状态污染问题。

## 分层优先级

### P0: No-Panic 契约

目标：任何用户态输入都不能直接打崩内核。

重点清理：

- syscall 分发表和 syscall 实现中的 `todo!()`、`unimplemented!()`。
- VM、VFS、net、tty、signal 中用户可触达路径的 `panic!()`。
- 由用户输入间接触发的 `unwrap()`，尤其是 flags、prot、fd、path、dirent、inode、page table 操作。

验收标准：

- 未实现功能返回 `ENOSYS`、`EINVAL`、`EOPNOTSUPP`、`ENOTTY`、`ENOPROTOOPT` 等明确 errno。
- 非法用户指针返回 `EFAULT` 或向用户进程发送 `SIGSEGV/SIGBUS`，不触发 kernel panic。
- 单个 LTP case 失败不影响后续 case 和后续测试组。

### P1: 用户内存访问与 Page Fault

这是最底层的 LTP 地基。当前 `copy_from_user`、`copy_to_user`、`UserPtr`、`UserBuffer`、
`translated_byte_buffer` 必须维持统一的方向、缺页和部分完成语义。

需要人工先确定：

- syscall 中访问未映射但可 lazy alloc 的用户页时，是否允许触发 lazy allocation。
- 写只读页、写 `PROT_READ` mmap 区域、写 COW 页分别如何处理。
- `EFAULT`、`SIGSEGV`、`SIGBUS` 的边界。
- 跨页 buffer 中部分页非法时的返回策略。
- PTE 权限检查是否在 user-copy 层显式执行。

重点影响 case：

- `read/write/readv/writev/pread/pwrite`
- `mmap/mprotect/munmap`
- `futex`
- signal frame 写用户栈
- `statx/getrusage/sysinfo/uname` 等 copy_to_user 类 syscall

验收标准：

- 用户 buffer 越界、溢出、跨未映射页不会 panic。
- `mprotect(PROT_READ)` 后写入能得到稳定 Linux 风格行为。
- user-copy 不绕过页表权限。

### P2: VM 区间管理

目标：`MapArea` 的有序、不重叠、可拆分、可合并规则稳定。

复杂点：

- `MAP_FIXED` 覆盖已有区域的完整拆分。
- `munmap` 对完整区域、头部、尾部、中间洞的处理。
- `mprotect` 对区域拆分后的权限同步。
- `brk/sbrk` 收缩到非页边界、未分配 heap、重复 shrink。
- file-backed `MAP_PRIVATE` / `MAP_SHARED` 的 fault 和写回语义。
- COW、lazy allocation、swap/compress 状态之间的转换。

验收标准：

- `mmap/munmap/mprotect/brk` 任意非法组合返回 errno，不 panic。
- fork 后 COW 正确，父子互不污染。
- file-backed page fault 超过 EOF 返回 `SIGBUS`，不是随机错误。

### P3: clone/thread/exec/wait

目标：线程组、进程组、父子关系、退出回收的语义稳定。

需要人工把握：

- `CLONE_VM`、`CLONE_THREAD`、`CLONE_SIGHAND`、`CLONE_FILES` 组合语义。
- `CLONE_CHILD_CLEARTID` 退出时写 0 并 futex wake。
- `execve` 多线程进程时其它线程如何退出。
- `exit` 与 `exit_group` 的区别。
- `wait4` 对 child、thread、zombie、`WNOHANG` 的处理。
- `getpid/gettid/tgid` 一致性。

验收标准：

- pthread/fork/exec/wait 类 LTP 不出现僵尸泄漏和父子关系错乱。
- 线程退出能唤醒 join/futex 等等待者。
- `execve` 后 fd cloexec、signal handler、futex、robust list 状态符合预期简化语义。

### P4: Signal 与阻塞 syscall

目标：signal frame、mask、handler、sigreturn、syscall interrupt/restart 可控。

复杂点：

- `sigaction`、`sigprocmask`、`sigsuspend`、`sigtimedwait`。
- `SA_RESTART` 与阻塞 syscall 的返回。
- signal frame 的 `ucontext`、`siginfo`、restorer 布局。
- handler 地址非法、用户栈不足、sigreturn frame 损坏的处理。
- 多线程进程中 signal 发给进程还是具体线程。

验收标准：

- signal frame 写用户栈失败时杀用户进程，不 panic。
- 阻塞 I/O 被 signal 打断时返回 `EINTR` 或按 `SA_RESTART` 重启。
- `sigreturn` 恢复上下文不破坏 syscall 返回值。

### P5: Futex 与阻塞/唤醒模型

目标：futex key、等待队列、timeout、signal interrupt 行为一致。

需要明确：

- private futex 使用 VA key，shared futex 使用物理地址 key 的边界。
- COW、munmap、mremap 后 shared futex key 是否仍可接受。
- timeout 是相对时间还是绝对时间。
- signal 打断 futex wait 返回值。
- robust list 在线程异常退出时是否处理。
- `WAIT_BITSET`、`WAKE_BITSET`、`CMP_REQUEUE` 是否进入近期目标。

验收标准：

- 基础 pthread mutex/condvar 不死锁。
- futex wait/wake/requeue 不丢唤醒。
- timeout 和 signal 打断有稳定 errno。

### P6: VFS/ext4 一致性

目标：文件系统类 LTP 失败可以定位为语义缺失，而不是内部状态损坏。

复杂点：

- inode 分配失败、目录项解析失败、extent 操作失败全部返回 errno。
- `link/unlink/rename/rmdir/symlink/readlink` 的生命周期。
- `nlink`、mode、uid、gid、ctime/mtime/atime。
- open file 被 unlink 后仍可读写，目录项消失但 inode 生命周期保留。
- `truncate/ftruncate/fallocate` 与 page cache 同步。
- 目录项空间复用、目录为空判断、`.` 和 `..`。

验收标准：

- ext4 用户可触达路径无 panic。
- rename/link/unlink/rmdir/symlink 的基本 Linux 语义稳定。
- stat/statx 字段与 VFS 状态一致。

### P7: TTY、Session、Foreground PGID

目标：交互脚本、shell、password 类 LTP 不再卡死整轮测试。

复杂点：

- TTY read 不得持锁跨调度点。
- `/dev/tty`、`/dev/ttyS0`、stdin/stdout/stderr 一致。
- `TCGETS`、`TCSETS`、`TIOCGPGRP`、`TIOCSPGRP`、`TIOCGWINSZ`。
- foreground process group、`SIGINT`、`SIGTSTP`。
- nonblock read、poll readiness、EOF、signal interrupt。

验收标准：

- 交互式 case 可以被 timeout/kill 干净终止。
- TTY read 不造成死锁或调度器卡死。
- shell/job-control 相关 case 至少得到稳定错误或简化成功。

### P8: Socket 深语义

目标：基础 TCP/UDP 之外，补齐 LTP socket family 所需的 fd、poll、message 语义。

复杂点：

- `O_NONBLOCK` 与 `MSG_DONTWAIT`。
- `sendmsg/recvmsg`、`msghdr/iovec`、ancillary data 的简化策略。
- Unix socket pair 生命周期。
- shutdown 后读写返回值。
- socket poll readiness 与 `read/write/send/recv` 一致。
- setsockopt/getsockopt 常见选项。

验收标准：

- socket fd 的 `read/write/poll` 和 socket syscall 返回语义一致。
- 不支持的 socket option 返回正确 errno。
- Unix socket、socketpair、recvmsg/sendmsg 基础 case 不 panic。

## 适合 AI 批量推进的工作

以下类型可以在底层规则明确后批量处理：

- syscall 号、分发表、名称表。
- 简单 syscall stub：`sync`、`syncfs`、`fadvise64`、`clock_getres`、`capget/capset`、`close_range`。
- errno 调整和非法 flags 检查。
- `/proc/cpuinfo`、`/proc/stat`、`/proc/filesystems`、`/proc/version` 等虚拟文件。
- `/dev/null`、`/dev/zero`、`/dev/random`、`/dev/urandom`、`/dev/rtc` 简化语义。
- `statx`、`rusage`、`sysinfo`、`utsname` 字段补齐。
- `fcntl/ioctl/setsockopt/getsockopt` 常见命令分支。
- LTP include/exclude/from、case 分类脚本、日志汇总。

## 不应批量硬糊的工作

以下工作需要作为子系统设计任务推进：

- `mremap`、复杂 `mmap`、COW、swap、OOM。
- `clone3`、线程组、tid futex、robust futex。
- signal delivery、`sigaltstack`、`sigsuspend`、实时信号。
- symlink/rename/link/unlink 的完整 VFS 一致性。
- 文件权限模型、uid/gid/capability 的真实语义。
- Unix socket、raw socket、sendmsg/recvmsg ancillary data。
- keyring、bpf、namespace、cgroup、ptrace。

## 推荐推进流程

1. 建立 LTP case 数据库：记录 case、libc、架构、结果、失败原因、涉及子系统。
2. 先跑上一代 `ltp` 分支白名单中的基础 case，作为第一批回归集合。
3. 每轮只选择一个家族，例如 `fcntl`、`statx`、`mmap`、`futex`、`signal`。
4. 对每个失败 case 先分类：
   - runner/environment
   - 缺 syscall
   - errno 不符
   - 用户指针/VM
   - VFS/ext4
   - task/signal/futex
   - socket/tty/proc/dev
   - 长期跳过
5. 表层问题批量修，底层问题转为专项任务。
6. 每次修改后至少做：
   - `make -C os rv64-kernel-build-only`
   - `make -C os la64-kernel-build-only` 或 Docker 内等价目标
   - 相关 LTP focused run
7. 任何 panic 都优先级高于单个 case pass。

## 阶段性里程碑

### Milestone A: 打不崩

- LTP inline runner 可以连续跑基础白名单。
- 单 case timeout 后能继续后续 case。
- 用户可触达 panic 基本清零。

### Milestone B: 基础 POSIX 面

- open/read/write/stat/fcntl/dup/pipe/poll/ftruncate/fallocate 基础 case 稳定。
- `/proc` 和 `/dev` 最小兼容层补齐。
- musl LTP 基础白名单大部分可运行。

### Milestone C: VM 与线程

- `brk/sbrk/mmap/munmap/mprotect` 基础与边界 case 稳定。
- clone/thread/futex 基础 case 稳定。
- pthread 类程序不随机死锁。

### Milestone D: Signal/VFS 深水区

- signal handler、sigreturn、sigsuspend、sigtimedwait 稳定。
- symlink/link/rename/unlink/rmdir/truncate 基础语义稳定。
- glibc 环境不再成为大面积失败来源。

### Milestone E: 高阶选择题

- socket message、Unix socket、raw socket。
- mremap、clone3、robust futex。
- namespace/cgroup/keyring/bpf/ptrace 根据比赛收益选择性推进。

## 决策规则

- 如果一个 case 需要改 VM/page fault，不作为单 case 修复，升级为 VM 专项。
- 如果一个 case 需要改 signal frame，不作为单 case 修复，升级为 signal 专项。
- 如果一个 case 需要在 ext4 内部绕过错误，不做；先修 VFS/ext4 errno 边界。
- 如果一个 case 是硬件、交互、权限安全模型重度相关，先 skip 并记录。
- 如果一个 stub 返回成功会污染后续状态，不做成功 stub，返回明确 errno。
- 如果一个 stub 只影响只读查询类 syscall，且 Linux 上允许保守字段，可做简化成功。

## 文档维护

每完成一个专项，应更新本文档：

- 调整对应子系统的验收标准。
- 记录已经接受的简化 Linux 语义。
- 标记仍需人工处理的机制问题。
- 将已稳定的 case 家族移入回归集合。
