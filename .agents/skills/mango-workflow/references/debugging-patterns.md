# 调试模式库

> 跨对话可复用的调试技巧和排查方法。

## 同盘分区覆盖前的备份必须分块校验并延迟发布完成标记

- **场景**: 定向覆盖只读工具分区前，需要把原内容暂存到同盘的独立持久分区以支持回滚。
- **方法**: 强校验源/目标分区边界、挂载属性、设备 mode、文件系统 UUID/卷标和空间；按 bootloader 可加载的小块复制，每块重新读取源并与落盘文件做强哈希比较。仅在所有块完成并同步后原子发布 `COMPLETE`，中断残留不得进入写盘流程。
- **边界**: 同盘备份只防分区定向覆盖，不防整盘故障；源分区被挂载或运行时使用时不得原地恢复。覆盖器还应在首个写操作前从 bootloader 逐块读取备份，证明恢复材料脱离当前内核后仍可访问。
- **相关文件**: `scripts/board/backup_2k1000_p3.sh`, `scripts/backup_2k1000_p3.py`, `scripts/write_2k1000_p3.py`

## Codex 后台快照重复打包未忽略的大文件

- **现象**: 没有可见 Git 操作时，ChatGPT/Codex 主进程仍反复拉起多个 `git add --pathspec-from-file=- --pathspec-file-nul`，持续占满 CPU、磁盘并触发风扇；旧进程结束后新 PID 很快重生。
- **定位方法**: 用 `ps -ww -o pid,ppid,%cpu,command` 确认父进程，再用 `lsof -p <pid>` 查找 `/tmp/codex-index-*`、`/tmp/codex-review-objects-*` 和正在读取的大文件。临时 index/objects 表明这是 Codex 审查快照，不是仓库真实 `.git/index` 暂存。
- **根因**: Git 工作树内存在未被 `.gitignore` 命中的多 GiB 构建产物；Codex 为任务建立回滚/审查快照时会将其加入临时索引，多个任务还可能并发重复压缩。
- **修复**: 为对应生成物添加尽可能精确的根目录忽略规则（例如 `/tmp/*.img`），或把大文件移出工作树；用 `git check-ignore -v <path>` 验证规则。仅杀 Git 子进程只能暂时降载，下一次快照仍可能重启。
- **相关文件**: `.gitignore`

## 堆分配器性能退化

### buddy allocator free-list 线性扫描导致渐进退化

- **根因**: `Heap::dealloc()` 在合并 buddy 时线性遍历 size-class 的 free-list（`for block in free_list.iter_mut()`）。heap 碎片化后 free-list 变长，每次 dealloc 扫描步数从 19 爆炸到 114（6x），导致 open/close 退化 2.6x、fork+exit 退化 1.8x。
- **修复**: 加 per-class free-membership bitmap。dealloc 前 O(1) 查 bitmap — buddy 不在 free-list 中就直接跳过扫描。bitmap 内存从 heap region 前端 carve 出来（~4MB / 256MB heap）。
- **教训**: 渐进退化优先怀疑"有状态的数据结构"（free-list、hash table、LRU list）而非纯计算路径。用 per-call scan_steps 计数器可以精准定位。
- **相关文件**: `os/vendor/buddy_system_allocator/src/lib.rs:161`

## 启动/Panic 排查

### QEMU 启动无显示
- 检查 `console::init()` 是否第一个被调用（在 `rust_main()` 中）
- 检查串口设备初始化顺序

### 内核 panic 定位
- 启动时加 `LOG=debug make rv64-run` 查看详细日志
- 使用 GDB 调试：`make rv64-debug` → `b rust_main` → `c`
- panic 输出包含 syscall 上下文、内存状态、任务信息（`panic_diag.rs`）

### 编译问题
- `cargo check` 在根目录一定失败 → 始终在 `os/` 目录用 Makefile 目标
- `Vec` 重复定义 → 检查是否同时 `use alloc::vec;` 和 `use alloc::vec::Vec;`
- lang_items 不匹配 → 编辑 `.rv` / `.la` 变体，不编辑 `lang_items.rs`

## 内存问题

### unmap 后读到旧数据
- 典型 TLB 刷新遗漏 → 检查 PTE 修改后是否有 `sfence.vma`/`invtlb`
- 用 GDB `info tlb` 查看 TLB 条目

### 物理地址异常（如 0xb0000000）
- la64: 检查 `MEMORY_SIZE` 是否匹配 DTB 中 RAM 范围
- rv64: 检查 `device_tree.rs` 中内存区域解析

### 堆耗尽
- 检查是否有 `try_reserve` 防御
- 查看 `heap_trace.rs` 的分配记录（需启用 feature）

### bind/umount 后 `/proc/mounts` 仍有 sandbox 残留
- 症状：LTP `fs_bind*` 清理阶段反复提示 `There are still mounts in the sandbox`，`umount` 看似成功但同一路径仍出现在 `/proc/mounts`
- 优先检查：子 `MountFS` 是否还能通过 `self_mountpoint` 找到父 `MountFSInode`，以及父 `mountpoints` 表是否真正删除了该 inode id
- 典型根因：挂载点 backref 只保存弱引用或 overmount 旧挂载未走统一 detach，导致 `detach_from_parent_and_cleanup()` 无法摘除父表项
- 修复模式：保留稳定 parent backref，在 detach 时 `take()` 断开引用；覆盖挂载旧节点也走完整 cleanup，避免 dentry/child mount 缓存继续持有 covered subtree

## 网络问题

### Socket 操作阻塞不返回
- `connect` 不返回 → 检查是否使用 `try_connect` + `wait_io` 模式
- `accept`/`recvfrom` 不返回 → 检查 `wait_io` 中是否调用了 `NET_INTERFACE.poll()`

### 非阻塞 socket 测试失败
- 检查 `try_xxx` 前是否调了 `NET_INTERFACE.try_poll()`

### BusyBox DNS 正常但 glibc getaddrinfo 失败

- **现象**：`nslookup` 和按数字 IP 发起的 HTTP 请求成功，但 glibc 程序通过域名
  访问时返回 `getaddrinfo` 失败。
- **定位**：先用 syscall trace 观察 resolver UDP socket。glibc 会先设置
  `IP_RECVERR`，再用 `sendmmsg(269)` 同时发送 A/AAAA 查询；任一调用返回
  `ENOPROTOOPT` 或 `ENOSYS` 都会让 resolver 在 DNS 报文发出前失败。
- **修复**：为 UDP socket 保存 `IP_RECVERR` 状态，空 `MSG_ERRQUEUE` 返回
  `EAGAIN`；按 Linux 64-bit `mmsghdr` ABI 实现 `sendmmsg`，复用已有
  `sendmsg` 校验并逐项写回 `msg_len`。
- **教训**：不能用 BusyBox resolver 的成功推断 glibc NSS/resolver 已兼容；应以
  动态 glibc 客户端的域名请求作为端到端验收，并逐个补齐实际出现的 ABI，避免
  猜测式实现。
- **相关文件**：`os/src/net/syscall/setsockopt.rs`,
  `os/src/net/syscall/sendmmsg.rs`, `os/src/net/socket/inet/datagram/udp.rs`

## 信号问题

### 信号处理不生效
- 检查 sigaction 是否正确设置了 `SA_SIGINFO` 等 flags
- la64: 检查 `rt_sigaction` 的 sigsetsize 参数（libc 传 16 字节而非 8）

### 进程停止/继续状态异常
- 检查 `SIGSTOP`/`SIGCONT` 是否正确更新进程状态
- 检查父进程 wait 是否正确消费 stopped/continued 事件

## 性能问题

### la64 大量 page fault 慢
- 检查陷阱入口是否有不必要的 `invtlb`
- 检查页帧清零是否用了高效的 64-bit store 而非 byte-wise

### fork/wait 越来越慢
- 检查 TID 分配器是否有 O(n²) 查重
- 检查物理页释放是否有线性扫描 free-list

### lmbench 污染后 pipe/context switch/open/stat 同步变慢
- 先用 counters 区分旧目录线性扫描和 scheduler-loop 后台维护：如果 `dir_full_scan_entries=0`、dentry miss rate 稳定，但 pipe/context switch 也变慢，优先查调度循环里的 reclaim/stale prune。
- 对 scheduler-loop maintenance 做 rdcycle 分阶段计时，而不是只看总耗时。典型阶段包括 FIFO registry、ext4 registry、`prune_inode_objects`、`prune_children_stale_entries`、page cache metric、clean shrink。
- 如果 stale weak cleanup 每固定 tick 全量扫描 inode/children registry，即使 `stale_weak=0` 也会随缓存规模增长而拖慢非 FS microbench。修复优先级通常是降频、压力触发、dirty flag 或 incremental prune，而不是继续扩大 dentry cache。
- 相关文件：`os/src/fs/reclaim.rs`, `os/src/fs/ext4/ext4fs.rs`, `os/src/task/processor.rs`

### reclaim 平均成本下降但污染态 lmbench 仍退化
- 先同时看 `cycles_total/avg` 和 `cycles_max`。固定周期 batching 可能让平均值变好，却把原本分散的 stale weak cleanup 攒成单次长尾尖刺。
- 如果 S0 变好但 S1/S2b 的 `open/stat/pipe/read/write` 仍同步恶化，重点看 `prune_kids cycles_max`、`kids_removed` 和 budget hit，而不是只看 `prune_kids` 占比。
- 修复模式：把全量 prune 或固定间隔 batching 改成 cursor/budget 增量回收；每次 reclaim 限制 parent inode 和 child entry 扫描量，并输出 `scanned/budget_hit` 证明工作被分摊。
- 相关文件：`os/src/fs/reclaim.rs`, `os/src/fs/ext4/ext4fs.rs`

### budgeted reclaim 长尾下降但 total cycles 仍高
- `budget_hit=100%` 不一定表示真实 backlog 没扫完；如果 parent 预算按 raw registry entry 计数，budget hit 也可能只是 cursor 每轮正好扫满固定父项。
- 判断 S2/S2b 这类重污染场景时，要同时看 `kids_removed`、`kids_entries_scanned`、`kids_skipped` 和 `prune_kids cycles_total`。如果 `kids_removed` 很低但 cycles_total 很高，优先怀疑反复空扫/近空扫，而不是盲目扩大 budget。
- 修复模式：在 cursor/budget 基础上增加 dirty/event-driven generation；normal reclaim 在 generation 追平后跳过，heap pressure/critical 再 force scan，避免 stale Weak 自然过期没有回调导致永不清理。
- 相关文件：`os/src/fs/reclaim.rs`, `os/src/fs/ext4/ext4fs.rs`

### 对象数量 budget 无法约束 reclaim 单次长尾
- **现象**: shrink/reclaim 已有 entry budget，但 `cycles_max` 仍出现数量级尖刺；同时 `removed` 大幅增加，说明不是空扫，而是一次调用内连续处理大量可回收对象。
- **定位方法**: 把 `removed/scanned/budget_hit/skipped` 与分阶段 `cycles_max` 同看。若 entry 数很小但 max 很高，说明单个对象操作或连续 stale 段成本不可用“条目数”近似；需要增加 `time_budget_hit`/cycle slice 之类的直接时间片计数。
- **修复模式**: 在 cursor/budget 的基础上加 cycle 时间片；时间片命中时保存游标并返回 scheduler loop，未完成工作留到下轮。不要在 scheduler reclaim 路径里调用真正的 task yield，因为此时可能没有当前用户任务可安全挂回 ready queue。
- **相关文件**: `os/src/fs/reclaim.rs`, `os/src/fs/ext4/ext4fs.rs`

### heap_trace live 不回落但 PCB/TCB 正常
- 先区分真实生命周期泄漏和缓存型常驻：同时看 `zpcb/stale/tcb`、heap used、free frames、对象 owner。
- la64 需要额外检查架构特定 cache，例如 kernel stack 以 `Vec<u8>` 从 kernel heap 分配并可能被全局 cache 保留；1000 fork/futex 压力可把缓存打满，看起来像 heap leak。
- 资源报告也要一起查：`/proc/meminfo` 的 `MemAvailable` 如果只看 free frames，可能把静态预留但空闲的 kernel heap 漏掉，导致 LTP 大内存用例误判 `TCONF`。
- 修复模式：给大对象 cache 设置字节上限，保留小规模复用；对用户可见资源报告区分 `MemFree` 和估算型 `MemAvailable`。

