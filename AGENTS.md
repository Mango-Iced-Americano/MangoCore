# oskernel2026-mango — AI 开发助手指令

## 项目简介

`#![no_std]` 裸机 Rust 内核，支持 **riscv64** 和 **loongarch64** 双架构，通过 OpenSBI 在 QEMU 上运行。

| 属性 | 值 |
|------|-----|
| 语言 | Rust `nightly-2026-05-10`（单一根目录工具链合同） |
| 架构 | `riscv64gc-unknown-none-elf`、`loongarch64-unknown-linux-gnu` |
| syscall | 约 218 个（新增时同步更新本节） |
| 功能 | ext4/fat32/tmpfs/ramfs/procfs、smoltcp TCP/UDP/RAW/Unix、virtio 块/网卡、SV39 虚拟内存、SysV IPC、epoll/eventfd/signalfd/pidfd、POSIX timer |
| 设计参考 | [DragonOS](https://github.com/DragonOS-Community/DragonOS)（VFS/MountFS 架构）+ Linux 6.6 语义 |
| 验证 | 使用 Make facade：`make check ARCH=<rv64|la64> PROFILE=<normal|regression>`、`make lint`、编译与 QEMU 集成测试 |

---

## 不可违反的规则

1. **Docker 优先** — 所有编译/运行/调试在 Docker 容器内：`make docker`
2. **工具链 provisioning** — 根目录评测入口 `make all` 会派生 HOME 对应的 `RUSTUP_HOME`/`CARGO_HOME`，并在需要时自动执行 setup 和 preflight；全新容器首次运行可能使用网络。直接执行 OS、用户态或架构目标前，先运行只读的 `make toolchain-preflight`，这些入口不会自动 provisioning。手动流程仍可在容器内运行 `make toolchain-setup`
3. **不要并行编译双架构** — rv64 和 la64 共用单一根目录 `nightly-2026-05-10`，并写入共享的架构生成状态；必须分开命令行串行执行
4. **`lang_items.rs` 使用单文件 cfg 分支** — 内核的架构差异由 `#[cfg(target_arch = ...)]` 选择；不要再复制、生成或寻找 `.rv`/`.la` 变体
5. **验证强度匹配风险** — 文档/注释不编译；架构专用代码先验证受影响架构；共享生产代码在工作包或提交前串行完成双架构编译。SMP 代码按 [SMP Agent 执行规范](docs/10_plan/smp-agent-execution-spec.md) 的 T0-T3 分级执行
6. **核心功能按风险做 QEMU 测试** — trap、IPI、调度、MM/TLB、锁与用户可见语义必须做对应 focused QEMU；纯重构、诊断或未改变运行语义的修改不机械重复全矩阵
7. **修改 PTE 后必须刷新 TLB** — `sfence.vma`（riscv）/ `invtlb`（la64），这是最常见 bug 来源
8. **不要跨越等待点持锁** — 锁 → clone Arc → 释放锁 → 执行操作
9. **不要 workaround** — 从根因解决问题，不做临时绕过
10. **Mango Workflow 门禁** — 调试或代码任务首次写入前加载 `mango-workflow`；同一连续任务且 Skill 未变化时复用已加载状态，不为每个 patch 重复全文读取。完整工作包结束时执行 A→D（更新 Work Log → 判断经验沉淀 → 检查文档同步）。详见下方「Mango Workflow Skill 门禁」小节。
11. **回复中必须声明门禁状态** — 每个代码工作包完成后，在回复末尾注明 `mango-workflow: loaded/reused, references: <文件名或无>`。
12. **SMP 适配按完整功能推进** — 默认不设机械行数上限，尽量一次完成一个语义闭合的独立功能；只有多锁、复杂并发或高风险协议才把关键实现控制在约 200 行并申请人工确认。新任务首次修改前完整阅读 [SMP Agent 执行规范](docs/10_plan/smp-agent-execution-spec.md)；同一连续任务只重读当前 Phase 和相关章节。
13. **SMP 注释优先中文** — 并发不变量、内存序、锁顺序、BSP/AP 所有权和架构寄存器约束使用中文解释；专有名词、寄存器名和代码引用可保留英文。
14. **SMP 初赛非回归门禁** — 改变普通用户任务执行路径的 T3 节点、Phase 退出和合并候选，必须按 [SMP Agent 执行规范](docs/10_plan/smp-agent-execution-spec.md#82-双架构-8-核初赛非回归门禁) 串行执行双架构 `CORE_NUM=8`、`mask=0x003`，同时检查硬结束条件、raw judge 与受约束的 semantic 失败集合基线；纯文档/注释及不进入用户路径的私有 helper 不机械触发全门禁。
15. **提交必须单独批准** — 修改、验证和汇报完成后默认保留为未暂存工作树；只有用户明确批准当前批次提交时才可执行 `git add`/`commit`。未经批准不得自行提交、push 或创建 PR，也不得把提交动作外包给 DeepSeek。

---

### Mango Workflow Skill 门禁

`mango-workflow` 不是"事后写日志"，而是**前置知识门禁**。

**触发条件：** 调试、代码工作包首次开始，以及工作包完成收尾。同一连续任务中的
中间 patch、编译重试和文档收尾复用已加载状态。

**前置阅读：**
- 性能退化/计数器/QEMU 长测 → 先读 `references/harness-patterns.md`
- Bug 调试/LTP/子系统故障 → 先读 `references/debugging-patterns.md`
- 纯文档整理 → 可只加载 skill，不读 references，但需说明原因

**执行记录：** 在回复中写明 `mango-workflow loaded: yes/reused, references:
<section or "none — 纯文档">`。未完成工作包收尾不得声称任务完成。

---

## 编译与测试

### 日常编译

```bash
make docker
make kernel ARCH=rv64 PROFILE=normal            # RV64 内核
make kernel ARCH=la64 PROFILE=normal            # LA64 内核（必须在 RV64 后串行执行）
make build ARCH=rv64 PROFILE=normal             # RV64 完整产物
make build ARCH=la64 PROFILE=normal             # LA64 完整产物
make all                                        # 评测用串行双架构全量
make lint                                       # 四格首方 warning 基线门禁
```

### 测试镜像

```bash
make testsuits-download
xz -dkc fs-img-dir/sdcard-rv.img.xz > sdcard-rv.img
xz -dkc fs-img-dir/sdcard-la.img.xz > sdcard-la.img
```

### 测试配置

`os_test.conf` 的 `mask` 字段用 12-bit 控制测试组（**不要日常跑全量**）：

```
bit0=basic  bit1=busybox  bit2=lua  bit3=libctest  bit4=iozone  bit5=unixbench
bit6=iperf  bit7=libcbench bit8=lmbench bit9=netperf bit10=cyclictest bit11=ltp
```

常用 mask：`0x001`（basic）、`0x003`（basic+busybox）、`0xFFF`（全量，仅提交评测用）

配置注入镜像：
```bash
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt    CONF_FILE=../os_test.conf
make -C os conf-inject CONF_ARCH=la64 CONF_BLK_MODE=virt_pci CONF_FILE=../os_test.conf
```

LTP 本地调试：`ltp_runner=inline` + `ltp_include=read01,write01`（提交前恢复为 `suite`）

### 运行测试

```bash
make run ARCH=rv64 PROFILE=normal  # LOG=info 查看 syscall 追踪
make run ARCH=la64 PROFILE=normal
make -C os ktest-run ARCH=rv64 PROFILE=normal
make test ARCH=rv64 PROFILE=regression
python3 scripts/run_full_test.py --serial  # 全量一键（串行架构）
# docker-test-parallel 已弃用并 fail-closed；不得并行双架构构建
```

---

## 架构地图

### 启动流程

```
QEMU → OpenSBI (M-mode) → entry.asm (S-mode) → rust_main()
  → initramfs CPIO → VFS_ROOT → devfs bootstrap
  → 加载 /init（exec 到 /sbin/init）→ PID1 → test-runner
  → run_tasks()
```

### 系统调用

`syscall/mod.rs` 中扁平 `match` 分发。子模块按领域拆分：

| 模块 | 文件 | 范围 |
|------|------|------|
| 文件 I/O | `syscall/fs.rs` | read/write/openat/close/lseek/getdents64/stat/fcntl |
| 进程 | `syscall/process/{mod,mm,ids,signal,time,ipc,lifecycle,exec}.rs` | clone/execve/exit/wait4/mmap/brk/signal/timer/IPC |
| 网络 | `net/syscall/*.rs` | socket/bind/connect/sendto/recvfrom/accept |
| FD/事件 | `fs/eventpoll.rs`、`fs/eventfd.rs`、`fs/pidfd.rs` | epoll/eventfd/pidfd/signalfd |

**返回值约定**：syscall 处理函数成功返回 `>= 0`，失败返回负 errno（如 `-11` = EAGAIN）。

### 内存管理

- **物理内存**：栈式帧分配器，4KB/帧；`frame_store.rs` 跟踪帧状态用于 swap/zram
- **虚拟内存**：SV39 页表，每进程独立 `MemorySet`；`VmaSet` 管理 VMA；`filemap.rs` 处理 mmap 文件缺页
- **用户内存访问**：`UserPtr/UserPtrMut`、`copy_from/to_user`、`UserBuffer`、`translated_str`
- **关键约束**：MAP_SHARED 页面不参与 CoW；用户 PTE 必须经 `UserMapper` 修改并由 `MmuGather` 记录；进程 VM 由 `AddressSpace` 强制“锁内 `record_change`—`seal`—解锁—`TlbFlush::execute`—最后释放 frame”；`execve`/`clone` 路径用 `try_reserve` 防 OOM
- **双架构 ASID**：ASID 由每 MM 的 `TlbContext` 持有，同一 epoch 内不立即复用；RV64 从 `SATP.ASID` 探测容量，LA64 从 `CSR.ASID` 读取容量，编号耗尽时都必须先完成全 CPU user-TLB flush/ack 再换代。TCB 不得重新持有或释放 ASID
- **OOM 防御**：`alloc()` 三次重试失败后设 `pending_oom_kill`，由 `trap_return()` 安全点发 SIGKILL

### 任务/进程

SMP 过渡期的安全点抢占调度：current 槽、idle context 和 `RunQueue` 已按 CPU
拆分，AP 在 scheduler-ready 后安装内核页表并进入本地调度循环。focused ktest 的
短生命周期 kernel-only 任务可显式远程入队，并可在真实 WaitQueue 阻塞后回到最近运行
CPU；B28 还让一个无共享 I/O 的受控用户探针在 CPU1 完成 getpid/yield/exit，再由 CPU0
wait/reap。B29 进一步让同一个探针先在 CPU0 运行，并在真实 `sched_yield` 切回 idle 栈后
唯一迁移到 CPU1；一次性 `migration_target` 不是 owner，状态仍只走
`Running(0) -> Queued(1) -> Running(1)`。B30 又让 `getcpu()` 返回当前连续逻辑 CPU；探针
在迁移前后分别验证 0/1，node 在当前无 NUMA 拓扑下返回 0。B31 为每个 TCB 增加初始不变的
`cpus_allowed`：普通任务为 bit0，定向 kernel-only ktest 任务仅含目标 CPU 位，迁移探针为
CPU0/CPU1。Publish、yield requeue 和 blocked wake 都必须先验证目标在 mask 内。B32 已让
raw `sched_getaffinity()` 按 TID 返回这份 per-thread mask；成功值是复制的 8 字节，不是 libc
包装后的 0。B33 已把 timer 与 `RESCHEDULE` 合并到唯一 `run_task_safe_point()`：hard IPI
仍只置位，运行中用户任务在双架构 trap-return 的 IRQ-off 边界最多切换一次；focused probe
不再用显式 yield，而由 CPU1 IPI 驱动 CPU0→CPU1 owner 交接。B34 已开放 current 线程的
运行期 `sched_setaffinity()`：短 mask 按 Linux 低字节语义读取；新 mask 排除 source 时先同步
目标内核栈映射，Release 发布 mask/target，再在 syscall 安全点立即切回 idle，由单个目标
runqueue 完成 owner 交接。B35 又开放了远程稳定 `Blocked` 线程的写侧：只有目标仍以同一
TCB 指针登记在 interruptible registry 中，才在 `TASK_MANAGER` 锁内发布新 mask；后续 wake
在同一锁域读取 mask 并重新选 CPU，因此无需搬运 current/runqueue owner。B36 继续支持稳定
`Queued(owner)`：保留 owner 的 mask 在该 runqueue 锁内直接发布；排除 owner 时先同步目标
内核栈映射，再以短暂 `Migrating` 串行化“源队列摘除—mask 发布—目标队列插入”，全程不同时
持有两把 runqueue 锁，锁外才发送 RESCHEDULE。B37 又让普通 `publish_task()` 按继承的
affinity 与 `nr_running + current_present` 选择首次 owner；启动期无 current 的 init/ktest
runner 仍显式进入 CPU0，定向 ktest 的单 bit mask 则通过同一入口精确到达 AP。
B38 补齐远程 Running/Blocking 写侧，不增加 TaskStatus：新 mask 仍包含
Running owner 时在 per-task 请求槽锁内直接发布；排除 owner 时先同步目标
内核栈，再登记 `RemoteAffinityRequest` 并发 RESCHEDULE。源 CPU 只在安全点
切回 idle，完成 `Running(source) -> Queued(target)` 后才发布 Applied；
Blocking 窗口等待回到 Running 或进入 Blocked 后重试。请求槽锁不得跨 IPI、
TLB ack、context switch 或其它等待点。
B39 把全局调度 deadline 拆为每 CPU 独立的 100 Hz 绝对 deadline。hard timer IRQ
仍只发布 deferred 标志，真正调度只发生在既有安全点；CPU0 额外独占全局
timer/timeout/timerfd/net poll，AP 只推进本地 quantum。AP 插入更早的全局 timer 时，
必须先释放 timer queue 锁，再发送 `TIMER_REPROGRAM` 请求 CPU0 重编程；每个 CPU
都必须先写 deadline、编程 one-shot timer，最后才开放本地 timer interrupt。计数器可由
AP 原子累加，但会读取 FS/net 并打印 console 的格式化性能快照仍限定在 CPU0。
该能力不改变默认 mask：普通新任务和用户任务仍从 CPU0-only 开始。
B40 已把永久 `exit_group` 改为 owner 自清理：第一个调用者在线程组门禁内固定退出码、
关闭 clone 首次发布，随后只发送 SIGKILL/wake/RESCHEDULE；每个 sibling 在自己的安全点
完成用户资源/TLB 清理后递减 live token，最后一个 ack 独占 PCB/MM 收尾。首次 clone 的
成员登记与 `New -> Queued(cpu)` 在同一 `thread_group -> 单个 RunQueue` 临界区提交，
门禁关闭时返回 `EAGAIN`。`sleep_interruptible()` 登记 Blocking 后必须复查退出原子码；
退出清理统一在可响应 IPI 的受控中断窗口内进行。
B41 已把多线程 exec 改为临时 `ExecSession + Completion`：同一线程组锁拒绝
concurrent exec 和 late `CLONE_THREAD` 发布，owner 只通知 sibling 在所属 CPU
安全点自清理；live count 在用户映射撤销/TLB ack 后降为 1，owner 才替换 MM 并重新
开放 clone。永久 group exit 优先覆盖临时 exec；WaitQueue/Completion/vfork 等待必须
允许生命周期停止请求安全退栈。禁止恢复由请求 CPU 直接清理远端 Running sibling 的旧路径。
动态 kernel-global 映射已支持全 CPU 撤映射 ack 和内核栈延迟回收；用户
trap-return 已登记精确 active MM 驻留并追赶本地 generation，调度切离会在 idle 栈清 bit，
用户 PTE 修改已能在 VM 锁外完成 shootdown 和 frame 延迟释放；LoongArch 已使用
MM-owned versioned ASID，并在全 CPU flush/ack 后才复用编号；最多 64 页的连续区间在
RV64 使用逐页 `sfence.vma va, asid` 与 SBI RFENCE FID 2，LA64 通过每发起 CPU 固定
原子槽传递 ASID、起始 VPN 和页数，按硬件相邻偶/奇页对执行 `invtlb 0x5`；更大跨度全刷。
B53 已让软件精准 handler 在失效后、ack 前发布 observed generation，并用 CPU1 持续用户
load + CPU0 真实 CoW PPN 替换证明双架构旧翻译被直接消除，而不是由 trap-return 偶然全刷。
B54 已将 LoongArch 恒等映射的软件 dirty 表改为原子 bitset，且把 slab 的 unsafe trait
授权收窄到经全局堆锁证明的 `SlabAllocator: Send`；内部 page/list/cache 不再声明
`Send/Sync`。
B55 已让正常 console 输出统一经过“本地 irq-save → 全局 OUTPUT_LOCK → 架构 writer”；
panic handler 必须先调用 `console::enter_panic()`，后续输出绕过 OUTPUT_LOCK 与 LA64 UART
Mutex。新增 console 调用不得形成 UART/业务锁到 OUTPUT_LOCK 的反向顺序。
B56 进一步规定 panic 诊断不得调用阻塞统计接口：allocator/current/task/active-MM 只能走
`try_*`，锁忙时打印降级信息；逐 CPU 原子快照仅供诊断，不能用于调度或释放决策。
B57 已删除固定对象路径的 `translated_ref*`：标量/数组 copy 必须在同一次 VM 锁持有期内
完成 fault、权限后验检查和 raw copy，不能返回或保存指向用户物理页的 Rust 引用。进入
faultable uaccess 前必须释放 fd table、task inner 与 file-private 等普通锁。B58 已让
`translated_str` 逐页在 VM 锁内复制，删除 `trans_ref!`/`trans_refmut!` 与未使用的
sockaddr 裸指针解引用，网络地址先复制为内核所有快照再解析。B59 进一步删除
`translated_byte_buffer` 及锁外物理页 slice：`UserBuffer`/`UserIoVec` 只保存 token 与
逻辑 VA 区间，实际 copy 每页重新取得 VM 锁并验证 PTE。流式 I/O 返回已完成前缀，固定
格式用 `read_exact`/`write_all`；pipe 在 ring 自旋锁内只能使用已预 fault 的 nofault copy。
预 fault 只用于 ABI 副作用排序，不代替真正 copy 时的再验证。B60 已让 SysV semaphore
`GETALL/SETVAL/SETALL` 和 POSIX `mq_open(O_CREAT)` 采用“锁内验证 → 锁外 copy → 锁内
重验/提交”；任何 IPC registry 或队列锁内都不得进入 faultable uaccess。B61 又将普通
`msgrcv` 的消息选择、摘取和统计更新合并到同一个 `MSG_REGISTRY` 临界区，摘取后再锁外
写用户 buffer；`MSG_COPY` 只生成非破坏快照。不得恢复“锁内观察、解锁、事后按序号删除”
的两阶段领取方式，否则两个 CPU 可能成功返回同一条消息。B62 又把 message queue 的一次性
requested ID 与自动 cursor 分离，并在对象插入 registry 前登记发布历史；本次内核运行中
发布过的 ID 不再复用，`IPC_RMID` 只删除对象并唤醒 waiter，不能在删除路径临时分配
tombstone。旧 waiter 醒来后必须通过历史得到 `EIDRM`，不能命中同号新对象。该规则只覆盖
message queue 的 requested/auto ID。B63 对 semaphore 采用更小的不变量：等待 helper 只能
在同锁已证明对象存在后进入，ID 单调不复用，所以后续缺失直接表示 `IPC_RMID/EIDRM`，删除
路径不得分配 tombstone。shared-memory ID 同样使用 checked 单调游标，耗尽返回 `ENOSPC`，
避免两阶段 `shmat` 在回绕后把 attachment 归到新对象。B64 又把 futex 从通用 WaitQueue
拆为专用 waiter：requeue 必须先更新 current key 再发布到目标队列，wake 必须先发布
`woken` 再让任务 runnable，timeout/signal/waitv 按 current key 与 Arc 身份精确撤销；不得
再用“已离开 source 队列”推断正常 wake，也不得在注册后重读原 futex word 判定结果。
B65 又把 shared futex key 改为 backing `Arc` 身份与页内偏移：`AddressSpace` 在同一 VM 锁
内验证 VMA resident frame 与 PTE，释放 VM 锁后才进入 futex table；每个非空 shared queue
持有一份 backing pin，空队列删除才释放，禁止恢复 raw PPN 长期 key。该证明排除 PPN 复用
造成的错误命中。B66 进一步把 faultable 用户读取和 key 解析移到 table 锁外；最后一次
值比较只能在 table 锁内通过 VM `try_read` 完成，成功后在同一临界区发布 waiter。VM 锁忙、
PTE/权限或 shared backing 变化只能在发布前返回内部 Retry，释放 table 后重做锁外读取和
完整 key 解析；不得把 Retry 暴露成 errno，也不得恢复入队后的第三次用户读取。相对 timeout
必须先固定为绝对 deadline，waitv Retry 必须重建全部条目。`force_swap_out`/truncate 后的
新 backing 漏匹配及精确并发压力仍待处理。FS/Net/Driver 的完整共享状态审计仍由对应负责人继续。
不要把 B29/B30/B31/B32/B33/B34/B35/B36/B37/B38/B39/B40/B41 的受控迁移、真实 CPU/affinity 查询、
current/远程 affinity、Blocked/Queued 写侧、affinity-aware 首次放置和用户返回
RESCHEDULE/本地 timer 抢占、永久 group-exit 与临时 exec stop/ack 不得外推为以下
能力已完成：默认全核调度、非 leader exec 的完整 Linux TID/TGID 身份接管、
共享子系统全核审计、多写侧及 `mprotect/munmap` stale 权限/有效位压力、任意内核点抢占。

- **TaskControlBlock** — 线程级（调度实体、内核栈、trap context）
- **ProcessControlBlock** — 进程级（地址空间、fd table、信号、PID）
- **信号**：`task/signal/` 子模块（action/delivery/frame/pending/wait）
- **WaitQueue** — 支持 epoll、eventfd、IPC 等通用阻塞原语；futex 使用专用 waiter
- **Completion** — 单次通知原语

### 文件系统（VFS）

分层设计：`File`（fd 层）→ `IndexNode`（inode 层）→ `FileSystem`（FS 层）→ `MountFS`（挂载层）→ `PageCache`

| FS 类型 | 模块 | 说明 |
|---------|------|------|
| ext4 | `fs/ext4/` | 主力文件系统，含 extent 树、稀疏文件 |
| FAT32 | `fs/fat32/` | 引导/EFI 分区支持 |
| tmpfs | `fs/tmpfs/` | 无大小限制的临时内存 FS |
| ramfs | `fs/ramfs/` | 物理页支持的内存 FS（`/dev/shm` 等） |
| procfs | `fs/procfs/` | `/proc` 伪文件系统（含 `/proc/[pid]/status/maps/fd`） |
| devfs | `fs/dev/` | 设备文件（null/zero/urandom/tty/pipe/pty/rtc） |

**PageCache**：状态机（Loading→UpToDate↔Dirty→Writeback）；后台写回阈值约 32MB、节流阈值约 64MB，批量 256 页。`reclaim.rs` 周期性后台回收。

**镜像角色**：normal QEMU 固定为 `x0` 根文件系统/sdcard 与 `x1` 工具盘；`x1` 的 P1 是 ext4 工具分区，P2 是 FAT32 scratch 分区。regression 与 ktest 不挂外部磁盘。

**MountFS**：包装层，处理跨 FS 边界 lookup 和挂载传播（shared/private/slave）。

### 网络栈

```
syscall → Socket trait → TcpSocket/UdpSocket/RawSocket/UnixSocket
  → NET_INTERFACE (smoltcp Interface + SocketSet)
  → adapter.rs → virtio_net → QEMU
```

**I/O 阻塞抽象**：
- `try_xxx` — 单次非阻塞尝试，不 poll/yield/循环
- `wait_io` — socket 操作阻塞包装（每次重试前 poll）
- `wait_io_core` — 通用文件 I/O 阻塞包装（不 poll）
- 非阻塞路径（MSG_DONTWAIT）在 `try_xxx` 前必须 `try_poll()` 防 livelock

### IPC / 同步

| 机制 | 文件 | 说明 |
|------|------|------|
| SysV IPC | `syscall/process/ipc.rs` | msgget/semget/shmget 全套（msg/sem/shm） |
| futex | `task/threads.rs` | 专用 table/queue/waiter、wait/wake/requeue |
| epoll | `fs/eventpoll.rs` | 可扩展 I/O 事件通知 |
| eventfd | `fs/eventfd.rs` | 事件计数 fd |
| signalfd | `syscall/process/signal.rs` | fd 方式接收信号 |
| pidfd | `fs/pidfd.rs` | 进程 fd（open/send_signal/getfd） |

### HAL

`hal/` 目录提供硬件抽象层，将架构相关代码（陷阱处理、页表操作、TLB 管理、控制寄存器）从架构无关代码中分离。支持多平台（rv64: QEMU/K210/fu740、la64: QEMU/2k1000）。

---

## 编码规范

### 命名规则

| 模式 | 用途 | 示例 |
|------|------|------|
| `sys_xxx` | syscall 处理函数 | `sys_read`、`sys_sendto` |
| `_xxx` | 内部辅助函数（单次执行） | `_read`、`_connect` |
| `try_xxx` | 一次非阻塞尝试，返回 `Result` | `try_recv`、`try_send` |
| `socket_xxx` | socket 专用，避免与 `File` 冲突 | `socket_r_ready` |

### 可维护性与成熟实现对照

- 审查发现命名相近、调用链过深、职责重复或文件拆分难以理解时，先对照 Linux、
  DragonOS 等成熟内核的对应主线，再决定重命名或重构；协议与 ABI 优先查官方规范。
- 借鉴的是职责边界、生命周期和行业通用术语，不机械复制与 MangoCore 不匹配的层次。
- 一个生产语义只保留一个主调用链；新增类型、文件或 wrapper 必须能说明独立所有权或
  并发边界，不能只为转发参数。汇报时说明参考对象、采纳内容和未采纳原因。

### 返回值编码

| 层 | 成功 | 错误 |
|----|------|------|
| `File::read()/write()` | `usize`（字节数） | `usize`（`-(errno as isize) as usize`） |
| `Socket::try_recv()/try_send()` | `Ok(isize)` | `Err(SyscallErr::XXX)` |
| syscall 处理器 | `isize`（>= 0） | `isize`（负 errno） |

### 死锁预防

- 锁 → clone Arc → 释放锁 → 执行操作
- 信号检查必须在释放 `task.inner` 锁后调用 `has_actionable_signal()`
- `NET_INTERFACE.xxx_socket()` 闭包内保持简短

---

## 关键易错点

### 内存

- **TLB 刷新**：所有 PTE 修改操作（`unmap`/`block_and_ret_mut`/`set_pte_flags`）后必须 `sfence.vma`/`invtlb`
- **MAP_SHARED**：不参与 CoW，fork 时恢复 W 权限，缺页只恢复 W
- **OOM**：`execve`/`clone` 路径 Vec 扩容必须 `try_reserve` 返回 `ENOMEM`

### 网络

- Socket 就绪检查前必须 `NET_INTERFACE.poll()`（`socket_r_ready`/`socket_w_ready`）
- 非阻塞路径 `try_xxx` 前必须 `NET_INTERFACE.try_poll()` 防 livelock
- `impl_file_for_socket!` 宏自动生成 `File` trait（`read` → `try_recv`，`write` → `try_send`）

### 文件系统

- ext4 hole（稀疏文件）→ `get_pblock_idx` 返回 `Err`，`read_at` 填零，`write_at` 分配新块
- ext4 `binsearch_extent` 不验证覆盖范围 → 调用者必须检查 lblock 在 extent 范围内
- PageCache invalidate 时不能持有 inode 锁（`TicketMutex` 不可重入）

### 错误码（Linux 语义）

- setsockopt 未知 level/optname → `ENOPROTOOPT(92)`，不是 `EOPNOTSUPP(95)`
- socketpair 非 AF_UNIX → `EPROTONOSUPPORT(93)`，不是 `EAFNOSUPPORT(97)`
- `Socket::alloc` 未知 domain → `EAFNOSUPPORT(97)`，不是 `EINVAL(22)`
- getpeername NULL addr → 先验证参数返回 `EFAULT`，再检查连接状态
- mmap 非匿名映射坏 fd → `EBADF` 优先于其他校验
- 跨进程 VM 访问 → 先做权限检查（`EPERM`），再访问远程地址（`EFAULT`）
- RISC-V 未对齐 addrlen → 需显式检查 `addrlen % 4 != 0`，硬件不报错
- **errno 常量双取反** → 项目 errno 常量已定义为负 `isize`（如 `EINVAL = -22`），返回时不能再取反（`-EINVAL` 会变成正数被当作成功）。直接返回 `EINVAL`/`EAGAIN`。`return -ENOERR` 在本项目始终可疑。

### 信号

- **被屏蔽信号导致错误的 EINTR** — 信号检查必须用 `sigpending.difference(sigmask)` 过滤被屏蔽信号，不能用 `is_empty()`。忽略掩码会导致阻塞操作被不应唤醒的信号提前打断。
- **信号检查持锁** — 必须在释放 `task.inner` 锁后调用 `has_actionable_signal()`，否则死锁。

### 编译

- `cargo check` 必须从 `os/` 目录用 Makefile 目标，不能在根目录
- `Vec` 重复定义 → 检查是否同时 `use alloc::vec;` 和 `use alloc::vec::Vec;`
- lang_items 不匹配 → 检查单个 `lang_items.rs` 中的 `#[cfg(target_arch = ...)]` 分支；不复制架构变体文件

---

## 新增功能

### 新增 Syscall

```rust
// 1. syscall/syscall_id.rs: 添加 pub const SYSCALL_MY_FEATURE: usize = NNN;
// 2. 对应模块中实现: pub fn sys_my_feature(...) -> isize { ... }
// 3. syscall/mod.rs: 注册到 dispatch match 分支
```

### 新增 Socket 类型

```rust
impl Socket for MySocket { /* try_recv, try_send, socket_type */ }
impl_file_for_socket!(MySocket);
// 在 net/mod.rs 的 Socket::alloc() 中接入
```

### 新增块设备/网卡驱动

实现 `drivers/block/mod.rs` 的 `BlockDevice` trait 或 `drivers/net/mod.rs` 的 `NetworkDevice` trait。

### 验证清单

- [ ] 已按改动风险选择验证档位并说明理由
- [ ] 受影响架构构建通过；共享生产代码在提交/门禁前补齐双架构
- [ ] 改变运行语义时，对应 focused QEMU/测试组通过
- [ ] 工作包要求时执行 `make lint`，首方 warning 匹配四格基线
- [ ] 更新 `docs/Work_Log/YYYY-MM-DD.md`（按 mango-workflow Skill 格式）

---

## 工作日志与知识维护

### 前置参考（调试前先读）

调试性能退化、非确定性 bug、或遇到可疑模式时，先读取以下已沉淀的经验：

- **性能退化调试工作流** → `.agents/skills/mango-workflow/references/harness-patterns.md`（§渐进性能退化调试方法论）
- **常见调试模式和技巧** → `.agents/skills/mango-workflow/references/debugging-patterns.md`

这些文件记录了之前跨多个对话验证过的调试策略和修复模式，可以帮助快速定位问题类型并避免重复试错。

### 自动 Worklog

每个完整代码工作包结束时必须执行 `mango-workflow` A→D。同一连续任务只加载一次
Skill；中间 patch 不单独制造 Work Log 条目，编译错误修复、对称架构实现和文档同步
合并记录到工作包。新任务、Skill 已变化或上下文没有加载记录时才重新全文读取。

格式：日期戳条目 → 涉及文件 → 验证结果 → 备注。

### 经验沉淀

发现**可能跨对话复用**的 bug 模式或调试技巧时，追加到对应 reference 文件：

- Bug 根因 → 修复模式 → `.agents/skills/mango-workflow/references/harness-patterns.md`
- 调试技巧 → `.agents/skills/mango-workflow/references/debugging-patterns.md`

注意：已经在本文档「关键易错点」中覆盖的内容无需重复沉淀。

### 架构变更

修改核心架构（模块拆分、trait 变更、新子系统）时，必须同步更新本文档对应章节。

### LTP 测试详情

具体的 LTP 测试跳过策略、逐用例适配记录见 `Doc/Work_Log.md`。本文档只保留通用级别的规则和易错点。

---

## 交流语言

AI 助手使用**中文**与用户交流。代码、注释、commit message 使用英文或中文均可。

## 参考资源

- 设计蓝本：[DragonOS](https://github.com/DragonOS-Community/DragonOS)
- 详细测试策略：`Doc/Work_Log.md`、`Doc/LTP_BOTTOM_UP_GUIDE.md`
- VFS 迁移历史：`Doc/vfs-migration-plan.md`
- 调试技巧：`docs/08_testing/README.md`
