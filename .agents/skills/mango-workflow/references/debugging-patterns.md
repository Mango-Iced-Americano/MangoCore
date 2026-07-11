# 调试模式库

> 跨对话可复用的调试技巧和排查方法。

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
- **根因**: la64 kernel stack 改为固定 VM slot 后，`KERNEL_STACK_MAX_SLOTS` 是硬容量边界；压力用例留下大量任务时，stack slot 分配可能先于普通 clone 失败路径触发边界 panic。
- **修复**: 后续应让 kernel stack 分配成为 fallible，并在 clone 中把 slot 耗尽转成 `EAGAIN/ENOMEM`；同时复核 task quota 与 stack slot 容量是否完全一致，以及超时 waiter 是否及时回收。
- **教训**: 全量回归要区分三类问题：guard 命中的真实栈溢出、BTreeMap/heap 这类随机破坏、slot/quota 这类确定性容量上限。看到 slot panic 时优先检查 quota、allocator 和压力用例残留任务，而不是继续调大单个栈大小。
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
- **修复**: futex wait 在 deadline 前预留 guard 窗口，尾部仍保持在 futex wait queue 中自旋，期间继续检查 futex word、信号和是否被 `FUTEX_WAKE` 移出队列；la64 对相对 `FUTEX_WAIT` 的中短 timeout 额外补偿固定出口尾差。
- **教训**: futex timeout 精度不能直接套用 sleep 的“先出队再自旋”，否则可能丢掉 deadline 前的真实 wake；尾部自旋必须保持 wait queue 可观察或显式处理被 wake 移除的状态。
- **相关文件**: `os/src/task/threads.rs`

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
- **方法**: 按固定的 512B sector 整数倍切块，块大小必须落在已验证的空闲 DRAM 区间；逐块执行 TFTP、内存 CRC、`scsi write`、同 LBA `scsi read` 和读回 CRC，后一块起始 LBA 累加前一块 sector 数。写盘前硬匹配 `scsi info` 的型号与容量，镜像 sector 总数还必须小于设备容量。
- **验收**: 所有块读回 CRC 一致后重新 `scsi reset`，检查 DOS/MBR 分区长度，再分别用 `ext4ls`/`fatls` 读取每个分区；最后启动目标内核验证设备节点、文件系统类型和实际挂载点。任何短传、短写、CRC 不一致或目标型号变化都立即停止。
- **实测参数**: 2K1000LA 的 6,443,499,520B 镜像使用 24 个 256MiB 块加 1 个 1MiB 块；256MiB 对应 `0x80000` sectors，加载地址 `0x9000000098000000`，目标为 `TS32GMTS400`。
- **相关文档**: `docs/03_fs/2k1000-full-test-disk.md`

### 板型 feature 不能兼任 bring-up 日志开关

- **现象**: 首次上板时加入的 CPUCFG、地址布局、内核栈读写、首任务切换和用户态返回探针长期绑定在 `board_xxx` 上，导致后续每个正式板级镜像都携带大量串口输出和一次性探针开销。
- **根因**: 硬件选择与诊断策略使用了同一个 feature；“运行在实板上”被错误等同于“始终处于 bring-up 阶段”。仅把日志级别设为 off 无效，因为 `println!` 和主动探针不经过 `log` facade。
- **修复**: 板型 feature 只负责链接地址、入口和驱动选择；另设默认关闭的诊断 feature。成功型早期输出使用统一编译期宏，带额外读写或原子状态的探针则整段 `cfg` 移除；panic 和真实错误路径不应静默。
- **验收**: 除双架构编译外，必须直接扫描最终 uImage/ELF，确认调试字符串不存在，同时确认正式配置与错误诊断字符串仍在；不要只搜索源码或依赖 `LOG=off`。
- **相关文件**: `os/Cargo.toml`, `os/src/console.rs`, `os/src/main.rs`, `os/src/hal/arch/loongarch64/`, `os/Makefile`

### U-Boot 串口自动化必须由 prompt 和内容校验驱动

- **现象**: 把多条 `setenv/tftpboot/bootm` 用固定 sleep 或一次性串口注入时，U-Boot 可能仍在网卡协商、TFTP 或 CRC，后续字符被丢弃；最终表现为偶发找不到镜像、命令截断或在未校验镜像时直接启动。
- **根因**: U-Boot 各命令耗时不固定，串口发送成功不代表命令执行完成；只检查 TFTP 返回也无法发现短传、错误文件或内存内容损坏。
- **修复**: 每条控制命令都读取到完整 `=>` prompt 后再发送下一条；TFTP 后同时校验 `Bytes transferred`、本地与 U-Boot CRC32，再用 `iminfo` 确认架构和镜像 checksum。`bootm` 之后切换为纯串口透传，主机侧 Ctrl-C 只关闭监视器。
- **安全边界**: 网络参数只用 `setenv`，禁止自动 `saveenv`；普通启动脚本禁止包含块设备写命令。自动接管串口时只关闭能明确匹配同一设备路径的 screen，未知占用者必须报错停止。
- **相关文件**: `scripts/boot_2k1000_tftp.py`, `Makefile`

### AHCI HBA reset 后不能假定 PI 保持不变

- **现象**: 同一控制器和 SSD 在 U-Boot 执行过 `scsi scan` 后可用，但直接 TFTP/`bootm` 时内核报 `NoUsablePort { implemented: 0, ... }`；PCI ID、class 和 ABAR 都已验证正确。
- **根因**: 部分片上 AHCI 控制器的 HBA reset 会清空可写的 `HOST_PORTS_IMPL`。若 bootloader 先扫描磁盘，它可能已经回写 PI，从而掩盖内核初始化缺失并形成隐式启动顺序依赖。
- **修复**: 对照厂商 U-Boot/Linux：reset 前保存实现寄存器，或由板级 Provider 提供固件定义的端口图；reset 后回写 PI 并读回刷新 posted MMIO write，再做端口和链路探测。未知平台不能使用无条件固定掩码。
- **教训**: “同一镜像偶尔能识别 SSD”要检查 bootloader 前置命令是否改变了控制器状态。内核驱动必须从其声明的硬件初始条件独立建立完整状态，不能依赖人工调试命令的副作用。
- **相关文件**: `dependency/dep_iso/src/provider.rs`, `dependency/dep_iso/src/block/ahci.rs`, `os/src/drivers/block/sata_blk.rs`