### 用户 buffer 大小与实际 copy 长度不一致
- 症状：同一 syscall 在 glibc/musl 或不同架构下偶发 `EFAULT`，但实际输出内容很短，例如 `getcwd()` 只复制当前路径却按用户传入的 `PATH_MAX` 校验整段 buffer。
- 优先检查：syscall 是否用“用户声明容量”做 VMA 可访问性校验，而真实 `copy_to_user` 只会访问更短的 `write_len`。
- 修复模式：保留 Linux 语义需要的容量判断（如 `ERANGE`），但地址可访问性和 `UserBufferWriter` 长度按实际读写字节数校验。

## QEMU / 测试

### `make docker` 拉镜像超时但 Docker CE 源已换国内镜像

- **现象**: `apt update`/`apt install docker-compose-plugin` 已走清华等 Docker CE 软件源，但 `make docker` 仍在拉 `os-dev` 镜像时 timeout。
- **根因**: Docker CE APT 源只影响 Docker 软件包安装；`docker compose up` 拉取镜像走容器 registry（Docker Hub 或显式 registry 前缀），由 `/etc/docker/daemon.json` 的 `registry-mirrors` 或 compose 中的镜像地址决定。
- **修复**: 先用 `docker compose config` 确认实际 image，再用国内 registry 前缀或可用 daemon mirror 拉取；项目入口应支持 `DOCKER_IMAGE=...` 覆盖。
- **相关文件**: `docker-compose.yml`, `Makefile`, `scripts/run_test_docker_parallel.sh`

### `os_test.conf` 修改不生效
- 使用 `conf-inject` 重新注入镜像（不能直接改镜像中的文件）

### 对照实验必须同时确认 kernel 和 sdcard 用户态产物

- **现象**: 在旧 commit 上应用用户态 probe 后直接跑 `make rv64-run`，日志可能仍显示候选版本行为；或者重新跑了 `*-kernel-build-only` 后，QEMU 日志仍没有新增 probe 输出。
- **根因**: `make *-run` 的 `comp` 目标会直接使用已有 `../kernel-rv`/`../kernel-la`；`*-kernel-build-only` 会重建 user/initramfs/kernel，但不会自动更新测试盘 `sdcard-*.img` 里的 `/initproc`。如果 stage-1 实际执行的是测试盘旧 `/initproc`，用户态 probe 不会生效。
- **修复**: 做旧版/新版对照时必须先重建对应 kernel，再显式确认或注入测试盘上的 `/initproc`；可用配置行新增字段、输出字符串、二进制大小或 `debugfs stat /initproc` 确认实际运行产物。
- **教训**: 对照实验不能只看源码 HEAD；必须把内核产物、initramfs 产物、sdcard 用户态二进制和 `/os_test.conf` 四者同时纳入控制变量，否则容易把旧产物误当成原版或候选行为。
- **相关文件**: `os/make/rv64.mk`, `os/make/la64.mk`, `os/Makefile`, `user/src/bin/init.rs`, `user/src/bin/initproc.rs`

### 性能探针自身污染 scheduler/pipe 指标

- **现象**: 加了 rdcycle/atomic counters 后，scheduler loop_avg、pipe EAGAIN rate 或 lmbench group time 出现新的放大；报告把“未拆分的剩余开销”直接归因到架构路径。
- **根因**: 高频调度循环和 pipe read/write 每秒可能执行数十万到数百万次；即使单个 atomic/rdcycle 很轻，累计也会改变 QEMU 时序。跨架构绝对 cycles 还可能来自不同计数源，不能直接比较倍率。
- **修复**: profile counters 默认关闭，只在 profile_before reset 时打开，profile_dump 后关闭；同时保留一组无 profile baseline，用来量化探针自身税。跨架构先看同架构 S1/S0，再看 wall time 和标准化事件数。
- **教训**: “non-reclaim delta” 只是未解释桶，不等于 SBI/trap/TLB。必须先拆 console、timer、wake、futex、fetch、switch_prep 等 scheduler stage，再做核心修复。
- **相关文件**: `os/src/task/processor.rs`, `os/src/fs/dev/pipe.rs`, `user/src/bin/initproc.rs`

### trap/interrupt 计时跨 context switch 导致虚高

- **现象**: timer trap/handler cycles 总量远大于同一 profile 的 scheduler loop cycles，且单次 max 接近整段任务延迟；报告看起来像 trap 本体比其它架构慢几十倍。
- **根因**: trap handler 内部可能调用 `suspend_current_and_run_next()`。如果在调用前读 cycle、调用返回后才记录，计时会跨过“当前任务被调度走直到再次调回”的整段时间，而不是纯 trap handler 成本。
- **修复**: handler 本体计时必须在可能 context switch 前完成记录；trap 入口成本和 handler 成本分开记录。若需要观察“被调度走多久”，另加独立的 off-CPU/latency counter，不要混入 trap cycles。
- **相关文件**: `os/src/hal/arch/riscv/trap/mod.rs`, `os/src/hal/arch/loongarch64/trap/mod.rs`, `os/src/task/manager.rs`

### QEMU 进程残留
- `pkill qemu-system` 或 `pkill qemu`

### Docker Compose 进入了别人的容器

- **现象**: 在自己的工作区执行 `docker compose exec os-dev ...`，但容器内 `/app` 实际挂载到队友目录，后续 `docker cp` 或容器内写入会改到别人的源码。
- **根因**: Compose 默认从 `.env` 读取 `COMPOSE_PROJECT_NAME`。如果多个工作区使用了同一个 project name，`docker compose exec` 会连接到已存在的同名 service 容器，即使当前 shell 位于另一个源码目录。
- **修复**: 每个开发者/实验任务必须使用独立 project name，并在任何编译或 QEMU 前确认挂载：
  `docker compose ps`、`docker inspect <container> --format '{{range .Mounts}}{{println .Source "->" .Destination}}{{end}}'`。期望 `/home/<user>/projects/MangoCore -> /app`。
- **教训**: 结果报告的 manifest 必须记录 `COMPOSE_PROJECT_NAME`、container id、git HEAD、host cwd 与 mount 映射；若这些字段缺失，性能数据不能作为提交依据。
- **相关文件**: `.env`, `docker-compose.yml`, `cc-codex/results-*/manifest.json`

### la64 编译失败（61+ errors）— 缺少 initramfs 特性

- **现象**: `rv64-kernel-build-only` 成功，但同样的 initramfs 代码在 la64 上报大量编译错误
- **根因**: la64 的 `make/la64o.mk` 使用 `--no-default-features`，而 rv64 使用默认 features（含 `initramfs`）。la64 内核中部分代码路径只在 `initramfs` 特性 gate 下才编译通过
- **修复**: la64 构建必须显式传递 `initramfs` 和 `preload_payloads`：
  ```
  cargo build --no-default-features --release --features "comp board_laqemu block_virt_pci log_off initramfs preload_payloads"
  ```
  或通过 `make -f make/la64o.mk build EXTRA_FEATURES="initramfs preload_payloads"`
- **注意**: 根 Makefile 没有 `la64-kernel-build-only` 目标；`rv64_all`/`la64_all` 通过不同的 Makefile 目标处理特性
- **相关文件**: `os/make/la64o.mk`, `os/Makefile`

### LTP 特定用例调试
- 使用 `ltp_runner=inline` + `ltp_include=testname1,testname2` 窄范围测试
- 提交前恢复为 `ltp_runner=suite` 或 `ltp_runner=script`

### la64 clone09 停在 CLONE_NEWNET 后无 timeout

- **现象**: `ltp_runner=inline` 单跑 `clone09`，日志停在 `create clone in a new netns with 'CLONE_NEWNET' flag`，超过 LTP 标称 30s timeout 仍无 `TPASS/TFAIL/TBROK`，且没有 BTreeMap/heap panic。
- **根因**: la64 64KiB kernel stack 对 netns clone 路径栈深度不足；guard page 版本会避免静默 heap corruption，但容量仍可能导致该路径无法正常返回。
- **修复**: 将 la64 `KERNEL_STACK_SIZE` 提升到 128KiB，同时保留每 slot 的 guard page；重新编译并注入 focused LTP 配置验证。
- **教训**: clone/netns 路径挂住时不要只看用户态 LTP timeout；对比 kernel stack 容量和 guard 命中情况，若扩大栈后用例恢复，说明是栈深度问题而非测试 harness 或工具盘问题。
- **相关文件**: `os/src/hal/arch/loongarch64/config.rs`, `os/src/hal/arch/loongarch64/kern_stack.rs`, `os/src/hal/arch/loongarch64/trap/mod.rs`

### la64 全量压力触发 kernel stack slot 上限

- **现象**: la64 全量 LTP 跑到 syscalls 尾段的 `futex_cmp_requeue01` 后，大量 waiter 打印 `wasn't woken up: ETIMEDOUT`，随后出现 `[task_quota] SOFT LIMIT reached: used=921/1024`，最终在 `clone` 路径 panic：`la64 kernel stack slot 1024 exceeds max 1024`。
- **根因**: wait/reap 会先释放进程 quota，再调用 `remove_zombie_tasks_by_pid()`；退出路径已改用专用 `zombie_queue`，但该清理函数仍只扫描 ready/interruptible。于是 quota 计数下降，TCB 和内核栈却留在专用队列；现场 `921` 个计费任务加约 `103` 个漏清 zombie 栈正好占满 1024 个 slot。
- **修复**: `remove_zombie_tasks_by_pid()` 同时 retain/收集专用 `zombie_queue`，同步维护原子队列计数，并继续在 TASK_MANAGER 锁外 drop TCB。2K1000 实板双 libc 的 1000-waiter `futex_cmp_requeue01` 复验通过。
- **教训**: quota 只在“释放时刻与受限资源生命周期一致”时才是可靠容量边界。调度器增加新生命周期队列后，所有按 pid 回收路径必须同步覆盖新队列；不能通过扩大 slot 上限掩盖漏清理。
- **相关文件**: `os/src/hal/arch/loongarch64/config.rs`, `os/src/hal/arch/loongarch64/kern_stack.rs`, `os/src/task/quota.rs`, `os/src/task/task.rs`

### rv64 musl LTP retry helper 变成 UINT_MAX timeout

- **现象**: 某些本应很快 `TCONF` 的 LTP suite 用例在 rv64/musl 下逐个触发外层 60s per-case timeout；日志里 `tst_test.c` 打印 `Timeout per run is 1193046h 28m 15s`。
- **根因**: suite runner 注入 `LTP_TIMEOUT_MUL=2` 后，当前 rv64 musl LTP 镜像的 `strtod()`/浮点解析路径会把 timeout multiplier 算坏，LTP retry helper 变成 `UINT_MAX` 秒级重试。
- **修复**: 对 `rv64 + musl` 不导出 `LTP_TIMEOUT_MUL`，让 LTP 使用默认 multiplier；其它架构/libc 保持原超时放大。
- **教训**: 当 syscall 已返回预期 errno 但 LTP 没有进入 `TCONF/TBROK`，先查 `Timeout per run` 和 `TST_RETRY_FUNC`，不要直接把问题归到 syscall 阻塞。
- **相关文件**: `user/src/bin/ltprunner.rs`

### LTP timer test 固定小幅 oversleep

- **现象**: `clock_nanosleep02`/`nanosleep01` 每组样本都比请求时间稳定多睡约 0.5ms，`tst_timer_test.c` 报 `slept for too long`，但没有大幅长尾或随机卡顿。
- **根因**: syscall sleep 直接等全局 timeout 队列的真实 deadline，任务被唤醒并重新调度后存在固定尾部延迟；LTP 的截断均值阈值约 450us，尾部延迟会让短 sleep 全组失败。
- **修复**: timeout 队列提前一个很小的 guard 窗口唤醒，最后一段用短 `spin_loop()` 等到真实 deadline，避免早醒又降低调度尾部误差。
- **教训**: 看到所有样本都平移式 oversleep 时，优先看 deadline 唤醒后的调度尾延迟；若是少量异常大值，才优先查 timer interrupt、抢占或 QEMU 抖动。
- **相关文件**: `os/src/task/sleep.rs`

### LTP futex_wait timeout 固定小幅 oversleep

- **现象**: `futex_wait05` 中 `FUTEX_WAIT` timeout 样本稳定多睡约 0.5ms 到 0.8ms，`tst_timer_test.c` 报 `futex_wait() slept for too long`；基础 `futex_wait01-04` 和 `futex_wake01-03` 仍正常。
- **根因**: futex timeout 直接阻塞到真实 deadline，任务从 timeout queue 唤醒并返回用户态存在固定尾差；la64 QEMU 的短 timeout 出口尾差更大，10ms/25ms/100ms 档会超过 LTP 约 450us 阈值。
- **修复**: futex wait 在 deadline 前预留 guard 窗口，尾部仍保持在 futex wait queue 中自旋，期间继续检查 futex word、信号和是否被 `FUTEX_WAKE` 移出队列；仅 LA64 QEMU 对相对 `FUTEX_WAIT` 的中短 timeout 补偿固定出口尾差，2K1000LA 不应用该补偿。
- **教训**: futex timeout 精度不能直接套用 sleep 的“先出队再自旋”，否则可能丢掉 deadline 前的真实 wake；尾部自旋必须保持 wait queue 可观察或显式处理被 wake 移除的状态。
- **相关文件**: `os/src/task/threads.rs`

### 命名 FIFO 以 O_RDWR 打开后写入 EBADF

- **现象**: LTP `select01` 的匿名 pipe 通过，但命名 FIFO fd 在 `SAFE_WRITE()` 阶段返回 `EBADF`，尚未进入 select。
- **根因**: FIFO open 把 `O_RDWR` 解码为 `for_read=true, for_write=true` 后先命中只读分支，返回 `readable=true, writable=false` 的 Pipe inode；VFS File mode 虽为 RDWR，底层 inode 仍拒绝写入。
- **修复**: 在单向分支前处理双向模式，创建同时可读写的 Pipe endpoint，并把 ring 的读写端弱引用都指向该 endpoint。
- **教训**: 分层访问权限必须同时检查 fd/File mode 和底层对象能力；组合模式不能依赖两个布尔分支的先后顺序隐式表达。
- **相关文件**: `os/src/syscall/fs.rs`, `os/src/fs/dev/pipe.rs`

### libcbench pthread 超时但 futex 计数正常

- **现象**: libcbench `b_pthread_create_serial1` 卡到 120s timeout；clone/exit 计数已经接近完成，`fut_wait == fut_ready` 且无 futex timeout/intr，最后 syscall 长时间表现为 `read()` 返回 1024。
- **根因**: libcbench 的 `print_stats()` 会以 1KiB buffer 读取 `/proc/self/smaps`。如果 procfs 每个 chunk read 都重新生成完整 smaps，或者每次从第一个 VMA 扫到目标 offset，高 VMA/线程 churn 后会形成 O(N²) 级开销，看起来像 pthread/futex 卡住。
- **修复**: 对大文本 proc 文件采用 Linux `seq_file` 思路的 per-open 快照缓存：打开文件后生成一次文本，后续 offset 读取只切片复制；高 VMA 情况下 smaps 还应压缩非必要字段。
- **教训**: 性能测试超时要先用轻量计数排除同步原语本身；若 `last_sys=read` 且返回固定小块长度，优先查测试程序的统计读取路径和 procfs 生成策略。
- **相关文件**: `os/src/fs/procfs/mod.rs`, `os/src/fs/procfs/pid/smaps.rs`, `os/src/mm/address_space.rs`

### lmbench pipe 慢但 syscall 空转不慢

- **现象**: `Simple syscall` 已在微秒级，但 `Pipe latency`/`Pipe bandwidth` 仍明显偏慢。
- **根因**: pipe 走的是 VFS stream fd 路径；如果仍沿用普通文件的 offset 原子更新、append/seal/mtime 检查，或者每次 notify 前重新锁共享 ring 查询 peer，单次小包 ping-pong 会放大这些固定开销。
- **修复**: `FMODE_STREAM` 在 `File::read/write` 中直接调用底层 inode，不推进 offset；pipe 成功读写后复用 ring 锁内取得的 peer，并在无 fasync 监听者时跳过 `SIGIO` 空列表分发。
- **教训**: lmbench pipe 指标优先拆分 VFS stream 包装层、pipe ring、wait queue 三段看固定成本；不要只盯调度器或 copy 本身。
- **相关文件**: `os/src/fs/vfs/file.rs`, `os/src/fs/dev/pipe.rs`, `os/src/fs/vfs/fasync.rs`

### 页级 TLB 刷新疑似失效时用 full flush 做对照

- **现象**: PTE 权限或 PPN 已按预期更新，但同一用户 VA 仍反复触发相同类型 page fault；临时把页级 flush 替换为 full TLB flush 后问题消失。
- **定位方法**: 先保留 fault VA/PC/ASID/PPN 的窄范围日志，确认不是新的地址或新的权限错误；再做 page flush vs full flush 的最小对照。如果 full flush 有效而 page flush 无效，应检查架构指令的 ASID/global 参数、VPN 对齐和当前地址空间切换时机。
- **教训**: full flush 只能作为定位实验，不应作为最终 workaround。最终修复应让页级 invalidate 精确命中目标 ASID 或 global 映射，避免掩盖地址空间隔离 bug 和性能退化。
- **相关文件**: `os/src/hal/arch/loongarch64/tlb.rs`, `os/src/hal/arch/loongarch64/laflex.rs`

### getcwd 失败排查：区分 syscall 路径 vs libc manual walk 路径

- **现象**: musl `getcwd()` 报 "cannot access parent directories: Invalid argument"，但 glibc 的 `getcwd()` 正常。内核日志中看不到 `sys_getcwd` 调用。
- **定位方法**:
  1. 确认 libc 是否调用了 syscall — grep qemu.log 的 `syscall getcwd(17)`，如果只有 glibc 程序有、musl 程序没有 → libc 走了 manual walk 回退
  2. musl manual walk 使用 `fstatat("/")` 获取根 inode，再通过 `openat("..")`/`getdents` 逐级往上走，比较 inode 判断是否到根
  3. 比较 `fstatat("/")` 的 `st_ino` 和 `fstatat("..")` 的 `st_ino` — 如果不同，inode 不一致导致 musl 永远检测不到根
  4. 如果 inode 一致但 getdents 找不到匹配条目 → 检查 `d_ino` 是否与 `st_ino` 一致（bind mount 场景常见）
- **教训**: `sys_getcwd` 走的是 `FsStatus::working_path` 缓存（`cb9053a4` 引入），与 VFS 路径解析是两条独立链路。修复 `sys_getcwd` 不等于修复 VFS 层 ".." 语义。任何依赖 `find("..")` 的调用链都可能是下一个受害者。
- **相关文件**: `os/src/syscall/fs.rs` (sys_getcwd), `os/src/fs/vfs/mount.rs` (do_find, lookup_dotdot)

### init stage-1 后无输出先查首个外部等待点

- **现象**: QEMU 日志完成 net/block/mount 后停在 `[init] MangoCore stage-1 boot (initramfs mode)`，没有后续 bind mount 或 initproc 输出。
- **根因**: stage-1 init 打印该行后立即执行 NTP 同步；如果 `ntpd`、DNS、guest 网络或 timeout wake 路径卡住，父进程 `waitpid` 会让日志看起来像挂载阶段之后卡死。
- **修复**: init 阶段的外部依赖必须 bounded best-effort；NTP 子进程用 `waitpid_wnohang` 轮询加硬超时，超时后 `SIGKILL` 并 fallback。调度器中 legacy timeout sweep 的移除必须先通过 early boot、网络等待、timerfd/nanosleep/futex timeout 的有效验证。
- **教训**: 看到“最后一行”不要直接把责任归给上一行的模块。先沿源码确认下一条将执行的用户态/内核路径，再对比旧成功日志中 stage-1 后的第一条输出。
- **相关文件**: `user/src/bin/init.rs`, `os/src/task/processor.rs`, `os/src/task/manager.rs`

### 保留兜底语义，用 next-deadline gate 降低轮询税

- **现象**: scheduler loop 中的 legacy timeout/timer sweep 能保证正确性，但污染态下 pending flag 长期为 true，导致每轮调度都锁 queue/heap、读时间和扫描，`sched_stage_wake_expired` 成为主要 cycles 来源。
- **根因**: pending 只表示“队列非空”，不表示“当前已有 deadline 到期”。把非空队列当作每轮都需要处理，会把少量未来 timer 放大成每个 scheduler loop 的固定税。
- **修复**: 不直接删除 sweep；为 timer queue 和 timeout waitqueue 维护 cached earliest deadline。热路径先读 pending + next deadline，未到期直接返回；只有到期或 next deadline 不可信时才加锁处理。这个模式类似 Linux timer/hrtimer 使用 cached next expiry 决定下一次处理/重编程。
- **教训**: 对早期启动、NTP、nanosleep、futex timeout、timerfd 这类等待路径，正确性兜底往往比性能优化更早暴露风险。优化顺序应是“让兜底便宜”，不是先删除兜底。
- **相关文件**: `os/src/task/manager.rs`, `os/src/task/processor.rs`

### 随机接口“能返回数据”不等于具备安全熵

- **现象**: `getrandom()`、`/dev/urandom` 或 TLS 应用可以运行，但输出可能来自全零、时间戳或每次调用重新初始化的弱 PRNG；功能测试通过仍无法安全生成密钥、nonce 或 API 凭据。
- **根因**: 把硬件熵源、随机池就绪状态、CSPRNG 输出和用户 ABI 混成一个临时函数，没有区分“不可预测的可信播种”和“确定性的安全扩展”。
- **修复**: 平台层只负责采集可信熵；统一随机子系统执行启动健康检查、调理和 CSPRNG 状态管理；`getrandom` 与随机设备共用该状态并在未就绪时 fail closed。调用方写入只能混入状态，不能提高熵计数；QEMU 必须显式挂载 VirtIO RNG，实板按芯片手册使用片上来源。
- **教训**: 随机数验收至少同时覆盖来源识别、非全零、连续输出差异、非法 flag errno 和“缺少可信来源时不回退弱随机”。统计活性测试只能发现明显故障，不能证明熵率或替代硬件安全评估。
- **相关文件**: `os/src/drivers/rng/mod.rs`, `os/src/random.rs`, `os/src/syscall/mod.rs`, `os/src/fs/dev/urandom.rs`

### LoongArch AddressError 先查地址规范性，不要先查页表

- **现象**: 软件页表查询能找到 PPN，PGDH 也已设置，但对某个高虚拟地址首次 load/store 立即触发 `Exception(AddressError)`，没有进入预期的 TLB refill 或 page fault 路径。
- **根因**: 虚拟地址不符合处理器实际 `VALEN` 的规范地址规则。以 40 位 VALEN 为例，高半区从 `0xffffff8000000000` 开始；紧邻其下的地址属于非规范区，CPU 会在页表查询之前拒绝访问。
- **修复**: 从 `CPUCFG1` 解码 `PABITS=[11:4]+1` 和 `VABITS=[19:12]+1`，将平台 `PALEN/VALEN` 与硬件对齐；逐一验证 kernel stack、mmap、direct map 等固定窗口处于合法低半或高半区。用启动栈上的一次 volatile 写回读探针验证新映射，再进入 context switch。
- **教训**: `AddressError` 与 `PageInvalid*`/TLB refill 是不同故障层级。前者先查地址位宽和 canonical form；后者才查 PGDH、PTE、权限、ASID 和 TLB 刷新。软件 `mapped_frame()` 成功不能证明该虚拟地址对 CPU 合法。
- **相关文件**: `os/src/hal/arch/loongarch64/config.rs`, `os/src/hal/arch/loongarch64/kern_stack.rs`, `os/src/hal/arch/loongarch64/trap/mod.rs`

### LoongArch 地址位宽变更必须联审 VA、TLB、PTE 与 DMW

- **触发场景**: 修改 `VALEN/PALEN`、迁移高半地址窗口，或把 QEMU 地址布局带到实板。
- **最小审计矩阵**:
  1. `CPUCFG1` 的 `PABITS/VABITS` 与构建常量是否一致，`RVACFG.RBits` 是否改变有效 VALEN。
  2. `VA_MASK/SEG_MASK`、VPN 掩码和 VPN 符号扩展是否分别按 VA 位宽与右移后的页号表示计算。
  3. TLB VPPN 是否严格对应 `VA[VALEN-1:13]`，paired-page VPN 读回是否补回低位；TLB 页大小字段应写 `log2(PAGE_SIZE)`，不能写 `log2(PTE_SIZE)`。
  4. PTE/TLBELO 的 PPN 是否对应 `PA[PALEN-1:12]`，不要把 PALEN 位掩码整体再左移 12 位。
  5. 页表实际索引位数是否小于 VALEN；若软件只索引低 39 位，必须检查不同高位 VA 的页表别名和动态映射边界。
  6. 高物理 MMIO 地址是否能作为 canonical 页模式 VA；不能时使用正确 MAT 的 DMW CPU 别名，同时保持 DMA 地址为原始 PA。
  7. PGDL/ASID 切换与 `invtlb` 操作是否覆盖目标项的 global、ASID 和 paired-page 语义；ASID 分配失败哨兵绝不能直接写入 CSR。
- **验证方式**: 编译期断言固定关键边界；启动期打印并断言 CPUCFG；对 refill/restore 裸汇编做目标文件反汇编；最后必须实际进入用户态，只有内核早期日志不算通过。
- **相关文件**: `os/src/hal/arch/loongarch64/config.rs`, `os/src/hal/arch/loongarch64/laflex.rs`, `os/src/hal/arch/loongarch64/tlb.rs`, `os/src/hal/arch/loongarch64/trap/`, `os/src/drivers/block/sata_blk.rs`

### 片上 AHCI 上板先按 SoC 资料定址，再做只读分阶段验收

- **现象**: QEMU/通用 PC 经验会让驱动默认从 BAR5 找 AHCI ABAR，或直接扫描 PCI 后挂载磁盘；在 SoC 实板上可能找不到控制器、误碰保留寄存器，或在命令状态异常时永久自旋。
- **根因**: 片上 PCI 设备可以使用厂商固定的 BDF、BAR 和 DMA 约束，不一定遵循独立 PC AHCI 控制器的常见布局。2K1000LA 的 SATA 是 `00:08.0`，ABAR 位于 BAR0 `0x400e0000`；其 PCI capability pointer 还是保留字段，不能用“存在 capability list”作为 AHCI 前置条件。
- **修复**: 先交叉核对芯片手册、板级原理图、官方 U-Boot/Linux DTS；只读配置头并验证 vendor/device/class/prog-if/BAR；PCI Command 用 16 位访问避免写回 W1C Status；所有 GHC/PxCMD/CI 等等待必须有界并在错误中携带 `TFD/PxIS/PxSERR`；DMA 地址遵守控制器 mask 和平台一致性模型。`PxSIG` 只作为设备分类提示，部分 SoC HBA reset 后可能暂时读到 `0xffffffff`；链路已 active 时应由只读 `IDENTIFY DEVICE` 做最终判定，不能仅凭签名提前拒绝端口。
- **验收顺序**: `IDENTIFY DEVICE` 打印型号/容量 → 两次读取 LBA0 并比较 → 多个固定 LBA 只读比较 → 分区解析 → 只读文件系统挂载 → 最后才开放写入和 cache flush。每一步失败都保持 ramfs/initramfs 可启动，禁止直接解除块设备保护。
- **教训**: “AHCI 标准协议”不等于“PCI 集成方式标准”。上板时应把控制器定址、DMA 可见性、命令协议和文件系统挂载拆成独立验证层，避免把硬件探测问题误判成 ext4/VFS 问题，也避免尚未验证的写路径损坏 SSD。
- **相关文件**: `dependency/dep_iso/src/block/ahci.rs`, `os/src/drivers/block/sata_blk.rs`, `os/src/main.rs`

### 分区表 LBA 与内核块大小不能混用

- **现象**: MBR 中分区起点和容量看起来正确，但挂载后读不到 ext4 超级块；换成 1MiB 对齐分区后又能工作，容易被误判为磁盘或文件系统偶发故障。
- **根因**: MBR 字段始终以 512 字节逻辑扇区为单位，而内核 `BlockDevice` 的块大小随平台变化（当前 rv64/LA QEMU 为 4KiB，2K1000LA 为 2KiB）。直接执行 `start_lba / (BLOCK_SZ / 512)` 会截断未对齐起点，使所有分区内偏移发生偏移。即使分区起点正确，ext4 物理块号和 FAT 扇区号仍以文件系统声明的原生块大小为单位，不能直接当成 `BLOCK_SZ` 块号。
- **修复**: 分区设备内部保存字节起点 `start_lba * 512`；自然对齐访问直接转发整块，未对齐访问使用父设备块 bounce buffer。文件系统打开前再按 ext4 超级块或 FAT BPB 套 `BlockSizeAdapter`。文件系统识别必须先验证裸 ext4/FAT32，再解析 MBR；不能仅凭 `0x55AA` 把 MBR 当成 FAT。
- **验收**: 同时测试 raw ext4、非平台块对齐 MBR ext4、ext4 原生块小于 `BLOCK_SZ`、FAT 512B 扇区，并实际读取根目录，不能只检查魔数或“mounted”日志。GPT/扩展分区必须显式报告 unsupported；protective/hybrid MBR 不能退化成普通 MBR 挂载。
- **相关文件**: `os/src/drivers/block/partition.rs`, `os/src/fs/filesystem.rs`, `os/src/fs/mod.rs`

### 只读源挂载经 bind 后写操作进入文件系统分配器

- **现象**: 原挂载明确带 `RDONLY`，底层块设备也禁止写入，但 bind 视图上的 `mkdir`、`link` 或文件创建没有返回 `EROFS`，反而进入 ext4 分配路径并报告 `No free blocks`、`ENOSYS` 等误导性错误。
- **根因**: bind/recursive bind 或挂载传播在构造新 `MountFS` 时使用了本次 syscall 的 `MS_BIND/MS_REC` 或空标志，没有继承源挂载的 `RDONLY`。底层只读块设备只能阻止最终持久写盘，无法替代 VFS 挂载属性检查。
- **修复**: 明确区分挂载的持久属性与操作控制位；克隆挂载时从源挂载继承持久属性，并过滤 `REMOUNT/BIND/REC`。所有 `MountFSInode` 修改入口继续统一检查 `RDONLY` 并返回 `EROFS`。
- **教训**: 验证只读挂载不能只测试原挂载点或块设备写函数，还必须覆盖 bind、recursive bind 和传播副本，并至少测试创建、写入、链接、重命名与删除。出现底层 allocator 日志说明失败层级已经过晚。
- **相关文件**: `os/src/fs/vfs/mount.rs`, `os/src/fs/vfs/propagation.rs`, `os/src/syscall/fs.rs`

### 启动挂载正常但用户 mount 小扇区文件系统 panic

- **现象**: 启动阶段能识别并挂载 ext4/FAT，但用户态对同一个分区执行 `mount(2)` 时，在 `I/O length must be a multiple of the logical block size` 断言处 panic；典型请求是 512B FAT 扇区，而平台块为 2/4KiB。
- **根因**: 启动自动挂载调用了 `detect_fs_layout()` 和 `BlockSizeAdapter`，普通 mount syscall 却只做类型探测，随后把原始 `PartitionBlockDevice` 直接交给文件系统。两条打开路径的块大小语义不一致。
- **修复**: 所有块文件系统打开入口都必须保留完整的 `DetectedFs`，并在构造 ext4/FAT 实例前调用同一个原生块到平台块适配函数；`MS_RDONLY` 同时下沉为底层只读包装器。
- **教训**: “启动能挂载”不能覆盖用户态 mount 回归。多块大小验证必须包含设备节点路径的 `mount + I/O + umount`，并覆盖 512B FAT、1KiB ext4 和平台自然块三类组合。
- **相关文件**: `os/src/fs/mod.rs`, `os/src/fs/filesystem.rs`, `os/src/drivers/block/partition.rs`, `os/src/syscall/fs.rs`

### U-Boot 内存小于整盘镜像时通过网络分块写盘

- **现象**: raw disk image 大于开发板 DRAM，单次 `tftpboot` 无法加载，但 SSD 留在板上且只能通过网线和串口操作。
- **方法**: 按固定的 512B sector 整数倍切块，块大小必须落在已验证的空闲 DRAM 区间；逐块执行 TFTP、内存 CRC、`scsi write`、同 LBA `scsi read` 和读回 CRC，后一块起始 LBA 累加前一块 sector 数。写盘前硬匹配 `scsi info` 的型号与容量，镜像 sector 总数还必须小于设备容量。若只替换一个分区，使用带 role、SHA-256、目标起止 LBA 和 sector 数的清单，并要求命令行再次显式确认固定起点；不要让操作者传任意写入 LBA。
- **验收**: 所有块读回 CRC 一致后重新 `scsi reset`，检查 DOS/MBR 分区长度，再分别用 `ext4ls`/`fatls` 读取每个分区；分区 payload 还应从设备端加载一个本轮变化的哨兵文件，与宿主做长度/CRC 比对，证明写入的不是同布局旧镜像。最后启动目标内核验证设备节点、文件系统类型和实际挂载点。任何短传、短写、CRC 不一致或目标型号变化都立即停止。

### 在现有磁盘尾部新增分区时最后发布 MBR

- **风险**: 先写 MBR 分区项、再传输大 payload，会在网络中断或复位后留下一个可见但内容不完整的文件系统；后续启动可能把半成品当作合法持久分区。
- **修复**: 先验证旧 MBR 和目标尾部范围，逐块写入并读回整个 payload，全部成功后才写 512B MBR sector。提交后重新扫描分区表，从设备端加载哨兵文件并比对 CRC；MBR 提交或验证异常时立即用预先准备的旧 sector 回滚，`KeyboardInterrupt` 也必须进入回滚路径。
- **教训**: “payload 完整”和“分区已发布”是两个独立状态。发布动作必须最小、最后发生且可回滚；文件系统内部再使用提交标记区分完整业务状态与中断初始化。
- **相关文件**: `scripts/write_2k1000_p4.py`, `user/src/bin/initproc.rs`
- **实测参数**: 2K1000LA 的 6,443,499,520B 镜像使用 24 个 256MiB 块加 1 个 1MiB 块；256MiB 对应 `0x80000` sectors，加载地址 `0x9000000098000000`，目标为 `TS32GMTS400`。
- **相关文件**: `scripts/write_2k1000_p3.py`, `scripts/restore_2k1000_p2.py`, `docs/03_fs/2k1000-full-test-disk.md`

### 板型 feature 不能兼任 bring-up 日志开关

- **现象**: 首次上板时加入的 CPUCFG、地址布局、内核栈读写、首任务切换和用户态返回探针长期绑定在 `board_xxx` 上，导致后续每个正式板级镜像都携带大量串口输出和一次性探针开销。
- **根因**: 硬件选择与诊断策略使用了同一个 feature；“运行在实板上”被错误等同于“始终处于 bring-up 阶段”。仅把日志级别设为 off 无效，因为 `println!` 和主动探针不经过 `log` facade。
- **修复**: 板型 feature 只负责链接地址、入口和驱动选择；另设默认关闭的诊断 feature。成功型早期输出使用统一编译期宏，带额外读写或原子状态的探针则整段 `cfg` 移除；panic 和真实错误路径不应静默。
- **验收**: 除双架构编译外，必须直接扫描最终 uImage/ELF，确认调试字符串不存在，同时确认正式配置与错误诊断字符串仍在；不要只搜索源码或依赖 `LOG=off`。
- **相关文件**: `os/Cargo.toml`, `os/src/console.rs`, `os/src/main.rs`, `os/src/hal/arch/loongarch64/`, `os/Makefile`

### U-Boot 串口自动化必须由 prompt 和内容校验驱动

- **现象**: 把多条 `setenv/tftpboot/bootm` 用固定 sleep 或一次性串口注入时，U-Boot 可能仍在网卡协商、TFTP 或 CRC，后续字符被丢弃；最终表现为偶发找不到镜像、命令截断或在未校验镜像时直接启动。
- **根因**: U-Boot 各命令耗时不固定，串口发送成功不代表命令执行完成；只检查 TFTP 返回也无法发现短传、错误文件或内存内容损坏。
- **修复**: 每条控制命令都读取到完整 `=>` prompt 后再发送下一条；TFTP 后同时校验 `Bytes transferred`、本地与 U-Boot CRC32，再用 `iminfo` 确认架构和镜像 checksum。`bootm` 之后切换为双向串口透传，`Ctrl-C` 发往目标，本地使用 `Ctrl-] q` 退出监视器。
- **安全边界**: 网络参数只用 `setenv`，禁止自动 `saveenv`；普通启动脚本禁止包含块设备写命令。自动接管串口时只关闭能明确匹配同一设备路径的 screen，未知占用者必须报错停止。
- **相关文件**: `scripts/boot_2k1000_tftp.py`, `Makefile`

### AHCI HBA reset 后不能假定 CAP/PI 保持不变

- **现象**: 同一控制器和 SSD 在 U-Boot 执行过 `scsi scan` 后可用，但直接 TFTP/`bootm` 时内核报 `NoUsablePort { implemented: 0, ... }`；PCI ID、class 和 ABAR 都已验证正确。
- **根因**: 部分片上 AHCI 控制器的 HBA reset 会清空可写的 CAP 和 `HOST_PORTS_IMPL`。只恢复 PI 时端口虽然可枚举，但 CAP.SSS 丢失会导致 `PxCMD.SUD` 被硬件清零，暖复位链路停在 `DET=1`。若 bootloader 先扫描磁盘，它可能已经回写这些寄存器，从而掩盖内核初始化缺失。
- **修复**: 对照厂商 U-Boot/Linux保存掩码和强制位：reset 前保存平台声明的 CAP 子集，reset 后先恢复 CAP、读回刷新，再恢复 PI；2K1000 保存 CAP bit28/17、强制 bit27 SSS，并写 `PI=0x0f`。未知平台默认不写只读 CAP，不能套用固定值。
- **教训**: “同一镜像偶尔能识别 SSD”要检查 bootloader 前置命令是否改变了控制器状态。内核驱动必须从其声明的硬件初始条件独立建立完整状态，不能依赖人工调试命令的副作用。
- **相关文件**: `dependency/dep_iso/src/provider.rs`, `dependency/dep_iso/src/block/ahci.rs`, `os/src/drivers/block/sata_blk.rs`

### FAT rename 不能复用 link + unlink

- **现象**: FAT32 上文件创建、写入和删除均正常，但 `mv old new` 返回失败；目录 rename 后的 `rmdir new` 随之报目标不存在。
- **根因**: VFS 默认 rename 通过 `link(new) + unlink(old)` 实现，而 FAT 标准没有硬链接，`link()` 必然返回不支持。外层测试脚本还可能无条件返回 0，所以只看组退出码会形成假通过。
- **修复**: 文件系统实现原生目录项 rename：保留源短目录项的首簇、大小、属性和时间，只生成新短名/长名；创建新项后删除旧项并同步父目录，删除失败则回滚新项。跨目录还需更新目录 `..`，覆盖目标还需保存和回滚目标，未实现前必须显式拒绝。
- **验收**: 同时检查测试脚本逐命令输出、旧路径消失、新路径可访问及后续删除成功；至少覆盖普通文件和空目录，不能只检查外层脚本 exit 0。
- **相关文件**: `os/src/fs/fat32/fat_inode.rs`, `user/src/bin/initproc.rs`

### 块设备写路径开放前使用分区外自恢复探针

- **场景**: 读取、分区识别和只读挂载已经通过，但 DMA 写命令、设备 cache flush 与持久性尚未在实板验证。
- **方法**: 独立 feature 硬匹配设备型号和分区表身份；动态计算最后一个分区末端，在明确 guard 后选择小范围扇区。先备份，再写测试模式、flush、读回比较，最后无条件恢复备份、再次 flush 并读回验证。
- **失败策略**: 第一次写命令发出前的错误可以安全返回；之后即使命令报告失败也要假定介质可能已修改并尝试恢复。恢复失败必须停止系统，不能继续挂载或运行测例。探针构建保持 ramfs-only，正式镜像继续只读。
- **教训**: “驱动存在 write_block”不代表文件系统可以改成读写。先验证原始命令和 flush，再开放单独 scratch 分区，最后才允许文件系统元数据写回。
- **相关文件**: `os/src/drivers/block/sata_blk.rs`, `os/src/main.rs`, `os/Makefile`

### SATA 暖复位后 PxSSTS=1 需要定时 COMRESET

- **现象**: 冷启动或前一轮探针可以识别 SSD，紧接着按板载 RESET 再启动时却报 `LinkTimeout { sata_status: 1 }`；尚未进入任何文件系统写操作。
- **根因**: `DET=1` 仅表示检测到设备但 PHY 通信未建立。按 CPU 速度相关的固定循环轮询可能在链路完成协商前就耗尽，而且 HBA reset 不等于 SATA PHY COMRESET。
- **修复**: Provider 提供基于架构 stable counter 的真实微秒延时。先给正常 spin-up 一个短时间窗口；未 active 时按 AHCI/Linux 顺序写 `PxSCTL.DET=1` 保持至少 1ms，再写 `DET=0` 释放，并在真实 10s 上限内等待 `DET=3, IPM=1`。
- **教训**: 硬件协议中的毫秒/秒级 deadline 不能用无时间基准的 spin 次数表达；冷启动成功不能覆盖暖复位回归。
- **相关文件**: `dependency/dep_iso/src/provider.rs`, `dependency/dep_iso/src/block/ahci.rs`, `os/src/drivers/block/sata_blk.rs`

### FAT32 写入后重挂载出现空文件或已删除项复现

- **现象**: `write()` 返回完整字节数，原始块写入和 flush 也已通过，但重新打开文件时长度为 0；`unlink()` 后新 inode 执行 `rmdir()` 报 `ENOTEMPTY`；或同一启动中创建/访问成功，换一份根 inode 后 `rmdir()` 报 `ENOENT`。
- **根因一**: `BlockSizeAdapter` 已把 `BlockDevice` 的 block id 统一为 BPB 扇区单位，FAT 层又按全局 `BLOCK_SZ/512` 二次换算，实际访问了错误 FAT sector。块大小适配边界不清晰会让数据区写入看似成功而 FAT 链损坏。
- **根因二**: 文件大小、首簇和删除目录项只在 `FatInode::drop()` 中回写。VFS 引用或独立 page cache 延长生命周期时，新 `find()` 构造的 inode 会直接读取尚未落盘的旧目录项；Rust 对象析构不是文件系统持久化协议。
- **根因三**: `EasyFileSystem::root_inode()`/`find()` 可生成同一磁盘对象的独立 inode/PageCache。create 只修改旧父目录缓存时，路径 dentry cache 会让后续 open 暂时成功，但另一份根 inode 直接读盘时完全看不到该目录。
- **修复**: FAT 内部只使用 BPB 声明的扇区单位，检查簇号 `2..cluster_count+2`、FAT 容量、双 FAT 镜像和 ExtFlags；write/resize 显式更新短目录项，inode `sync()` 依次写回数据页、父目录项和父目录页，create/unlink/rmdir 在成功返回前持久化目录页；stale inode `Drop` 不再修改父目录元数据。
- **验收**: 探针必须跨新文件系统实例完成 create/write/flush/reopen/read/content-compare/unlink/rmdir/final-reopen，不能只在同一 inode/page cache 内回读。失败后用受限分区恢复工具重建 scratch 分区，避免在已损坏 FAT 上反复试写。
- **相关文件**: `os/src/fs/fat32/bitmap.rs`, `os/src/fs/fat32/efs.rs`, `os/src/fs/fat32/fat_inode.rs`, `os/src/fs/mod.rs`, `scripts/restore_2k1000_p2.py`

### 只读测试源与可写运行目录应显式分层

- **现象**: 测试二进制和脚本位于只读 ext4，初始化阶段安装 busybox applet、动态库链接或测例创建临时文件时大量返回 `EROFS`；直接把整个测试分区改成可写会扩大介质损坏范围。
- **修复**: 保持工具和测试源只读，保留 initramfs 的 `/bin`、`/lib`、`/usr` 作为易失可写运行时；只把即将执行的测试组复制到隔离的 FAT32 scratch 工作区。工作区复制后校验入口脚本、runner 和工具文件，准备失败时拒绝回退只读源，避免脚本缺失却返回 0 的假通过。
- **FAT32 注意项**: BusyBox `cp -R` 可能在数据复制成功后尝试恢复目录 mode，而 FAT32 `set_metadata` 返回 `ENOSYS`。可屏蔽这类已知诊断，但必须保留 `cp` 退出码并执行文件完整性校验。不要用未声明局部变量的递归 shell 函数替代复制：BusyBox shell 函数变量默认可污染父递归层的源/目标路径。
- **验收**: 测例日志必须打印工作区准备成功，`getcwd` 应指向 scratch，且每个实际子项有 START/END；仅检查外层脚本退出码不足以证明测试执行。复位启动时还应确认 bootloader 没有运行会改变控制器状态的存储命令。
- **相关文件**: `user/src/bin/init.rs`, `user/src/bin/initproc.rs`, `docs/03_fs/2k1000-full-test-disk.md`

### 多调用 benchmark 的最小 payload 要审计隐藏文件依赖

- **现象**: benchmark 已复制到可写工作区，外层脚本运行到 END 且退出 0，但某个子项仍打印 `ENOENT`、`EBADF` 或性能项缺失；只复制入口脚本和统一多调用二进制看似足够。
- **根因**: 多调用二进制仍可能把普通文件名当作 mmap/pagefault 输入，或由极小 wrapper 使用编译期绝对路径回调统一二进制。例如 lmbench 的 `lat_sig ... prot lat_sig` 需要当前目录中的 `lat_sig` 文件，`hello` wrapper 则调用 `/code/lmbench_src/bin/build/lmbench_all`。
- **修复**: 同时审计入口脚本参数、wrapper 文本和统一二进制 strings；把数据文件、exec wrapper 与绝对回调链接纳入最小 payload，并在每个 libc 工作区准备后重建指向当前二进制的链接。复制后逐项校验文件存在，不能只校验主程序。
- **验收**: 不仅检查组退出码，还要比对关键指标行和 stderr；本例要求 `Protection fault` 出现且 `mmap: Bad file descriptor` 消失，同时 fork+exec、文件带宽和 context switch 跑到 END。
- **相关文件**: `user/src/bin/initproc.rs`, `docs/03_fs/2k1000-full-test-disk.md`

### 第三方 benchmark 纳入基线前先证明它真的在做目标工作

- **反例**: 只看到脚本退出 0 并不代表 workload 有效。曾有 pidigits 把多位整数逐项拼接，要求 2000 位却生成约 139 万字符；另一个 scheduler 在第一轮把任务链截断，随后几十万轮只检查一个 suspended 节点。两者都能产生稳定耗时，但测到的不是声明的算法。
- **准入检查**: 每个样本返回可重复的结果 token，算法类负载验证已知值或计数器，I/O/并发类负载验证字节数、子进程状态和线程完成；固定随机种子，避免把输入生成或熵源波动混入目标路径。低于宿主调度/时钟噪声尺度的负载应增加等价迭代，而不是用最小值掩盖波动。
- **采样约束**: import、GC 和临时目录准备放在计时区外；每个 module 使用独立解释器进程，保留全部原始样本并报告 median/min/max/CV。不同 workload 的工作单位不同，不计算无物理意义的绝对耗时几何平均数。
- **介质边界**: 解释器和库可留在只读 tools 分区，benchmark 源码单独按内容哈希部署到 scratch；不要为了几十 KiB 测试脚本整体重写数百 MiB 的工具分区。
- **相关文件**: `user/tools/cpython/bench/bench_runner.py`, `user/tools/cpython/cpython_benchmark.sh`, `scripts/package_cpython_bench.py`, `scripts/deploy_cpython_bench.py`

### 动态程序在 loader 固定地址非法指令时审计跨架构 HWCAP

- **现象**: musl/static 版本正常，glibc 动态程序在进入 `main()` 前稳定触发非法指令；PC 落在 `ld.so` 的 LSX/LASX 等 ISA 优化 resolver，同一测试每次地址一致。
- **根因**: 架构无关 ELF 栈代码写死了另一架构的 `AT_HWCAP` 数值。HWCAP 位号由各架构 ABI 独立定义；RISC-V 的 ISA 字母位图在 LoongArch 下可能被解释为 LASX/LBT。即使硬件实现扩展，内核没有保存对应扩展寄存器状态时也不能向用户态发布该能力。
- **修复**: 由 HAL 按架构生成 HWCAP；读取 CPUCFG/架构寄存器映射硬件能力，再与内核实际启用和上下文保存能力取交集。EUEN/扩展使能与 HWCAP 保持一致。若目标 libc 基线本身已生成 LSX 等指令，仅隐藏 HWCAP 无法解决；必须选择真正的通用 ISA 运行时，或同时实现 trap 与 signal frame 中的完整扩展保存/恢复后再启用。用 loader PC 减加载基址定位具体指令。
- **验收**: 动态程序必须越过 loader，完成真实多线程/多进程上下文切换，并执行一次用户信号 handler 往返；只看到 `main()` 第一行不足以证明扩展状态切换安全。
- **相关文件**: `os/src/mm/address_space.rs`, `os/src/hal/arch/mod.rs`, `os/src/hal/arch/loongarch64/mod.rs`, `os/src/hal/arch/riscv/mod.rs`

### 只读系统盘上的聚焦测试配置使用易失启动标记

- **场景**: 实板系统盘和测试源必须保持只读，但需要反复切换测试组、超时和 LTP 白名单；直接改写系统分区既危险，也会让现场状态难以复现。
- **模式**: 建立默认关闭的诊断 feature，由内核在 ramfs 根目录创建只读语义的空标记。用户态 init 在加载普通磁盘配置后检查标记，只覆盖本次启动的运行计划；诊断镜像使用独立文件名和 Make 目标，正式镜像不带标记。
- **安全边界**: feature 必须编译期限制到目标板和已验证的 scratch 组合；覆盖只能调整测试调度，不能放宽块设备写权限。验收时扫描最终 uImage 确认标记和计划存在，同时确认系统盘配置未写入。
- **相关文件**: `os/Cargo.toml`, `os/src/fs/mod.rs`, `os/Makefile`, `user/src/bin/initproc.rs`
## DWMAC 首包后停止：用 current descriptor 判断布局错误

- **现象**：DWMAC 能收到或发出第一个包，但后续描述符 OWN 不再变化；PHY 和 DMA
  base 均正常。
- **定位**：同时打印软件期望的 next descriptor 与 DMA current descriptor。若硬件
  从 `base` 走到 `base + 0x10`，而软件按 32/64 字节槽位组织 ring，说明 chain 位未被
  硬件识别，优先检查 normal/alternate/enhanced descriptor 模式是否一致。
- **根因示例**：2K1000LA 星云板 U-Boot 定义 `CONFIG_DW_ALTDESCRIPTOR`。alternate
  格式的 RX chain 在 control bit14，TX chain 在 status bit20，TX first/last 也在
  status；套用 normal 格式会造成首包后硬件线性读取槽位 padding。
- **修复**：以正在工作的 bootloader/厂商驱动编译配置为真值，不只复制通用 DWMAC
  寄存器定义；修复后确认 current descriptor 按软件 next 指针跳转并跨 ring 回绕。
- **相关文件**：`os/src/drivers/net/gmac_2k1000.rs`

### 串口输出正常但交互输入无响应时检查主机透传方向

- **现象**：自动启动后能持续看到内核和 Shell 输出，键盘输入在终端上似乎可见，但按回车后设备没有任何响应；改用 `screen` 等串口终端则正常。
- **根因**：主机监视器只实现了 `serial -> stdout`，没有实现 `stdin -> serial`。终端 canonical/echo 设置可能在本地显示输入，使单向转发看起来像设备收到了命令。
- **修复**：使用 `select`/poll 同时监听串口和 stdin；交互期将 TTY 切为 raw 模式，逐字节写入串口，并在 `finally` 中恢复原终端属性。板端常用的 `Ctrl-C` 必须原样透传；本地退出使用带前缀的独立序列（当前为 `Ctrl-] q`），不能占用目标程序的中断字符。
- **验收**：用 pipe/PTY 回环同时验证设备输出、普通输入只转发一次、CR 和 `Ctrl-C` 字节保留、本地退出序列不进入串口、分两次读取的转义序列仍可识别，以及异常退出后终端属性恢复。
- **相关文件**：`scripts/boot_2k1000_tftp.py`

### 中断轮询消费状态事件后延迟跨子系统提交

- **现象**：协议栈的 try_poll 使用 try_lock，看似中断安全；加入 DHCP、热插拔等状态事件后，单核系统可能在事件到达时随机卡死。
- **根因**：中断轮询虽然没有等待协议栈锁，却在事件回调中获取 device_list、路由表或文件系统等阻塞锁。若中断打断了持有这些锁的任务，CPU 会在中断上下文永久自旋。
- **修复**：中断路径只推进硬件/协议状态机，并把最新事件保存在所属对象；任务上下文轮询在释放协议栈锁后再提交跨子系统状态。状态型事件可采用 latest-wins，避免先发布已被后继 Deconfigured 覆盖的旧租约。
- **验收**：分别编译正常任务轮询和 IRQ 轮询调用点；审计 IRQ 路径不再调用会获取其他子系统锁或打印阻塞日志的提交函数。
- **相关文件**：`os/src/net/config.rs`, `os/src/task/manager.rs`

### 多接口 RAW socket 仅外网 ping 出现稳定 DUP

- **现象**：loopback ping 严格一发一收，但网关和公网每个序号都稳定出现第二个 reply；UDP/DNS 正常，GMAC TX/RX ring 也没有重复提交证据。
- **根因**：RAW socket 创建时已在 `lo`、`eth0` 各放置一个 smoltcp handler，发送外网前又把主 `lo` handler rebind 到 `eth0`，导致同一 ICMP reply 被同一 DeviceStack 内两个 RAW handler 各入队一次。重复发生在接收交付层，不是线上真的发送两次。
- **修复**：RAW handler 必须与 ifindex 一起保存；发送时按路由选择目标接口已有的 handler，不执行跨栈迁移。全局等待注册仍按逻辑 socket 计数，但 readiness 必须扫描该 socket 的全部接口 handler。
- **教训**：遇到 ICMP DUP 先用 `127.0.0.1` 与网关做接口对照，再区分 TX 重复、线上回包重复和协议栈多 handler 重复交付；多接口协议对象不能同时采用“每栈预创建”和“发送时迁移主对象”两套策略。
- **相关文件**：`os/src/net/socket/inet/raw/raw.rs`, `os/src/net/socket/mod.rs`, `os/src/net/config.rs`

### 实板外网测试先消除 QEMU 常量和宿主网络假设

- **现象**：局域网 ARP/TCP 正常，但实板外网测试超时；测试硬编码 QEMU 地址、QEMU DNS 或某个公共 IP 时，换到 DHCP、互联网共享、校园网或代理环境后形成批量假失败。
- **根因**：接口地址和 DNS 属于运行时配置，公共 IP/端口也可能被上游网络阻断。宿主机 TUN 代理还可能返回 `198.18.0.0/16` Fake-IP，而互联网共享转发流量并不经过宿主进程的代理接管路径。
- **排查**：先让实板连接宿主网关上的本地 TCP/HTTP 服务，验证驱动、ARP、IP、TCP 和校验和；再让宿主机禁用代理后直连同一公网目标。只有两者都通过后，公网失败才应归因到内核。
- **修复**：通过 `SIOCGIFADDR` 和 `/etc/resolv.conf` 获取运行时参数；公网 HTTP 目标先做 DNS 解析并发送正确 Host，避免把单个裸 IP 当成网络真值。代理 TUN 与 macOS 互联网共享并用时，测试前关闭 TUN/增强模式。
- **相关文件**：`user/src/bin/inet_test.rs`, `os/build_initramfs.sh`, `docs/06_net/test-map.md`

### HTTPS 证书验证先建立可信时间下界

- **现象**：TCP、DNS 和 HTTP 均正常，HTTPS 却报告证书尚未生效或已经过期；使用 `-k` 后请求成功，但无法证明 TLS 验证链可用。
- **根因**：裸机没有持久 RTC，NTP 又可能被局域网或共享网络阻断，内核实时钟停留在零点或过时的硬编码日期。
- **修复**：构建 initramfs 时写入可复现的 `SOURCE_DATE_EPOCH`；启动时优先 NTP，全部失败后只接受经过格式和合理下界校验的构建 epoch。验收同时执行默认 CA 校验的成功请求和错误主机名证书的失败请求，禁止用 `-k` 作为完成标准。
- **教训**：构建时间只能保证证书验证所需的近似时间下界，不能替代长期 RTC/NTP；协议打通也不能替代密码学安全随机源审计。
- **相关文件**：`os/build_initramfs.sh`, `user/src/bin/init.rs`, `scripts/build_curl_runtime_la64.sh`

### 串口短命令正常但长命令变形时降低发送粒度

- **现象**：Shell 的短命令稳定，粘贴或自动注入长命令时字符缺失、顺序错乱，问题在网络轮询繁忙时更容易出现。
- **根因**：主机一次写入多个字节的突发速度超过板端轮询 TTY 的接收/消费能力；本地回显还可能掩盖设备实际没有完整收到命令。
- **修复**：交互监视器对内核阶段输入逐字节发送并加入固定间隔；用超过 100 字符的唯一 marker 检查设备实际回显和执行结果，再继续测试目标子系统。
- **教训**：串口能输出且短输入可用，不代表批量输入链路可靠。自动化测试前应先验证传输层完整性，避免把命令损坏误判为内核或应用故障。
- **相关文件**：`scripts/boot_2k1000_tftp.py`

### 非连续 DRAM 上多次单页分配不构成连续 DMA

- **现象**：扩展第二段 DRAM 后普通页压力正常，但 VirtIO/AHCI/网卡在 bank 边界附近可能访问 MMIO 空洞；或改成只从 fresh 区取连续页后，长期 I/O 在仍有大量 recycled 页时失败。
- **根因**：连续的分配调用顺序不等于连续物理地址，region 切换会跨越空洞；只搜索 fresh extent 又会让释放后的 DMA 页无法复用，形成假性耗尽。
- **修复**：平台显式声明 DRAM region；连续分配在单一 region 内原子取 extent，优先搜索同一 region 的连续 recycled 页，再使用 fresh 尾部。不要求连续的 SysV SHM/VMA 页集合使用独立接口。
- **验收**：跨 bank 的 RamFS 压力必须打印 region 切换并完成内容校验/释放恢复；块设备快照持续读测确认 `share/unshare` 不产生分配 panic。
- **相关文件**：`os/src/mm/frame_allocator.rs`, `os/src/drivers/block/virtio_blk.rs`, `os/src/drivers/block/virtio_blk_pci.rs`

### 固件报告为 DRAM 不代表内核入口即可分配

- **现象**：`bdinfo` 报告完整内存，首尾写探针和短时压力也能通过，但显示控制器、次核或 bootloader 后台状态仍可能引用其中一段，问题表现为屏幕泄露、次核乱跑或非确定性复位。
- **根因**：DRAM 拓扑只回答地址是否有存储介质，不回答所有权是否已经交接。Framebuffer DMA、其他 CPU 的 park loop、BPI/FDT 和 U-Boot 栈/堆都可能位于普通 DRAM。
- **修复**：把容量、地址上界、DRAM region 和固件 carveout 分开建模；入口仅分配已交接区间。关闭设备 DMA、把次核重停放到内核自有代码并复制启动参数后，再分阶段显式回收 carveout。
- **验收**：结合 U-Boot LMB、链接地址、设备寄存器和次核启动代码审计；压力测试必须确认 allocator 的 region 末端停在 carveout 前，而不是只看 `MemTotal`。
- **相关文件**：`os/src/hal/arch/loongarch64/config.rs`, `os/src/hal/arch/riscv/config.rs`, `os/src/mm/frame_allocator.rs`, `os/src/main.rs`

RISC-V/OpenSBI 上若 frame allocator 日志把内核入口之前的低端 DRAM 列为可用，且首次 SATP 切换后回到固件 banner 或内核入口，应优先检查 OpenSBI 区间是否被页表页或批量清零覆盖。QEMU 常见布局为固件 `[0x80000000, 0x80200000)`、S-mode 内核从 `0x80200000` 开始；两者的所有权边界必须显式进入 `FIRMWARE_RESERVED_REGIONS`。

### LoongArch FPR 与 LSX 恢复必须按物理别名二选一

- **现象**：动态运行时在 QEMU 可长期通过，实板却在任意 syscall、定时器中断或调度后随机损坏字符串/向量数据；重复启动和 signal 往返会快速放大问题，但 PC 不一定落在 trap 返回处。
- **根因**：LoongArch 标量 FPR 是 LSX 向量寄存器的低 64-bit lane。若 trap 返回先用 `VLD` 恢复完整 128-bit 向量，再对同一寄存器执行 `FLD.D`，硬件会覆盖低 lane；反向顺序则会让向量快照成为权威状态。QEMU 可能未完整建模该别名，因而形成模拟器假通过。
- **修复**：保存 LSX 时记录完整向量；恢复依据 `EUEN.SXE` 在完整 LSX 与纯标量 FPR 路径中二选一，绝不顺序执行两套恢复。signal frame 同时含标量和向量视图时，先把标量低 lane 合并到向量快照，再执行一次完整 LSX 恢复。
- **验收**：实机连续启动动态运行时至少数十次，同时覆盖定时器抢占、线程切换和用户 signal handler 往返；只跑 QEMU 或只进入一次 `main()` 不足以证明正确。
- **相关文件**：`os/src/hal/arch/loongarch64/trap/trap.S`, `os/src/hal/arch/loongarch64/trap/context.rs`, `os/src/syscall/process/signal.rs`

### PageCache 文件系统要同时实现内核缓冲区与 UserBuffer I/O

- **现象**：普通用户态 `write(2)` 正常，VFS 默认 `symlink()`、内核预装文件或其他内核发起的写入却返回 `ENOSYS`；上层容易把失败误判为文件系统不支持该 inode 类型。
- **根因**：`write_at_user` 只覆盖直接 UserBuffer 快速路径。VFS 内部操作调用的是 `IndexNode::write_at`，其默认实现仍返回 `ENOSYS`；共享 PageCache 并不会自动桥接这两个 trait 入口。
- **修复**：PageCache-backed inode 同时实现 `write_at` 和 `write_at_user`，两者统一校验溢出、配额、目录类型、文件长度与 truncate 的锁序；内核缓冲区路径把旧文件大小传给 PageCache，以正确处理 EOF 外部分页写入。
- **验收**：除普通 read/write 外，至少执行 `symlink + readlink + 透过链接读取 + stat/lstat`，并在两架构 QEMU 中验证，防止只覆盖 syscall 快速路径。
- **相关文件**：`os/src/fs/tmpfs/mod.rs`, `os/src/fs/vfs/index_node.rs`, `os/src/fs/page_cache.rs`

### FAT rename 元数据已切换但新路径读到旧内容

- **现象**：覆盖 rename 后源路径消失，目标目录项首簇、文件大小和 inode 标识都已切到源文件，但重新打开目标仍读到旧目标 payload，或读到该簇上更早的残留内容。一次性测试可能通过，重复复用相同名称和簇后稳定失败。
- **定位**：先同时记录源/目标 payload 前缀、rename 前后首簇、文件大小和旧目标 fd。若命名空间与目录项已经正确而内容恰好等于另一份完整旧 payload，优先审计 inode/PageCache 身份和最终 writeback，不要继续改目录项事务。
- **根因**：同一 FAT 磁盘对象可被构造成多份 inode/PageCache，导致路径切换后命中不同缓存；同时 PageCache 后端若只持有 `Weak<Inode>`，最后一个强引用进入 owner `Drop` 时 `upgrade()` 必然失败，所谓 Drop 内最终写回会失去簇映射并静默丢弃脏页。
- **修复**：文件系统级弱引用表把已分配对象按首簇 canonicalize；无首簇的空文件暂按父目录簇和目录项偏移标识，并在首次分簇、truncate、rename、unlink 时原子重键。PageCache 后端只强持有完成 I/O 所需的最小共享状态（如 `Arc<RwLock<FileContent>>`），不要强持 owner 形成环。覆盖目标从规范表 detach，但其簇延迟到旧 fd 最后关闭后回收。
- **验收**：至少覆盖空源/空目标、同名重复无 `fsync` 压力、源和目标不同长度、目标仍被旧 fd 打开、旧 fd 写入不影响新目标。QEMU 虚拟磁盘通过后，必须在真实介质上重复验证簇复用路径。
- **相关文件**：`os/src/fs/fat32/efs.rs`, `os/src/fs/fat32/fat_inode.rs`, `os/src/fs/page_cache.rs`, `user/tools/cpython/L7_filesystem.py`

### 隔离动态运行时加入 PATH 后仍无法直接执行

- **现象**：运行时文件已经写入并挂载，测试通过显式 loader 可以运行，但 Shell 输入命令仍报告 unknown command；把二进制目录加入 `PATH` 后又可能得到 `ENOENT` 或缺少动态库。
- **根因**：`PATH` 只负责定位文件，不会重写 ELF 的绝对 interpreter，也不会自动设置私有 library path、语言运行时根目录和 CA。只读 tools 分区还可能被绑定到 `/usr`，使启动期临时创建链接失败。
- **修复**：保留运行时隔离布局，提供位于全局 `PATH` 的轻量包装器，由它显式执行私有 loader 并传入库、运行时和证书环境；链接既预置进只读 tools 镜像，也在可写 staged 根文件系统启动时兜底安装。缓存、临时文件和用户包指向独立可写分区。
- **验收**：集成测试必须从普通 Shell 路径直接调用 `/usr/bin/<command>`，不能只验证内部脚本的显式 loader 路径；同时覆盖 tools 绑定到 `/usr` 和保留可写 `/usr` 两种启动模式。
- **相关文件**：`user/tools/cpython/python3-wrapper.sh`, `user/src/bin/initproc.rs`, `scripts/make_2k1000_full_test_disk.py`, `os/Makefile`

### Shell 变量可用不代表子进程环境可见

- **现象**：外层 Shell 能通过私有动态 loader 正常启动解释器，但解释器内部的 `subprocess` 直接执行同一运行时失败；实板返回 `-127`，容易误判为 fork/exec 内核缺陷。
- **根因**：环境初始化脚本只给 Shell 变量赋值，或板上持久 runtime 仍是未导出变量的旧版本。Shell 展开 `$VAR` 不需要 `export`，但 Python `os.environ` 和其后代进程只能看到已导出的变量。
- **修复**：先用 Shell 直接 loader 启动与 Python 内单子进程探针分层排除；需要保持只读 runtime 时，在部署到 scratch 的 benchmark wrapper source 后显式 `export` 必需变量，不覆盖持久分区。
- **教训**：动态运行时的 fork/exec benchmark 必须验证 loader、library path 和 runtime root 穿过 Shell → Python → child 三层环境边界；功能失败样本修正前不能进入性能排名。
- **相关文件**：`user/tools/cpython/cpython_benchmark.sh`, `user/tools/cpython/run_cpython.sh`, `user/tools/cpython/bench/bm_fork.py`

### 包管理器的下载缓存与安装根必须分层

- **现象**：HTTPS 下载和原始包缓存正常，但把包管理器根目录放在 FAT32 后出现 chmod、符号链接、维护脚本或动态加载器异常；直接把系统/工具分区改成可写又会扩大损坏范围。
- **根因**：下载缓存只保存不透明的归档文件，安装根却需要 Unix mode、符号链接、包数据库和事务中间状态。两者的文件系统语义不同，不能因“都需要写入”而放在同一 FAT32 路径。
- **修复**：首先把静态包管理器、仓库配置和公钥嵌入只读 initramfs；安装根放在 RAMFS/TmpFS，已验证的 FAT32 scratch 只承载可删除缓存。用在线 update、fetch、带脚本/trigger 的 add，以及安装根私有 loader 执行动态程序形成闭环；持久化安装另行设计 ext4/overlay 上层。
- **验收**：日志分别确认索引签名、缓存文件、包数据库、安装文件和动态执行；外层超时要按实板网络速度设置并解码 wait status，避免把测试框架的 `SIGKILL` 当成包管理器失败。
- **相关文件**：`os/build_initramfs.sh`, `os/src/fs/mod.rs`, `user/src/bin/initproc.rs`, `docs/08_testing/apk-isolated.md`

### 慢下载先分解代理、通用协议栈与物理网卡

- **现象**：板端 pip/curl 下载很慢，即使显式指向宿主代理也只有数百 KiB/s；只看公网
  请求无法判断是代理节点、DNS/TLS、TCP 实现还是物理网卡。
- **定位**：对同一大文件依次测宿主直连、宿主显式代理、板端到宿主本地 HTTP、QEMU
  到宿主本地 HTTP。下载统一落到 `/dev/null`，局域网加入 `NO_PROXY`，再同步采集宿主
  send queue/重传和内核 poll、TCP RX、网卡 ring/DMA 计数。QEMU 快而实板慢时，通用
  用户态/TCP 路径不是首要嫌疑，应转向板级驱动。
- **代理边界**：HTTP(S) 显式代理通过代理地址和 CONNECT 工作，不要求 TUN/增强模式；
  TUN 影响透明接管、Fake-IP DNS 和宿主转发路径。代理节点慢与板端局域网慢可以同时
  存在，必须分别处理。
- **硬件计数**：DWMAC `DMA_STATUS` 事件位是 W1C 且会黏住。统计窗口结束后只清事件
  位、保留 process state，下一窗口的 RU/TU/overflow 才能证明事件持续发生。小 RX
  ring 的突发容纳时间可按 `descriptors * frame_bits / link_bps` 估算；用默认 ring 与
  扩大 ring 的单变量 A/B 验证，不能同时切换 ACK、poll 策略和代理。
- **已验证实例**：2K1000LA 的 8 RX/4 TX ring 下载 8 MiB 本地文件平均
  `129649 B/s`，每个活跃窗口都有新 `RU`；48 RX/16 TX 平均 `12286495 B/s`，
  提升约 94.77 倍且 `RU` 消失。仅关闭 delayed ACK 的平均值为 `129296 B/s`，可排除
  ACK 策略。放大 ring 后仍有 `TU` 时应作为独立 TX 问题，不得否定已闭环的 RX 根因。
- **教训**：端到端下载速度不是单一指标。先用本地服务去除公网变量，再用 QEMU 去除
  物理网卡变量；所有调优都应有默认关闭的诊断 feature 和可复现实验矩阵。
- **相关文件**：`os/src/drivers/net/gmac_2k1000.rs`, `os/src/net/config.rs`, `os/src/net/socket/inet/stream/mod.rs`, `docs/06_net/debugging.md`

### ext4 压力安装后出现连锁 ENOENT 时从磁盘一致性反推第一处破坏

- **现象**：APK 等事务先成功创建大量文件，随后连续报告“failed to commit ... No such file or directory”；单个 read/write 探针和同一启动中的路径查找可能仍正常。
- **定位**：每轮都从全新 ext4 fixture 开始，把测试 chroot 到真实被测文件系统，关机后立即执行宿主 `e2fsck -fn`。比较第一条 fsck 诊断和最早变化的 bitmap/group counter，不在已经损坏的镜像上反复试错。测试程序使用绝对 `/tmpN` 路径时，只有 chroot 或明确的 mount namespace 才能保证它没有误跑在 ramfs。
- **根因模式**：现代 mkfs 可给后续块组设置 `BLOCK_UNINIT/INODE_UNINIT`；把未初始化位图直接当作磁盘真值会分配元数据块或旧 inode。批处理若每次从挂载时 superblock 副本更新计数，还会覆盖本轮前序变化。
- **修复**：首次分配时按实际 group 边界重建 lazy bitmap，保留 super/GDT、两类 bitmap、inode table 和尾部无效位，清 UNINIT 并重算 checksum；superblock/group descriptor 更新从当前 cache/batch 快照累计。新 inode slot 在发布前清零，重复 free 不重复增加计数。
- **验收**：双架构在 clean fixture 上通过目标 fs test，再运行 basic/libctest/iozone，最后每个镜像都必须离线 fsck clean。只看用户态 exit 0 或内核未 panic 不足以证明 ext4 元数据正确。
- **相关文件**：`os/src/fs/ext4/balloc.rs`, `os/src/fs/ext4/ialloc.rs`, `os/src/fs/ext4/superblock.rs`, `os/src/fs/ext4/ext4fs.rs`

### ext4 目录项删除正确但延迟 flush 仍可覆盖新文件

- **现象**：删除或 rename 后目录短期可读，后续 inode/block 复用或 APK 原子提交时路径成批消失；fsck 可能同时报告目录项类型、checksum、引用计数或块重复占用。
- **根因模式**：块内第一条可变长目录记录没有前驱，错误地把其 `rec_len` 合并给自身会破坏后续扫描；目录 checksum 错用第一条目录项 inode 而非目录 inode；释放的目录/extent 块仍留在 metadata cache 中，旧 dirty entry 会在物理块转交新文件后延迟写回。低层写父 inode 后，VFS 对象中的旧快照也可能在下一次操作覆盖新 size/extent。
- **修复**：第一条记录只清内容并保留 framing，其他记录才并入直接前驱；checksum 显式传入目录 inode/generation；释放块立即 invalidate metadata cache；所有低层目录修改完成后刷新父 inode 快照，并在分配前从真实目录拒绝重复名称。
- **验收**：覆盖首条/非首条删除、重复 symlink、rmdir link count、live inode 延迟回收、释放块复用和同目录 rename；压力结束后离线 fsck。
- **相关文件**：`os/src/fs/ext4/direntry.rs`, `os/src/fs/ext4/ext4fs.rs`, `os/src/fs/ext4/meta_cache.rs`, `os/src/fs/ext4/test.rs`

### 全局 inode 状态表不能用未实现的 st_dev 占位值作身份

- **现象**：ext4 普通文件首次创建和写入成功，关闭后以可写方式 reopen 却返回 `ETXTBSY`；被测文件从未执行，inode 号却恰好等于 initramfs/tmpfs 中的可执行文件。
- **根因**：多个文件系统的 `Metadata.dev_id` 都是占位值 0，全局 busy 表以 `(dev_id, inode_id)` 为键，导致不同文件系统的同号 inode 碰撞。
- **修复**：为 `FileSystem` 提供启动期实例身份，内部状态表使用 `(fs.identity_key(), inode_id)`；MountFS/bind wrapper 必须转发到底层身份。该 key 只解决内核内部对象隔离，不能冒充已经实现了用户态稳定 `st_dev`。
- **验收**：同时保持一个 ramfs 可执行文件 busy，并在 ext4 创建相同 inode 号的文件，验证 writable reopen 不报错；再通过 bind mount 验证同一底层 inode 仍共享 busy 状态。
- **相关文件**：`os/src/fs/vfs/file_system.rs`, `os/src/fs/vfs/mount.rs`, `os/src/task/process.rs`

### 用户栈只按指针宽度对齐会被误判为文件系统数据损坏

- **现象**：同一 ext4 镜像离线内容正确，rv64 比较通过，la64 用户态却稳定把 16 字节文件误报为只读到前 10 字节；内核 read 返回长度和 copy_to_user 内容均正确。
- **根因**：初始 `sp` 只按 8 字节对齐，违反 rv64/la64 的 16 字节入口 ABI。LLVM 基于 ABI 假设折叠地址运算后，生成的指令会利用本应恒为零的低位；错误表现落在普通 libc/Rust 比较代码中，与 VFS 无关。signal handler 入口若只按 context 自然对齐也会重现同类问题。
- **修复**：统一定义 16 字节用户入口对齐；按完整 argc/argv/envp/auxv 表长度动态计算 padding，并让 exec 容量预检复用同一公式；signal frame 同时满足 ABI 和架构 context 的最大对齐。
- **验收**：两架构分别运行真实用户态比较和信号往返，必要时对用户 ELF 反汇编并核对发生误判的地址计算；仅检查内核 copy 日志不能闭环。
- **相关文件**：`os/src/mm/address_space.rs`, `os/src/mm/mod.rs`, `os/src/syscall/process/exec.rs`, `os/src/task/signal/frame.rs`

### 串口逐字节发送仍不足以保证多阶段测试脚本可靠

- **现象**：命令已按单字节和固定间隔发送，但宿主在工作负载尚未结束时继续排队后续 marker/快照命令；板端繁忙输出期间仍会丢字符，结束 marker 变成不存在的命令，计时边界随之失真。
- **根因**：发送节流只控制瞬时字节率，没有提供执行阶段的流控。TTY 回显又包含 marker 字面量，宿主若只搜索字符串，会把“命令已回显”误判成“命令已执行完成”。
- **修复**：测试开始先关闭终端回显；每个控制阶段输出唯一 ACK，宿主读到实际 ACK 后才发送下一阶段；工作负载在同一 shell 行内包住 begin/end/rc，完成后再关闭计数和读取后快照，最后恢复回显。
- **教训**：串口性能 harness 需要同时解决传输完整性、执行流控和 marker 真伪三件事。出现 marker 缺失的样本必须判无效，不能用宿主总 wall time 补成正式结果。
- **相关文件**：`scripts/kernel_perf.py`, `scripts/boot_2k1000_tftp.py`

### 用户态 workload 的高 sys 先检查“被内核模拟的用户指令”

- **现象**：纯字符串、浮点对象或容器操作没有显式 I/O，却有 40%–70% system time；同一程序在 QEMU 正常、在实板极慢。
- **定位**：先用稳定用户态计算作负对照，再只包住 workload body 统计架构异常的次数、访问宽度和 handler ticks；读取实板 CPU capability，不能用 QEMU 的 UAL/扩展能力替代。handler ticks 与 rusage sys 对齐时，异常模拟比 syscall 分布更接近根因。
- **放大链证明**：对非对齐 store 计算 `sum(width * count)`，再与单页 TLB invalidate 比较。两者近似一一对应时，检查逐字节 uaccess 是否对每个 byte 重走 fault-in、private COW 权限恢复和 TLB flush，而不是只优化统计或 syscall dispatch。
- **边界**：Rust handler 计时不含 trap 汇编保存/恢复，因此是下界；启动/import 和 workload body 必须分窗，否则会把 loader/runtime 代码的异常归给算法本身。
- **相关文件**：`os/src/hal/arch/loongarch64/trap/{mod.rs,trap.S}`, `os/src/mm/{uaccess,page_fault,vma}.rs`, `os/src/hal/arch/loongarch64/laflex.rs`

### 释放耗时随页数平方增长时审计逐项删除的数据结构

- **现象**：匿名映射关闭在 1/4/16/32/64 MiB 下从毫秒增长到数秒，而 frame free 与 TLB invalidate 都只随页数线性增长。
- **定位**：让映射中的每页实际 resident，只计时 close/munmap；同时报告 `elapsed/page²`、frame free 和 TLB page。大尺寸 `elapsed/page²` 稳定时，优先查找“外层逐页遍历 + 内层 retain/remove 全表扫描”。
- **已验证模式**：`Vma::unmap` 对每个 resident VPN 调用 `remove_in_memory`，后者对 active vector 执行 `retain`，总工作量为 N+(N-1)+...+1。该模式会放大 Python arena、大 buffer 和进程退出，但在没有完整 workload 分窗前不能声称其占据每项 benchmark 的具体比例。
- **相关文件**：`os/src/mm/vma.rs`, `os/src/mm/frame_store.rs`, `user/tools/cpython/bench/diag_mmap_release.py`

### 宿主串口超时不等于板端 workload 已终止

- **现象**：宿主 capture 超时并返回 124，但板端前台解压、递归遍历或 I/O 仍在继续；立即发送审计命令会被当成前台程序输入，后续“恢复超时”只是同一现场的连锁结果。
- **判定**：超时后先把样本标为控制面未知，不把宿主 wall time当作 workload 失败耗时。只有读到原命令的工作结束 marker/rc、明确的 shell prompt，或完成物理复位后，才允许发送下一条测试命令。
- **恢复**：短时尝试 Ctrl-C/换行和唯一 echo marker；若目标内核/驱动路径不可中断且没有 prompt，停止注入更多命令，保留原始串口并请求物理复位。复位后先审计 staging/canonical 路径和目标分区哈希，再决定清理或继续。
- **报告**：把下载、解包、串口恢复等 pre-benchmark 失败与 benchmark failure 分表；前者可以暴露部署/VFS 性能问题，但不能降低已成功正式矩阵的 pass 数。
- **相关文件**：`scripts/kernel_perf.py`, `scripts/deploy_cpython_runtime.py`

### ext4 `statfs` 空间值不随分配变化时不要继续相信 `df`

- **现象**：创建或删除数百 MiB 文件/目录后，`statvfs` 的 `f_bfree/f_bavail` 完全不变，
  但分配器已经报告 ENOSPC；清理旧对象后同一部署又能成功。
- **定位**：先做已知大小分配前后的 statfs 对照，再审计文件系统 `super_block()` 是否读取
  挂载时结构副本，而分配/释放路径是否更新了另一个 metadata cache/current superblock。
  “返回值稳定”不是容量稳定，尤其不能用它决定删除 canonical release。
- **处理**：在 statfs 修复前，把 free block 视为低可信诊断；发布器采用 staging、失败清理、
  current 保护和按 hash 审计旧 release，先删已确认非 current 的历史版本。压缩传输对象可
  放 tmpfs 减少目标 ext4 峰值，但解压树、状态和最终测试仍必须落在目标文件系统。
- **验收**：修复后需在实际 ext4 上做分配、sync、statfs、删除、sync、statfs 的单变量测试，
  并核对 current superblock/bitmap；仅让部署成功不能证明统计接口正确。
- **相关文件**：`os/src/fs/ext4/ext4fs.rs`, `scripts/deploy_cpython_runtime.py`

### CLI help 通过不代表可选后端依赖闭包完整

- **现象**：Python 应用的 console command、`--help`、基础 import 都通过，只有创建某个
  LLM/数据库/图像等可选后端时才报“请安装 extra”；用户明明已经能在 traceback 中看到
  extra 的顶层包，最外层错误仍声称它未安装。
- **根因**：`--help` 不执行后端工厂；后端又常用 `try: import top_level` 包住整个导入
  链，任何传递依赖失败都会被统一改写成安装 extra 的提示。只读最外层异常会把
  `top_level -> transitive_dependency` 的失败误判成顶层包缺失。
- **定位**：从 traceback 最内层 `ModuleNotFoundError` 开始；在目标环境执行
  `importlib.metadata.distribution(...).requires`、`pip show` 和 `pip check`，按现场精确版本
  解依赖，不用最新版文档替代旧版 metadata。原生/纯 Python 边界以 wheel tag、
  `Root-Is-Purelib`、安装树 `.so` 和实板 `module.__file__` 四项共同确认。
- **验收**：使用 dummy key 和不可达的本地 base URL实际创建后端 client，确保完整 import
  和构造路径已执行但不发送公网请求；再用本地固定响应端点覆盖序列化/反序列化。真实
  API 只作最后的端到端体验测试，并与本地结果分开计时。
- **安装边界**：若 `pip --target` 从 tmpfs 跨设备搬到 ext4 时先 `EXDEV`、后在
  `shutil.copytree` 的 metadata 操作收到 `ENOSYS`，即使日志先打印 `Successfully
  installed` 也要判失败，不能复用部分目标树。已校验 universal wheel可在目标 ext4
  同盘 staging 解包、功能验证后 rename 发布；缺失的 FS syscall 仍需作为独立根因记录。
- **相关文件**：`scripts/board/verify_persist_python.sh`,
  `docs/09_debug/la64_on_board/260717/09-aligned-pillow-and-smolagent-closure.md`
