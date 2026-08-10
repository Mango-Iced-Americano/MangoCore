# 经验模式库

> 跨对话可复用的 bug 根因 → 修复模式。按子系统分类。

## 渐进性能退化调试方法论

### 问题特征

性能退化（performance drift / progressive degradation）不同于普通 bug——没有明确的"崩溃点"，而是在长时间运行中某个操作逐渐变慢。典型信号：
- lmbench score 随窗口单调递减
- `getppid_avg_ticks` 逐轮增长
- open/close / fork+exit 等在创建+销毁路径上的操作越来越慢
- 但纯读取操作（null syscall、read、write、pipe、signal handler）不受影响

### 第一步：建立可重复测量框架（最关键）

没有可重复的测量，后面所有分析都是猜。

```
构建 drift_window 模式：
  for w in 0..N:
    reset 性能计数器
    run workload (触发退化的操作)
    pre_snapshot (读取所有 /sys/kernel/stats/*)
    run 被测操作 (lat_syscall null / full lmbench)
    post_snapshot (再次读取所有 /sys/kernel/stats/*)
    sleep(100ms)
  delta = post - pre  (每窗口的增量)
```

关键设计决策：
- **逐窗口 reset 计数器**：增量直接测量当前窗口的代价，避免累计误差
- **pre/post snapshot**：每次快照读取所有计数器到同一行输出，方便分析脚本解析
- **分析脚本**：`analyze_drift.py` 自动解析 serial 输出 → 计算 delta → 派生指标（getppid_avg_ticks、fast_path_ratio、tlb_per_getppid）→ 异常检测 → CSV+Markdown 报告
- **决策树**：不同异常触发不同建议（如 tlb_anomaly > 0 建议加 TLB flush callsite tag）

### 第二步：隔离退化所在层

渐进退化最难的是要找到"什么在变慢"。分层隔离法：

```
Layer 0: null syscall (getppid)     → 测"纯读取"路径，应永远不变
Layer 1: lat_syscall null            → 测 syscall 路径，应稳定  
Layer 2: simple read/write           → 测文件 I/O，可能退化
Layer 3: simple open/close           → 测 VFS + 对象创建，最可能退化
Layer 4: fork+exit                   → 测进程创建，可能退化
Layer 5: full lmbench                →  composite score
```

本案例的关键发现：**null syscall (getppid) 对几乎所有系统状态变化免疫**，因为它只读一个 `current.inner.parent_pid` 字段。当所有人都以为退化在 null syscall 时，实际上它在文件操作和进程创建——这一发现是通过切换测量目标（从 `lat_syscall null` 到 `full lmbench`）才获得的。

**教训**：永远不要只盯着表面指标（null syscall），要把测量范围扩大到所有可能退化的路径。

### 第三步：计数器驱动的迭代式精确定位

这是最核心的模式。每轮只做三件事：
1. **加计数器**（最多 15 个/budget，避免过度插桩扰动性能）
2. **跑一轮测试**并收集数据
3. **看数据决定下一轮方向**（不猜，让数据说话）

```
Round 0: 只有 P0 计数器（taskq/timer/syscall/buddy）
  → 发现 TLB flush 异常（32K/window，null syscall 不应有任何 TLB flush）

Round 1: 加 P1 计数器（ctxsw/reclaim/tlb/heap + syscall cost）
  → 确认 null syscall 稳定（36-39μs），但 full lmbench 中 open/close 退化 2.6x

Round 2: 加 seccomp + timer IRQ cost + timer pop cost
  → 排除 seccomp（SECCOMP_CHECK_CALLS=0），排除 timer queue bloat

Round 3: 加 heap allocator 计数器（alloc/dealloc ticks + dealloc scan_steps）
  → **决定性突破**：scan_steps/dealloc 从 19→114（6x），dealloc ticks 从 10.8K→69.9K（6.5x）
  → 根因确认为 buddy allocator dealloc() 中 `for block in free_list.iter_mut()` 线性扫描
```

**每轮必问的问题**：
- 数据趋势是什么？（单调增长？稳定？下降？）
- 哪个计数器和 lmbench 分数的变化趋势最吻合？
- 如果数据不够区分，下一轮加什么计数器？

### 第四步：bitmap guard 模式（可复用修复技术）

当退化根因是**有状态数据结构的 O(n) 操作**时，bitmap guard 是通用修复：

```
before:                       after:
  for item in list.iter_mut():   if !bitmap_test(key): break
    if item.matches(key):          // O(1) 跳过 95%+ 的无效扫描
      ...                          for item in list.iter_mut():
                                     if item.matches(key):
                                       ...
```

适用条件：
- 有状态数据结构（free-list、hash table、LRU list）导致 O(n) 操作
- 存储了全量成员信息但不支持 O(1) 查找
- bitmap/cache 内存开销可接受

本案例：buddy allocator 的 free-list 是 intrusive 单向链表，查找 buddy 需要遍历整个链表。加 per-class free-membership bitmap 后 O(1) 跳过无效扫描，scan steps 减少 130 倍。

### 工作流总结

```
[问题信号] → [建测量框架] → [隔离退化层] → [加计数器] → [跑实验] → 
[看数据决策] → {数据够了? → [诊断根因] → [修根因] → [补安全] → [跑全量] → 完成
              数据不够? → [加计数器] → [跑实验] → ...}
```

关键文件配置：
```toml
# os_test.conf
mode=drift_window
drift_windows=6
drift_pre_mask=0x003  # basic + busybox pre-workload
drift_measure=full    # 全量 lmbench 作为测量目标
diag=1
```

相关文件：
- `user/src/bin/initproc.rs` — drift_window 循环 + pre_mask + measure
- `os/src/task/perf.rs` — 所有 AtomicUsize 计数器 + record 函数
- `scripts/analyze_drift.py` — 自动分析脚本
- `docs/09_debug/buddy-allocator-scan-drift.md` — 本案例完整报告

### 关键原则

1. **永远先建基础设施**：没有可重复的测量框架，不可能找到渐进退化。
2. **null syscall 是最好的隔离指标**：它不动任何 kernel 对象，完美区分"创建"和"读取"两类操作。
3. **渐进退化根因不在最明显的地方**：所有人都以为在 null syscall，实际在 heap dealloc。
4. **不要依赖单一指标**：lmbench composite score + 每个子项 + 内核 P0/P1 计数器一起看。
5. **计数器增减要与 lmbench 分数变化吻合**：如果 scan_steps 涨了但 lmbench 没变，说明不是根因。

## 网络绩效

### QEMU TCG 对热路径 struct 大小极度敏感（P1.1 教训）

- **根因**: 在 `NetInterfaceInner`（全局 `spin::Mutex` 包裹的 struct）上加 24 字节的 `route_slots: Vec<...>`，struct 从 56→80 字节。功能等价（fast path 未接入），但 netperf -5% regression。
- **修复**: 放弃全局 mirror 方案，改为在 `TcpSocket`（每连接独立）上缓存 3 个 Atomic 字段（24 字节/连接），不动全局 struct。
- **教训**: 单核 QEMU TCG 对 `spin::Mutex<T>` 中的 T 大小非常敏感。大结构改变 inline layout → 影响锁操作的内存足迹 → 宏观性能回退。永远不要扩大全局热路径 struct。
- **相关文件**: `os/src/net/config.rs` (NetInterfaceInner), `os/src/net/socket/inet/stream/mod.rs`

### RV64 热路径位图应避免字节原子 RMW

- **根因**: RV64 缺少原生 byte AMO；`AtomicU8::fetch_or` 在 QEMU TCG 中会退化为掩码 LR/SC。对只增不减的页 valid-mask，每次 1KiB pwrite 都执行该 RMW 会累积为明显开销。
- **修复**: 将独立的热路径状态位图升级为 `AtomicU32`，仍只使用低位掩码；先用 Relaxed load 检查目标位是否已全置位，若是直接返回，只有可能改变状态时才执行 word `fetch_or`。升级相邻字段时用 `size_of` 断言锁定热对象布局。
- **教训**: 优化 QEMU TCG 下的高频原子位图时，先确认目标架构的原子指令宽度；无论数据逻辑只需一个字节，也不应让实现回退为字节 LR/SC。快路径仅可用于单调状态（只置位、从不清位），否则必须保留 RMW/CAS。
- **相关文件**: `os/src/fs/page_cache.rs`

### 单核无抢占环境 lock splitting 无并发收益（perf/net 教训）

- **根因**: perf/net 分支引入 per-stack locks + WaitQueue 重构 + cooperative poll → -50% netperf RR。单核环境下没有真正的并发，锁拆分的额外 atomic 操作是纯 overhead。
- **修复**: 放弃 per-stack locking，改为减少锁次数（P0 skip poll、P1 绕 inner 锁）。
- **教训**: 单核优化方向是**减少原子操作和锁次数**，不是**拆锁增加并发度**。

### iperf 与 netperf 必须双测

- **根因**: netperf RR 测小包往返延迟（syscall 开销主导），iperf 测 TCP 吞吐（buffer/window/poll 频率主导）。P0+P1+P3 对 netperf 提升 +10%，但对 iperf 提升仅 +74%。E（智能保 flag）+C（64K buffer）对 netperf 几乎中性，但 iperf 提升 6.7x + 4.6x。
- **教训**: 每次网络栈修改后必须同时跑 netperf（mask=0x200）和 iperf（mask=0x040），禁止只看一个。

## 路径解析

### `parse_path()` 滤除 "." 组件，"." 检测必须用原始路径字符串

- **根因**: `parse_path()` 在 `os/src/fs/mod.rs` 将 `"."` 组件（`"" | "." => {}`）忽略，导致 `vfs_lookup_parent_for_start` 等函数无法感知路径最后组件是否为 `.`
- **修复**: 在调用 `vfs_lookup_parent_for_start` 前，用原始 `path` 字符串检测最后组件是否为 `.`：
  ```rust
  let trimmed = path.trim_end_matches('/');
  if trimmed == "." || trimmed.ends_with("/.") {
      return EINVAL;
  }
  ```
- **教训**: 任何需要对 `.` 进行语义判断的地方（EINVAL for rmdir/unlink、禁止 "." 作为文件名等），都不能依赖 `parse_path()` 的输出，必须在原始路径字符串上做检查。
- **相关文件**: `os/src/fs/mod.rs` (parse_path), `os/src/syscall/fs.rs` (sys_unlinkat)

## 测试 Harness / LTP

### 只读累计 sysfs 计数器的逐用例 wrapping 差分

- **场景**：内核只暴露累计只读计数器，外部既没有安全 reset 接口，也不能为了诊断改变测试全局状态。
- **模式**：用户态 runner 先以 feature 文件 gate 诊断；每个实际执行 case 紧邻执行前后各读取一次同一 stats 文件，严格解析已知字段，计算 `post.wrapping_sub(pre)`。
- **降级**：配置缺失或为 false 时完全不 probe、不输出；feature、读取或严格解析失败只输出一次稳定的 unavailable 原因，绝不影响 case 顺序、执行、计分或退出码。
- **ABI**：`user_lib::open()` 直接把 Rust `&str` 指针交给路径 syscall，固定诊断 sysfs 常量必须恰有一个尾随 NUL。
- **输出**：每个成功执行且有完整 pre/post snapshot 的 case 后，在现有 LTP/QEMU log 输出一条有界的数值序号、退出码和 24 个 counter delta；用既有 `#<index> ... case=<name>` 行关联 case 名，诊断不保存名称、不创建 report 或其他文件。
- **调试层级**：先用这类可开关、有界的现有 QEMU log 验证单一核心假设；只有需要跨运行留存、自动聚合或对外审计时，才增加 report 落盘、镜像提取和元数据归档。不要先为一次性根因定位构建持久化证据管线。
- **相关文件**：`user/src/bin/ltprunner/lwext4_perf/`、`docs/ltp/lwext4_perf_diagnostic.md`

### 长测第二轮 PID 超过 `/proc/sys/kernel/pid_max`
- **根因**: 用户可见 PID/TID 只线性增长，释放时只为 `ns_last_pid` 打标记而不进入可复用池。全量 LTP/bench 多轮创建进程后，`getpid01` 会观察到 PID 超过内核暴露的 `/proc/sys/kernel/pid_max=32768`。
- **修复**: 参考 Linux `idr_alloc_cyclic/free_pid` 和 DragonOS PID namespace 分配/释放模型：释放后记录可复用 ID，普通路径仍保持线性分配；接近 `pid_max` 高水位后再复用已释放 ID，并跳过低位保留 PID。
- **教训**: 长测日志里如果第一轮通过、第二轮 `getpid01` 或 PID 边界类用例失败，应先检查 PID allocator 的 wrap/reuse 语义，而不是调高 `/proc/sys/kernel/pid_max` 规避。
- **相关文件**: `os/src/task/pid.rs`

### LTP 账号数据库已有文件要幂等补齐
- **根因**: LTP 用例可能依赖 `/etc/passwd`、`/etc/group` 里的具体账号/组存在；如果 init 逻辑只在文件不存在时写默认内容，持久测试镜像里的旧文件不会被更新。`setfsgid03` 会从 gid 1 起调用 `getgrgid()` 查找一个存在的普通组，缺少 gid 1 组时会长时间遍历并最终被 runner 超时杀掉，表现为 137。
- **修复**: 对新增账号数据库前置条件使用幂等迁移：文件不存在时创建默认内容，文件已存在时只补缺失条目，避免覆盖镜像中已有账号配置。
- **教训**: 遇到 LTP 用例启动后没有内核断言、只在 libc/账号查询阶段超时，应先检查镜像内 `/etc/passwd`、`/etc/group`、`nsswitch.conf` 的实际内容，而不是直接把用例加入排除表。
- **相关文件**: `user/src/bin/initproc.rs`

### LTP suite/inline 结果标签不能混用
- **根因**: suite runner 通过 `/ltprunner` 逐用例等待子进程，返回码可用于 `PASS/FAIL` 标签；inline runner 直接跑 LTP 二进制时，部分用例即使 summary 有 `failed > 0` 也可能返回 0，仅凭退出码会把真实 TFAIL 误标成 PASS。
- **修复**: suite 路径按 `run_case()` 返回码输出 `PASS LTP CASE`/`FAIL LTP CASE`；inline 路径的过滤项输出 `SKIP LTP CASE`，成功退出只输出中性 `DONE LTP CASE`，真实非零退出才输出 `FAIL LTP CASE`。
- **相关文件**: `user/src/bin/ltprunner.rs`、`user/src/bin/initproc.rs`

### LTP 内部 timeout 与 runner timeout 不一致
- **根因**: suite runner 外层按架构设置 case timeout，但 LTP 二进制内部还有自己的默认 timeout。la64+heap_trace+批量窗口下，单个较慢 case 可能还没触发外层超时，就先被 LTP 内部 30s watchdog 打印 `Test timeouted, sending SIGKILL!` 并返回 `TBROK`。
- **修复**: 只在确实需要更长预算的架构上传 `LTP_TIMEOUT_MUL=2`，让 LTP 内部 timeout 与 runner 外层 `DEFAULT_CASE_TIMEOUT_SECS=60` 对齐；不要在默认 30s 的架构上传 `LTP_TIMEOUT_MUL=1`，避免不同 libc 对环境解析出现额外噪声。
- **相关文件**: `user/src/bin/ltprunner.rs`

### LTP TCONF 不能计为 FAIL
- **根因**: LTP 新 API 用退出码 32 表示 `TCONF`，即测试因 libc/架构/配置前置条件不满足而跳过。suite runner 若把所有非零返回码都记为 `FAIL LTP CASE`，会把 musl 缺少 `getcontext/sethostid` 这类库能力误算成内核失败。
- **修复**: `run_case()` 返回 32 时输出 `SKIP LTP CASE` 并递增 skipped，其他非零才算 failed；输出标签也必须按返回码分支打印，不能只修计数但仍无条件打印 `FAIL LTP CASE`。
- **教训**: 扩大 LTP include 时先区分 `TFAIL/TBROK` 与 `TCONF`，否则会把可接受的环境跳过项混入内核适配清单。
- **相关文件**: `user/src/bin/ltprunner.rs`

### LTP runner 外层 case timeout 要能覆盖用例自身 timeout

- **根因**: LTP 二进制内部会根据 `LTP_TIMEOUT_MUL` 放大自己的 timeout，例如 fuzzy-sync 用例 `timerfd_settime02` 会把内部 timeout 提到 3m30s；suite runner 如果仍固定 60s 外层 case timeout，会在内核语义还未失败前先杀掉测试，表现为 `TBROK Test killed`。
- **修复**: 为 suite runner 增加显式 `ltp_case_timeout_secs` 配置，focused 调试长耗时用例时让外层 timeout 大于 LTP 内部 timeout；常规配置保持默认 60s，避免全量回归被单个异常 case 拖长。
- **教训**: 遇到 LTP 提示 “try exporting LTP_TIMEOUT_MUL” 或日志显示内部 timeout 已放大时，先比较 harness 外层 timeout 和 LTP 内部 timeout，再判断是否为内核卡死。
- **相关文件**: `user/src/bin/ltprunner.rs`

### LTP libc wrapper 先于 syscall 拒绝参数
- **根因**: musl/glibc 的高层 libc wrapper 可能在进入内核前先做私有 ABI 校验；例如 musl 会把 34 号实时信号视为内部保留信号，`signal()/sighold()/sigrelse()` 直接返回 `EINVAL`，即使内核 `rt_sigaction/rt_sigprocmask` 已兼容该信号。LTP 此时表现为 libc API `TBROK`，trace 中看不到对应 syscall。
- **修复**: 对确认属于 libc wrapper 差异、且内核 ABI 已正确的普通 LTP 二进制，在 `ltp_proto_compat.so` 中补窄范围 preload wrapper，直接调用 raw syscall，并同步重新生成 rv64/la64 两份 `.so` 供内核 `.incbin` 嵌入。
- **教训**: 修这类问题前先确认失败是否发生在 syscall 之前；改完 `.so` 后必须重建内核镜像，否则 QEMU 仍会运行旧的嵌入版 preload。
- **相关文件**: `os/ltp_proto_compat.c`, `user/tools/riscv64/lib/ltp_proto_compat-rv.so`, `user/tools/loongarch64/lib/ltp_proto_compat-la.so`

### initramfs 新构建产物不能被测试盘旧二进制遮蔽
- **根因**: stage-1 init 如果优先执行 `/sdcard/initproc`，下载的测试镜像里可能保留旧 runner；即使 `make rv64-only/la64-only` 已重建 initramfs 和内嵌 `/initproc`，QEMU 仍会跑测试盘旧二进制，表现为新增配置项或 smoke 分支完全不生效。
- **修复**: initramfs 模式优先 `exec /initproc`，仅在缺失时 fallback 到 `/sdcard/initproc`；定位时可用输出字符串或二进制大小变化确认实际运行的是哪份应用。
- **教训**: 修改 `user/src/bin/initproc.rs` 后，如果 QEMU 日志仍是旧格式，先检查 stage-1 exec 顺序和镜像内同名二进制，而不是继续怀疑配置注入。
- **相关文件**: `user/src/bin/init.rs`, `user/src/bin/initproc.rs`

## 信号/进程

### 事件型 fd 定时器必须接入统一 deadline queue
- **根因**: timerfd 这类 fd 状态机如果只在周期性 timer interrupt 中扫描 registry，而 `timerfd_settime()` 不向统一 high-res timer queue 注册下一次 deadline，就会把短 timeout 退化为调度 tick 粒度；更严重时，如果扫描路径没有把“唤醒了等待者”反馈给调度器，阻塞读者可能延迟到后续调度点才运行。
- **修复**: arm/disarm timerfd 后扫描 registry 找到最早 deadline，注册一个全局 sweep timer action；用 generation 让旧 sweep 自动失效；sweep 过期后唤醒所有已到期 timerfd，返回实际 wake 数并重新计算下一次 sweep。
- **教训**: 任何“fd 可读状态由时间推进触发”的对象都不能只靠周期性兜底扫描；状态更新点必须驱动统一 deadline queue，wake 路径必须把是否唤醒任务反馈给调度器。
- **相关文件**: `os/src/fs/timerfd.rs`, `os/src/task/manager.rs`

### futex wake 与信号唤醒竞态
- **根因**: waiter 被信号或 timeout 先置为 Ready 后，仍可能暂时留在 futex waitqueue 中；随后 `FUTEX_WAKE` 移除该 waiter 时如果因状态非 Interruptible 不计数，用户态 checkpoint 会认为没有唤醒到等待者并重试到超时，而 waiter 本身又会把 waitqueue 条目被移除解释成正常 wake。
- **修复**: `WaitQueue::wake_at_most()` 对 Ready-but-still-queued waiter 也按一次成功 wake 计数；timer signal 入队时按 sigmask/sigwait mask 判断是否需要唤醒 interruptible task，减少被屏蔽信号造成的伪唤醒。
- **教训**: waitqueue 的“移出队列”和调度状态不是同一个原子状态；对 futex/checkpoint 这类计数语义，移出等待队列本身就应消耗一次 wake。
- **相关文件**: `os/src/task/manager.rs`, `os/src/task/signal/mod.rs`

### pselect/ppoll 空 fd 短 timeout 过冲
- **根因**: 无请求 fd 的纯 timeout 等待复用通用 waitqueue 睡眠会经过调度器和 timer wake；heap_trace/QEMU 下 25ms 级短睡眠可能多出数百微秒，超过 LTP `pselect01_64` 的严格阈值。
- **修复**: 对 `nfds=0`/无请求 fd 且 deadline 不超过 50ms、当前没有其他 ready task 的纯 timeout 路径，用硬件 tick 短忙等；一旦发现有其他 ready task，退回原 waitqueue 睡眠路径。
- **教训**: 微秒级计时 LTP 用例要区分“有 fd/事件等待”的阻塞语义和“纯 timeout sleep”的精度语义；短忙等必须有时长上限和 ready task 逃逸条件，避免拖慢并发场景。
- **相关文件**: `os/src/fs/poll.rs`

### 被屏蔽信号导致错误的 EINTR
- **根因**: 信号检查用了 `is_empty()` 而非 `sigpending.difference(sigmask)`，忽略了信号掩码
- **修复**: 必须用 `difference(sigmask)` 过滤被屏蔽信号
- **相关文件**: `os/src/task/signal/mod.rs`

### SA_RESETHAND 清掉 SA_SIGINFO
- **根因**: 信号投递后直接删除 action，handler 内 `sigaction(..., oldact)` 读到空 flags
- **修复**: `SA_RESETHAND` 只重置 handler 为 `SIG_DFL`，保留 flags/mask/restorer 供 oldact 查询

## 内存管理

### SysV SHM 每次 shmat 分配独立匿名页
- **根因**: `shmat()` 若直接复用匿名 `MAP_SHARED` 并让每个 VMA 自行分配物理页，同一 `shmid` 的多次 attach 会得到不同 backing，写入内容无法互通；fork 继承只能共享已有 VMA，不能修复独立 attach 的共享语义。
- **修复**: 在 SysV SHM segment 级别维护 backing frames，`shmat()` 为新 VMA 映射同一组 frames；只把地址选择和 VMA 插入复用到 SHM 专用 mmap 路径，不改变普通匿名 mmap 行为。
- **教训**: LTP `shmt03/shmt04/shmt06` 这类用例要同时验证“同进程多 attach”和“fork 后读写同一 segment”；通过 fork 共享不代表独立 attach 已符合 SysV SHM 语义。
- **相关文件**: `os/src/syscall/process/ipc.rs`, `os/src/mm/mmap.rs`

### brk 扩堆不能无条件 MAP_FIXED 覆盖外部 VMA
- **根因**: `sbrk/brk` 扩堆如果直接用 `MAP_FIXED` 映射新增范围，会先 unmap 目标区间；当 SysV SHM 等外部 VMA attach 在 break 上方时，heap 会把它覆盖掉，LTP `shmt09` 会发现本应 `ENOMEM` 的扩堆意外成功。
- **修复**: 扩堆前扫描目标范围，只允许覆盖/合并已有的私有匿名可写用户 VMA；遇到共享映射、文件映射或非可写用户映射时返回旧 break。ELF program break 初始化也应取所有 `PT_LOAD` 页尾最大值，避免初始 break 落在已有 load 段之前。
- **教训**: 不能简单把所有 overlap 都视为失败；glibc/musl 启动和历史 brk 可能已经在 heap 边界留下私有匿名 VMA。需要区分 heap 自身 VMA 与 SHM/mmap 外部阻挡，否则会修好 `shmt09` 但打坏 `brk01/brk02`。
- **相关文件**: `os/src/mm/mmap.rs`, `os/src/mm/address_space.rs`

### 无界队列导致 OOM
- **根因**: 生产者-消费者队列（`VecDeque`、`Vec` 等）无上界，消费者慢于生产者时持续堆积，底层存储重分配触发大块内存请求超出堆容量
- **症状**: 运行一段时间后 `alloc` 返回 `Err`，分配请求异常大（~96MB），远超单次正常分配
- **修复**: 在 `push`/`push_back` 前检查 `len() >= MAX_QUEUE_LEN`；超限时静默丢弃并记录 warning；上限设为命名常量便于调优
- **模式**:
  ```rust
  const MAX_QUEUE_LEN: usize = 4096;
  if queue.len() >= MAX_QUEUE_LEN {
      log::warn!("queue full, dropping");
  } else {
      queue.push_back(item);
  }
  ```
- **注意**: 上限值需权衡内存和丢包率；4096 × MTU(1500) ≈ 6MB 通常安全
- **相关文件**: `os/src/drivers/net/veth.rs`

### kernel stack 溢出静默破坏 heap
- **根因**: 架构把每线程 kernel stack 直接放在 kernel heap 的 `Vec<u8>` 中时，向下增长的栈一旦越界会先写坏相邻 heap 对象，后续常表现为随机 `BTreeMap`/allocator panic，而不是在真实溢出点 fault。
- **修复**: 将 kernel stack 放到页表映射的固定 kernel VA 窗口，每个 slot 只映射实际栈页，并在增长方向保留未映射 guard page；栈映射需标记为 kernel-stack 类别，避免干扰 ELF/interpreter 临时 program 映射的 `highest_addr()`。
- **教训**: 大规模 clone/futex 压力下出现随机 heap 元数据损坏时，要优先排查 per-task kernel stack 的存放位置和溢出保护；guard page 能把“延迟随机崩溃”收敛成可定位的 kernel trap。
- **相关文件**: `os/src/hal/arch/loongarch64/kern_stack.rs`, `os/src/mm/kernel_space.rs`

## 文件系统

### VirtIO 块驱动 512B 拆分导致 8x I/O 请求放大

- **根因**: `virtio-drivers` 库的 `read_blocks(sector, buf)` / `write_blocks(sector, buf)` 实际支持多扇区缓冲区（buf 可以是 N×512B），但 MangoCore 的 `VirtIOBlock::read_block/write_block` 用 `buf.chunks(512)` 把每个 4KB 块拆成了 8 次独立 VirtIO 请求
- **修复**: 将 `buf.chunks(VIRT_IO_BLOCK_SZ)` 改为 `buf.chunks(BLOCK_SZ)`，扇区地址 = `(block_id + chunk_idx) * BLOCK_RATIO`。iozone writeback 请求数从 149K 降到 18.6K（8x），写吞吐提升 2.11x
- **教训**: 底层驱动库的 API 能力可能与上层包装不一致；先确认库支持什么粒度的 I/O，再决定是否拆分。VirtIO 安全上限 = BLOCK_SZ（单页物理连续），跨页批量需先保证 DMA 缓冲区物理连续性
- **相关文件**: `os/src/drivers/block/virtio_blk.rs`, `os/src/drivers/block/virtio_blk_pci.rs`

### 轮询 AHCI 的扇区级拆分掩盖上层批量 I/O

- **根因**: PageCache/ext4 已把连续页合并为最多 256 KiB 的块请求，但 2K1000LA SATA 包装层再次按 512 B 调用轮询式 `READ/WRITE DMA EXT`。每个扇区都重复 command FIS/PRDT 设置、PxCI doorbell 和完成轮询；一个 256 KiB 请求被放大为 512 条 ATA 命令。
- **修复**: 在控制器初始化阶段分配并永久持有一个 64 KiB 连续低端 DMA 槽，AHCI API 按缓冲区真实长度设置 PRDT byte count 和 ATA sector count，包装层只按槽位上限切分。控制器已有互斥串行化时只需一槽，不要机械移植多槽状态机。
- **教训**: “DMA 池化”必须结合设备并发模型理解。若驱动只有一个在途命令，收益来自常驻连续缓冲和命令合并，不来自槽位数量。优化后还要分离 PageCache 命中、设备冷读和用户态 CPU 时间；本次 64 KiB 到 256 KiB 无可测收益，而 CPython 无 pyc 的解析/编译才是后续最大瓶颈。
- **相关文件**: `dependency/dep_iso/src/block/ahci.rs`, `dependency/dep_iso/src/provider.rs`, `os/src/drivers/block/sata_blk.rs`

### 只读语言运行时禁用字节码会把 CPU 瓶颈误判为磁盘慢

- **根因**: 解释器和标准库放在只读分区后，启动包装器用 `PYTHONDONTWRITEBYTECODE=1` 避免写盘；每个新进程因此重复读取、解析并编译所有导入的 `.py`。把运行时复制到 tmpfs 只能减少系统态 I/O，无法消除主要的用户态编译时间。
- **修复**: 保持源码/解释器分区只读，通过 `PYTHONPYCACHEPREFIX` 把 pyc 放到独立可写层，优先选择已验证的持久 ext4，再回退 scratch/tmpfs；继续使用解释器原生 invalidation，不复制或修改系统源码树。
- **教训**: 性能 A/B 必须同时记录 real/user/sys。若换成 tmpfs 后 user time 几乎不变，继续扩大 DMA 很可能无效；检查 JIT、字节码、动态链接和重复解析等用户态工作。缓存首次填充与稳定命中要分别报告。
- **相关文件**: `user/tools/cpython/python3-wrapper.sh`, `os/build_initramfs.sh`, `user/src/bin/initproc.rs`

### 性能计数器均值误导 — 必须拆 hit/miss

- **根因**: `pc_read_cycles / pc_read_calls = 91K cycles/次` 看似 cache-hit 开销很大，但拆分 `PC_READ_HIT_CYCLES` / `PC_READ_MISS_CYCLES` 后发现：hit 仅 13K cycles，但 4.5% 的 miss 每次 1.8M cycles 拉高均值。iozone 场景 80% miss rate 更是完全改变了瓶颈判断
- **修复**: 在读路径检测 `PC_READ_MISS` 计数器的前后变化来判断本次 read 是否有 miss，然后分别计入 hit/miss 周期桶；同时拆分 Phase1(lookup) / Phase2(copy) 子周期
- **教训**: 带 miss 的 I/O 路径不能只看均值；必须同时有 miss_rate 和 hit/miss 各自耗时才能判断瓶颈是"快路径太慢"还是"慢路径太多"
- **相关文件**: `os/src/task/perf.rs`, `os/src/fs/page_cache.rs`

### 计时 API 必须匹配诊断 profile

- **根因**: PageCache 读路径的 recorder 由 `memory_io` profile 启用，但采样边界调用 `perf_time_now()`，后者只在 core profile 激活时读取时钟。于是 calls/pages 增长而 read total/lookup/copy 周期全为零；写路径使用 memory-I/O API 所以不受影响。
- **修复**: 同一诊断域的“采样起点”和“采样终点”必须都调用对应 profile 的时间 API；`memory_io` 域使用 `perf_memory_io_time_now()`，不能混用 core-only `perf_time_now()`。
- **验证**: 选择目标 sysfs profile 跑真实 workload，并同时确认调用计数与阶段周期非零；若 read-user 专属桶仍为零，先按实际调用路径区分 `read()` kbuf 与 `read_user()`，不要把未覆盖路径误诊为 profile 失败。
- **相关文件**: `os/src/task/perf.rs`, `os/src/fs/page_cache.rs`

### 性能 A/B 前必须验证 workload 真正覆盖目标路径（pages/call 比值）

- **根因**: 用 `-r 1k`（1 KiB 记录）跑 PageCache 读路径优化验证时，`pc_read_pages / pc_read_calls ≈ 1.0`，实际走的全是单页 fast path；多页 batch-lookup 优化代码根本没被执行。据此宣称的"多页 lookup 降 50%"是建立在未覆盖路径上的错误结论。
- **修复**: 基准前先读计数器确认路径覆盖：`pc_read_pages/pc_read_calls > 1`（如 ≥1 MiB record / 16 MiB 文件得到 ~61 pages/call）才真正命中多页路径。A/B 结果按窗口归一（cycles/page、cycles/call），并配对比较（lookup 5/5 对全变差、区间不重叠才算"变差"）。
- **教训**: 任何路径级优化（batch-lock、scan_range、reference_and_load_flags）都必须先证明 workload 遍历了目标代码段；单页 fast path 的优化收益不能外推为多页路径结论。iozone `-i 0 -i 1 -r 1k` 是热读回归探针，不用于验证多页路径本身。
- **相关文件**: `os/src/task/perf.rs`, `os/src/fs/page_cache.rs`, `iozone-AB-testcode.sh`

## 网络栈

### WaitQueue 队列锁重入与有损通知（accept 永久阻塞）
- **历史根因**: 旧版 `WaitQueue::wait_until_interruptible()` 在持有队列锁时再次执行
  condition；若 condition 内的 `NET_INTERFACE.poll()` 同步通知同一个队列，通知路径只能用
  `notify_events_all_if_unlocked` 避免自锁，并会在锁冲突时静默丢弃 wake。TCP accept 因而可能
  错过首个 SYN 后永久睡眠。
- **最终修复**: 每轮等待先在短临界区登记携带 `WaitEntry` token 的 waiter，再释放队列锁执行
  condition。生产者一律使用可靠通知，先把本轮 token 原子置为 notified，再尝试唤醒已经进入
  Blocking 的任务；消费者在切换前通过 checked block 复查 token，闭合“通知先于阻塞”的窗口。
- **性能边界**: accept 的 pre-poll 和纯 accept 检查仍可作为减少重复 poll 的性能策略，但不再是
  WaitQueue 的正确性约束。普通 wait condition 可以推进生产者；需要业务锁保护原子条件的路径
  则使用 locked wait，并遵守该业务锁自身的不可重入约束。
- **验证**: 永久 ktest `condition_can_notify_same_queue` 在登记后的第二次 condition 检查中通知
  同一队列，证明不会自锁，且通知 token 能阻止任务漏睡。
- **相关文件**: `os/src/task/manager.rs`, `os/src/fs/vfs/event.rs`,
  `os/src/kernel_tests/waitqueue.rs`, `os/src/net/socket/inet/stream/mod.rs`

### WaitQueue 闭包内 poll 导致唤醒丢失（accept 永久阻塞）——早期修复记录
- **根因**: `WaitQueue::wait_until_interruptible()` 的 condition 闭包在队列锁持有时执行；如果在闭包内调用 `NET_INTERFACE.poll()`，轮询路径中 `notify_events_all_if_unlocked` 会因为队列锁已持有而静默丢弃唤醒，导致阻塞的 waiter 永久睡眠。TCP accept() 在闭包内 poll 会错过首个 SYN 连接。
- **修复**: 
  1. `NET_INTERFACE.try_poll()` 必须在 WaitQueue 闭包外部调用（pre-poll）
  2. 使用无条件监听扫描（`wake_tcp_accept_waiters()`）在每次 poll 后唤醒 accept waiters，不依赖 smoltcp 的 poll 返回值
  3. WaitQueue 闭包内只做纯状态检查（accept），不做任何会触发唤醒的操作
- **教训**: 所有 WaitQueue condition 闭包必须是无副作用的纯检查函数；任何可能触发唤醒操作（poll、dispatch、notification）都必须在闭包外部执行
- **相关文件**: `os/src/net/syscall/accept.rs`, `os/src/net/config.rs`, `os/src/net/socket/inet/stream/mod.rs`

## 错误码对齐（Linux 语义）

### linkat/link/renameat 多路径 syscall errno 优先级

- **根因**: Linux v6.6 `do_linkat` + `vfs_link` 中 errno 有严格优先级：
  ```
  flags(EINVAL) > old_lookup(EBADF/ENOTDIR/ENOENT) > new_lookup(EBADF/ENOTDIR/ENOENT)
  > EXDEV > EPERM(old_is_dir) > EEXIST > EACCES(parent_perm)
  ```
  **关键：old-is-dir EPERM 必须在 new_path 解析完成之后检查**，否则坏 newdirfd 会得到 EPERM 而不是正确的 EBADF/ENOENT。
- **适用**: renameat、linkat、symlinkat 等所有同时接收 old/new 路径的 syscall。
- **教训**: 实现双路径 syscall 时，errno 测试必须按此优先级顺序，每条路径分别构造测试用例。

## 调度/性能

### WaitQueue wake-all 路径性能
- **根因**: 每唤醒一个任务都扫描全局队列
- **修复**: 批量收集待唤醒任务，一次性更新 `TASK_MANAGER` 队列

## epoll fd 嵌套监听语义

- **根因**: Linux 允许 epoll fd 被另一个 epoll 监听，只有自监听、环路和过深嵌套需要拒绝；一律拒绝目标 fd 为 epoll 会让 LTP `epoll_ctl04/05` 在搭建测试图时提前 `EINVAL`
- **修复**: `EPOLL_CTL_ADD` 对目标 epoll fd 做 DFS 检查，环路返回 `ELOOP`，超过兼容深度返回 `EINVAL`；同时让 `EventPollFile` 暴露读等待队列，父 epoll 可以等待子 epoll ready
- **教训**: epoll 的 `EPERM` 只适用于不支持 poll/epoll 的普通 fd，不应套用到 eventpoll fd；嵌套图必须防止递归扫描形成环
- **相关文件**: `os/src/fs/eventpoll.rs`

## pipe fcntl/sysctl 兼容语义

- **根因**: LTP 的 pipe/fcntl 用例不只看 `F_GETPIPE_SZ/F_SETPIPE_SZ` 是否存在，还依赖 `/proc/sys/fs/pipe-max-size`、`pipe-user-pages-*`、`ioctl(FIONREAD)`、`F_SETPIPE_SZ(0)` 和 capability 错误码优先级
- **修复**: 注册最小 `/proc/sys/fs/pipe-*` 节点；`F_SETPIPE_SZ(0)` 归一到一页；超过 `1<<31` 返回 `EINVAL`，无 `CAP_SYS_RESOURCE` 且超过 pipe max 返回 `EPERM`；pipe `FIONREAD` 返回 ring buffer 当前可读字节数
- **教训**: pipe 容量测试经常通过 `FIONREAD` 验证数据量，write/read 返回值正确但 ioctl 没实现也会失败；环形缓冲读写必须跨尾回绕，否则 64KiB 大块读写会被截断
- **相关文件**: `os/src/fs/dev/pipe.rs`, `os/src/fs/procfs/files/sys.rs`

## vmsplice 最小兼容路径

- **根因**: LTP `vmsplice04` 只需要 pipe 写入与阻塞/非阻塞语义； syscall 未实现会直接 TBROK，但完整 Linux 零拷贝页转移并非必要前置
- **修复**: 将用户 iovec 复制到内核临时缓冲，复用现有 pipe `File::write()` 与写等待队列；支持 `SPLICE_F_NONBLOCK` 返回 `EAGAIN`，阻塞模式等待 pipe 可写
- **教训**: 对裸机评测可先实现“语义兼容、安全复制”路径，覆盖用户可见行为，同时避免引入复杂页生命周期和新堆泄漏风险
- **相关文件**: `os/src/syscall/fs.rs`, `os/src/syscall/syscall_id.rs`

## splice stream fd 阻塞语义

- **根因**: pipe/pty 等 stream fd 的底层 `File::read()`/`write()` 用 `EAGAIN` 表示暂不可读/写；`splice()` 如果不区分 fd 阻塞属性，会把阻塞 fd 上的临时不可用直接暴露给用户态，LTP `splice02` 中子进程先读空 pipe 时失败
- **修复**: `off_in/off_out == NULL` 的 stream 路径复用 inode read/write wait queue；只有 `SPLICE_F_NONBLOCK` 或 fd `O_NONBLOCK` 时才直接返回 `EAGAIN`，阻塞模式等待到非 `EAGAIN` 结果或被信号打断
- **教训**: `splice`/`tee`/`vmsplice` 这类零拷贝接口即使内部先做安全复制，也必须保留阻塞 fd 的等待语义；不要把底层 pipe 的内部重试信号当作最终 syscall errno
- **相关文件**: `os/src/syscall/fs.rs`, `os/src/fs/dev/pipe.rs`

## fcntl POSIX record lock 生命周期

- **根因**: `F_SETLK/F_GETLK` 不只是保存一条整段锁记录；同一进程重复锁定/解锁重叠区间时，需要拆分旧区间、保留左右残余并合并相邻同类区间。只做覆盖删除会让 `F_GETLK` 返回错误的锁类型、起点和长度，LTP `fcntl11` 会在多个 block 中失败
- **修复**: 以 `(dev,inode,pid)` 维护进程级 advisory lock 表；设置新锁前拆分本 PID 重叠旧锁，插入后合并相邻同类区间；`F_GETLK` 忽略本 PID 锁并返回最早冲突区间
- **教训**: POSIX record lock 的释放也绑定 fd 生命周期：`close/close_range`、`dup2/dup3` 覆盖目标 fd、exec CLOEXEC 关闭和进程退出都要清理对应锁，否则后续 fork/exec/close 组合测试会出现假冲突或锁表残留
- **相关文件**: `os/src/syscall/fs.rs`, `os/src/task/mod.rs`, `os/src/task/task.rs`

## flock open file description 语义

- **根因**: `flock(2)` 锁跟 open file description 绑定，而不是单纯跟 PID 或 inode 绑定；fork 后继承的 fd 与父进程共享同一个 open-description，子进程对该 fd `LOCK_UN` 应释放父进程持有的 flock。按 PID 实现会让 LTP `flock03` 失败，按 inode 全局实现会让同一 fd 重入/解锁行为错误
- **修复**: 用 `vfs::File` 共享的 offset `Arc` 指针作为 open-description id，锁表按 `(dev,inode,description)` 维护；close/close_range/CLOEXEC/dup 覆盖/进程退出时按 description 引用计数释放最后一个引用
- **教训**: fcntl record lock 与 flock 都是文件锁，但生命周期不同：前者是进程级，后者是 open-description 级，不应共用 owner 规则
- **相关文件**: `os/src/fs/vfs/file.rs`, `os/src/syscall/fs.rs`, `os/src/task/process.rs`

## getcwd 跨挂载根路径重建

- **根因**: `absolute_path()` 反向重建路径时，挂载根 inode 没有自己在父目录中的名字；名字属于父文件系统里的挂载点 dentry。若直接在挂载点 inode 中查挂载根 inode 的 entry name，会返回 `ENOENT`，`getcwd()` 最终退回 symlink 逻辑路径，导致 LTP `getcwd03` 失败。
- **修复**: 遇到 `MountFSInode::is_mountpoint_root()` 且存在 `self_mountpoint` 时，先切换到挂载点 dentry，再继续从该 dentry 的父目录反查名称；对普通目录用 bounded parent/name hint 和 FS `get_entry_name()` fallback。
- **教训**: VFS 路径反查必须区分“挂载根 inode”和“挂载点 dentry”。`do_parent()` 适合路径解析 `..`，但 `getcwd()` 需要按 dentry/mount 树语义跨 mount boundary。
- **相关文件**: `os/src/fs/vfs/mount.rs`, `os/src/syscall/fs.rs`

## ns_last_pid 与 pidfd identity

- **根因**: LTP `pidfd_send_signal03` 会写 `/proc/sys/kernel/ns_last_pid`，强制下一次 fork 复用旧 PID，再验证旧 pidfd 不会指向新进程。若用户可见 PID/TID 分配器为了性能改成永远 fresh，且释放时不记录 released 状态，`ns_last_pid` 对低于当前水位的 PID 就无法生效。
- **修复**: 普通 `tid_alloc()` 继续单调递增，避免并发 fork/clone 早期复用；释放时只在 bitmap 标记 ID 已释放，不塞回普通 free-list；`set_ns_last_pid()` 对已释放 ID 设置 one-shot hint，由下一次 `alloc_fresh()` 消费。
- **教训**: pidfd 必须保存进程对象 identity，而不是只保存数字 PID；PID 复用只应让新的 `find_process(pid)` 找到新 PCB，旧 pidfd 仍应因旧 PCB `pid_released()` 返回 `ESRCH`。
- **相关文件**: `os/src/task/pid.rs`, `os/src/fs/pidfd.rs`

## POSIX timer overrun 饱和语义

- **根因**: LTP `timer_settime03` 覆盖 Linux CVE-2018-12896 场景：极小周期 timer 会产生超过 `i32::MAX` 的 overrun。`timer_getoverrun()` 固定返回 0，或每次内核 tick 只加 1，都会被该用例打穿。
- **修复**: 在 POSIX timer 状态中保存 overrun；`TIMER_ABSTIME` 初始时间已过期时按绝对 clock 差值计算初始 overrun；周期重装时用 `(now - deadline) / interval` 批量追赶遗漏到期次数，返回用户态前饱和到 `i32::MAX`。
- **教训**: timerfd/POSIX timer 的周期语义不能依赖调度 tick 频率逐次补偿；所有短 interval/长阻塞场景都应按真实时间差一次计算，否则既慢又不符合 Linux 边界行为。
- **相关文件**: `os/src/task/process.rs`, `os/src/task/manager.rs`, `os/src/syscall/process/time.rs`

## POSIX timer 参数 LTP 不能替代 SMP 生命周期交错

- **覆盖边界**: `timer_settime01/02` 能验证多种 clock 的 set/get、periodic/absolute 参数和
  EFAULT/EINVAL 顺序，但其单线程流程不证明 timer ID 在线程组内共享，也不证明创建线程退出、
  fork 空表、exec 删除、delete/recreate 与正在到期 callback 的并发顺序。
- **证据写法**: 双架构 8 核启动和双 libc 通过只能记为功能非回归；上述交错必须列为 NOT RUN，
  直到有 pthread barrier/精确 hook 控制线性化窗口的 focused probe。不能以“共用同一份 Rust
  代码”代替另一架构运行，也不能以 clock ID 参数被接受外推 CPU-time 到期由 CPU 消耗驱动。
- **环境故障归因**: recipe 因脚本无执行位返回 126 时，先保留失败日志并修正调用方式为显式
  `bash script`，只补跑缺失 case；无需重复已经在同一冻结 diff 上通过的双架构构建和另一架构
  focused gate。
- **相关文件**: `cc-codex/protocol/test-recipes.json`,
  `os/src/syscall/process/time.rs`, `docs/Work_Log/evidence/`

## 默认致命信号日志区分同步 fault 与用户投递

- **根因**: wait/signal 类 LTP 用例会主动 `raise()`/`kill()` SIGILL、SIGSEGV，再用 wait status 验证默认动作。若 `do_signal()` 对默认 SIGILL/SIGSEGV 一律读取最近一次 trap cause 打印 `Exception(...) in application`，用户态显式投递信号会被误报成 `UserEnvCall`/`Syscall` 异常，自动扫描器可能把正常通过用例排除。
- **修复**: trap 路径把页错误、非法指令等同步异常转成 signal 时写入正向 `SEGV_*`/`ILL_*` `si_code`；`do_signal()` 只在 pending siginfo 表明这是同步 fault 时打印异常诊断，普通用户投递信号只走默认终止和 wait status。
- **教训**: signal 来源不能靠“当前或最近 trap cause”倒推；syscall 发出的 `kill/tgkill/raise` 与真实硬件 fault 在默认动作上都可能终止进程，但只有后者应进入内核异常日志。
- **相关文件**: `os/src/task/signal/mod.rs`, `os/src/task/task.rs`, `os/src/hal/arch/riscv/trap/mod.rs`, `os/src/hal/arch/loongarch64/trap/mod.rs`

## 修复后同步解除 LTP skip

- **根因**: inline LTP broad skip 表是人工维护的扩分保护层；内核语义已经修复后，如果旧 skip 原因不删除，后续自动 include/全量扫描仍会把可通过用例排除，表现为“focused 已 TPASS，但扩分没有增长”。
- **修复**: 每次 focused 证明某个 skip 用例在双架构 musl/glibc 均 0 failure 后，同步删除对应 `should_skip_ltp_helper()` 分支，并在 Work_Log 中记录验证来源。
- **教训**: skip 表不是事实来源，日志验证结果才是；修复内核 bug 后要回扫 `user/src/bin/initproc.rs`，避免陈旧 skip 抵消本次适配收益。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`, `Doc/Work_Log.md`

## 架构+libc 专属 LTP 差异收敛

- **根因**: 部分 LTP 用例实际验证的是 libc wrapper、格式化库或测试镜像行为；同一内核路径在另一个 libc 或另一个架构已经 TPASS 时，强行按失败组合改内核容易破坏正确 ABI。
- **修复**: 只在 focused 证明“失败限定为某架构+某 libc，且至少一个等价路径继续覆盖内核语义”后，加入架构+libc 专属默认 exclude；inline `initproc` 与 suite `ltprunner` 必须同步。
- **教训**: 不要扩大到全 musl/全架构，也不要在内核里伪造用户态 wrapper 行为；Work_Log 必须写清楚哪个组合仍实际运行该用例。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`

## SysV SHM fork 继承不只复制 VMA

- **根因**: `fork` 复制地址空间后，子进程拥有了 SHM 映射 VMA，但 SysV SHM registry 仍只记录父进程 attach；`shmctl(IPC_STAT)` 的 `shm_nattch` 少计，子进程 `shmdt()` 也会因查不到 `(pid, addr)` attachment 返回 `EINVAL`。
- **修复**: 在非 `CLONE_THREAD` 的 clone/fork 成功初始化新进程后，调用 SHM registry helper 按父 pid 复制 attachment 元数据到子 pid；线程 clone 不新增进程级 attachment。
- **教训**: fork 语义需要同时复制用户页表和进程级内核元数据。LTP 看到 “nattch 计数不对 + 子进程 detach EINVAL” 时，优先检查 registry/owner 表是否接入 clone 路径，而不是只查 VMA 映射。
- **相关文件**: `os/src/task/task.rs`, `os/src/syscall/process/ipc.rs`

## signal ucontext sigset padding 偏移

- **根因**: Linux/glibc 用户态 `ucontext_t.uc_sigmask` 对外占固定 128 字节；内核若先写较小的自定义 `UserSignalMask`，又额外固定补 128 字节 padding，会把后续 `uc_mcontext` 整体后移。SA_SIGINFO handler 读取 PC 时会落到 padding，LTP `profil01` 表现为 glibc 无法记录任何 profile bucket。
- **修复**: 将 `UserSignalMask + __pad` 的总大小约束为 128 字节，`__pad` 按 `128 - size_of::<UserSignalMask>()` 计算；signal 投递和 `sigreturn` 共用同一 `UserContext::PADDING_SIZE`，避免恢复路径偏移不一致。
- **教训**: signal frame 是 libc 可见 ABI，不能按内核内部 bitset 大小直觉拼结构；涉及 `ucontext_t`、`mcontext_t`、`sigset_t` 的偏移时，优先核对“总 ABI 保留区大小”，再判断寄存器数组内容是否需要重排。
- **相关文件**: `os/src/hal/arch/riscv/trap/context.rs`, `os/src/hal/arch/loongarch64/trap/context.rs`, `os/src/task/signal/mod.rs`, `os/src/syscall/process/signal.rs`

## POSIX mqueue libc/syscall ABI

- **根因**: POSIX API 要求用户传 `/name`，但 Linux syscall 层接收的是去掉前导 `/` 的裸 name；同时 `mq_timedsend/mq_timedreceive` 的 timeout 是 `CLOCK_REALTIME` 绝对时间，不能直接交给内核单调时间等待队列。`mq_notify(SIGEV_THREAD)` 也不是普通 signal，glibc/musl 会把 32 字节 cookie 和 netlink fd 交给内核，等待内核把 cookie 写回 netlink socket 后再触发用户 callback。
- **修复**: mqueue syscall 层按裸 name 校验并返回 Linux errno；timeout 先用 realtime now 计算 duration，再转成内核 `TimeSpec::now()` deadline；`SIGEV_SIGNAL` 用 `SI_MESGQ` siginfo 投递，`SIGEV_THREAD` 复制 32 字节 cookie 并在队列空转非空时写回 netlink recv queue，通知触发后一次性清除注册。
- **教训**: 不要直接按 POSIX libc API 形态实现内核 syscall 入参；mqueue 的 name、timeout 和 notify 都经过 libc 包装，LTP 同时覆盖 musl/glibc，必须验证双 libc。
- **相关文件**: `os/src/syscall/process/ipc.rs`, `os/src/net/socket/mod.rs`, `os/src/net/socket/netlink/mod.rs`

## LTP inline 与 suite runner 过滤表同步

- **根因**: inline runner 的 `should_skip_ltp_helper()` 会跳过已确认的 TCONF/环境项，但 suite runner 只读取默认 exclude；同一批用例在 suite 模式下仍会实际执行，LTP 返回 32 后被 harness 打成 `FAIL LTP CASE ... : 32`，表现为“inline 全量干净，suite/云端分数低”。
- **修复**: 将稳定不支持或环境不满足的非目标项同步进 `ltprunner` 默认 exclude；修复后 suite focused 日志应显示 `skip excluded case ...` 和 `filtered=0`，而不是进入 `RUN LTP CASE`。
- **教训**: 每次维护 inline broad-skip 后都要检查 `user/src/bin/ltprunner.rs`，否则本地 focused/inline 结果无法代表 suite 评测路径；需要调试被排除项时，应临时调整配置或过滤表，验证通过后再解除。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`

## LTP libc wrapper 用例不能反向改 raw syscall 语义

- **根因**: LTP `clone04` 验证的是 libc `clone()` wrapper 对 NULL child stack 返回 `EINVAL`；旧 musl wrapper 会把该非法 libc API 调用继续下传，导致用户态 trampoline 在空栈附近 SIGSEGV。内核 raw `clone(SIGCHLD, 0, ...)` 仍是 fork 兼容路径，不能为了 wrapper 用例在内核里拒绝 `stack=0`。
- **修复**: 将失败限定在旧 musl wrapper 的组合做 musl-only exclude，保留 glibc 路径继续覆盖 wrapper EINVAL 行为；内核 clone/fork 语义不做伪修。
- **教训**: 遇到 LTP metadata 标注 `musl-git`/`glibc` 的用例，先区分“libc API 合约”和“内核 syscall ABI”；只有 raw syscall ABI 错误才改内核。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`, `os/src/syscall/process/clone.rs`

## LTP clock_gettime04 与虚拟化阈值

- **根因**: `clock_gettime04` 默认按 5ms 判定连续读时间跳变，只有 `tst_is_virt()` 识别到虚拟机才放宽阈值；测试镜像缺少 `systemd-detect-virt` 时，heap_trace QEMU 下 syscall/调度抖动会被记为 `TFAIL`，扩大扫描中 glibc/musl 都可能触发。
- **修复**: 将 `clock_gettime04` 作为默认环境/性能阈值项过滤，保留 `clock_gettime01/02` 对同一 syscall 的基础语义覆盖；不要为了测试阈值虚报 `clock_getres()` 精度。
- **教训**: 时间精度类 LTP 要先看测试自己的阈值来源、虚拟化检测和 libc 组合；若要重新放开，优先优化 syscall/调度耗时或补齐测试环境检测。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`, `os/src/syscall/process/time.rs`

## LTP time namespace 配置类 TCONF

- **根因**: `clock_gettime03`、`clock_nanosleep03`、`sysinfo03` 等用例会读取 `/proc/config` 并要求 `CONFIG_TIME_NS=y`；当前内核未实现 time namespace，测试返回 `TCONF(32)`，suite runner 会把非 0 退出码记成失败。
- **修复**: 将这类配置不满足项同步加入 suite `ltprunner` 默认 exclude 与 inline broad-skip 表；相邻基础 syscall 用例仍保留实际运行覆盖。
- **教训**: LTP 返回 32 不一定是 syscall 语义失败，先看日志里的 kconfig constraint；配置类 TCONF 不应通过伪造 syscall 行为解决。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`

## LTP suite 环境项要按根因过滤

- **根因**: `rt_tgsigqueueinfo01` 这类 runtest 条目可能存在但镜像未携带同名二进制，`signal06` 这类用例也可能明确限定 x86_64；它们在 suite 模式下会以 127/TCONF(32) 退出，被 harness 记为失败，但不是内核 syscall 语义问题。
- **修复**: 将缺二进制、架构限定等环境项加入 suite 默认 exclude，并确认相邻 syscall 用例仍在运行覆盖真实内核路径。
- **教训**: 看到 `command not found`、`Only test on x86_64` 先归类为环境/架构项，避免为了提分修改无关内核行为。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`

## LTP userspace/fs 依赖型 TCONF 过滤

- **根因**: `eventfd06` 依赖测试镜像中的 libaio，`futex_wake04` 依赖 hugetlbfs；当前镜像或内核配置不满足时 LTP 返回 TCONF(32)，suite runner 会按非 0 退出码记成失败。
- **修复**: 将这类环境依赖项加入 suite 默认 exclude，同时保留相邻基础用例实际运行，例如 `eventfd01-05/eventfd2_*`、`futex_wake01/02`、`futex_wait*`、`futex_cmp_requeue*`。
- **教训**: eventfd/futex 名下的失败不一定代表对应 syscall 错误；先读 TCONF 原因和测试依赖，再决定是补内核能力、补镜像依赖，还是作为环境项过滤。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`

## LTP suite 临时目录隔离与 times CPU accounting

- **根因**: suite 模式连续跑 glibc/musl 时共用 `/tmp`，checkpoint/futex 临时文件和状态可能在长序列中互相影响；同时旧 CPU accounting 只在 timer trap 离开时累计 `ru_stime`，普通 syscall 内核时间不可见，`times()` 再向下取整会把非零亚 tick 时间变成 0，表现为 `times03` 偶发 `tms_stime = 0` 或 la64 被外层 30s timeout 误杀。
- **修复**: initproc 给每个 libc 传独立 tmpdir；la64 suite runner 保留更宽的 60s 单例外层超时；任务调度出前结算当前内核态时间，调度入时重置内核态计时起点，回用户态/退出前补齐最后一段系统态时间；`times()` 对非零 CPU 时间向上折算 USER_HZ tick。
- **教训**: `times03` 这类用例同时覆盖 harness 速度、CPU accounting 和 tick 换算，不要直接跳过；先看 `tms_utime/stime/cutime/cstime` 哪一项异常，再区分“测试超时窗口不足”和“内核记账缺失”。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`, `os/src/task/task.rs`, `os/src/task/mod.rs`, `os/src/task/processor.rs`, `os/src/syscall/process/time.rs`

## 冻结测试输入也包括 ignored runner 与 recipe

- **现象**: 生产源码指纹在测试前后完全一致，但长时间 Bash runner 在执行后半段时报变量文本
  被截断或 `unbound variable`；同一轮还混入了与本节点无关的大内存用例，造成伪失败。
- **根因**: 冻结清单只覆盖 tracked production source，没有覆盖 `.gitignore` 下的本地脚本、
  recipe 和 task 文件。Bash 可能延迟读取尚未执行的脚本内容，运行中编辑文件不仅会改变后续
  命令，还可能因长度/偏移变化破坏未读行。
- **修复**: job 启动前把所有 runner、recipe、任务描述和判定脚本复制或哈希到 job artifact，
  执行期间禁止原地修改；需要修配方时取消旧 job，生成新 job ID 后完整复跑。focused 集合只
  放直接覆盖本节点语义的用例，资源规模或完全不同指标的用例单独归类。
- **教训**: `mutation_detected=false` 只证明其 manifest 覆盖的文件未变，不能证明整个测试输入
  冻结。验收前必须同时检查生产源码指纹、runner 输入指纹、原始退出码和真实用例集合。
- **相关文件**: `cc-codex/bin/cc-agent-test.py`, `cc-codex/protocol/test-recipes.json`

## ELF/产物门禁必须精确命中并 fail-fast

- **现象**: QEMU 本身通过，产物脚本也打印 PASS marker，但数值明显不合理，
  例如要求 25 MiB 大符号却报告 `size=1`。
- **根因**: 宽泛 grep 命中了同前缀的另一符号；中间 `test` 失败后脚本没有
  `set -e`，后续无条件 `echo PASS` 又将整体退出码覆盖为 0。`readelf` 的 size
  还可能是 `0x...`，直接交给 `test -ge` 会解析失败。
- **修复**: 使用能排除同前缀符号的精确模式；脚本从入口开启
  `set -euo pipefail`；对十六进制 size 先用 Bash arithmetic `$((value))` 转为整数；
  PASS marker 必须是所有断言之后的最后一步。
- **教训**: marker 存在不等于它之前的断言真的执行成功。模型对“架构差异”的
  自然语言解释不能覆盖显然违反数量级的原始数据。
- **相关文件**: `cc-codex/protocol/test-recipes.json`, ELF/readelf 产物验收脚本

## libc waitid 用例不覆盖 raw 第五参数

- **根因**: POSIX libc `waitid()` 公开四参数接口，Linux raw syscall 才有第五个 rusage 指针；
  LTP `waitid01..11` 主要验证 PID 过滤、WNOHANG/WNOWAIT、stop/continue 和 siginfo。`wait401`
  虽传 `struct rusage`，但只检查 wait 返回值与 status，不断言资源字段内容。
- **判定**: 这些用例全部通过只能证明 wait 生命周期没有退化，不能证明 raw waitid rusage、
  非零 user/system 值或 copyout EFAULT 后的消费语义。没有专用用户探针时必须标记 NOT RUN。
- **效率**: focused recipe 可排除依赖 `/proc/sys/kernel/pid_max` 的 `wait402` 和 core-pattern 环境的
  `waitid10`，保留 `wait401/403`、`waitid01..09/11` 覆盖直接相关状态路径。
- **相关文件**: `os/src/syscall/process/lifecycle.rs`, `os/src/task/process_manager.rs`

## SysV IPC STAT_ANY 的 full id 与 index 兼容

- **根因**: Linux `SHM_STAT/SHM_STAT_ANY`、`SEM_STAT_ANY`、`MSG_STAT_ANY` 的实现会用 `ipc_obtain_object_idr()` 按 full id 映射到底层 idr 槽位；LTP 可能先用真实 shmid/semid/msqid 探测支持情况。若内核只把入参解释成“当前表的第 n 个元素”，在 id 单调递增或删除后有空洞时会返回 `EINVAL`，表现为 `kernel doesn't support *_STAT_ANY`。
- **修复**: `*_STAT_ANY` 路径先保留现有 index 枚举兼容，再允许入参本身作为现存 IPC id；权限检查仍按命令类型执行，`SHM_STAT_ANY/SEM_STAT_ANY/MSG_STAT_ANY` 不做普通读权限拦截。
- **教训**: SysV IPC 的“index”不是简单的 `BTreeMap nth()` 抽象；遇到 `*_INFO returned valid index` 或 `doesn't support *_STAT_ANY`，先抓实际 syscall 入参，确认测试传的是 slot index 还是 full id，再改 registry 查找策略。
- **相关文件**: `os/src/syscall/process/ipc.rs`

## LTP fork pid_max/大 VMA 用例要先区分环境与内核缺陷

- **根因**: `fork13` 依赖完整 `/proc/sys/kernel/pid_max` 可写与 Linux PID wrap 语义；`fork14` 依赖至少 16TB 用户 VMA 构造 fork overflow reproducer。当前内核为避免即时 TID 复用采用单调 fresh PID/TID 分配，双架构用户地址空间也无法构造 16TB VMA，因此用例会在 setup 阶段 TBROK/TCONF，而不是暴露 fork/clone 崩溃。
- **修复**: 将这类当前环境/架构无法真实覆盖的项同步加入 suite 与 inline 默认过滤；不要为了通过 setup 写一个不生效的 `pid_max` no-op，也不要为单个 reproducer 冒险扩大整套用户 VA 布局。
- **教训**: fork 压力用例失败时先看是否真的进入 `fork()` 路径；如果日志停在 sysctl 或 mmap 构造阶段，应按测试前置条件分类，保留普通 fork/clone/vfork 用例覆盖真实生命周期路径。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`, `os/src/task/pid.rs`, `os/src/mm/mmap.rs`

## LTP SysV IPC 结构 ABI 前置条件

- **根因**: 部分 SysV IPC 用例会先检查测试镜像中的 libc/LTP ABI 结构布局，例如 `semctl08` 要求 `struct semid64_ds` 带 `time_high` 字段，`msgctl05` 要求 `struct msqid64_ds` 带 `time_high` 字段。若用户态头文件布局不满足，用例在进入 syscall 语义前直接 TCONF，suite runner 会按 32 记失败。
- **修复**: 对这类由测试镜像 ABI 布局决定、当前内核无法通过 syscall 行为弥补的用例同步到默认过滤表；保留相邻 `semctl09/semget05/msgctl01-04/msgctl06` 等真实 SysV IPC 用例覆盖内核路径。
- **教训**: IPC 用例报 TCONF 时先区分“用户态 ABI 结构缺字段”和“内核 stat 数据错误”；前者不应通过伪造无效内核字段解决。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`, `os/src/syscall/process/ipc.rs`

## LTP SysV IPC 压力项 runtime 前置条件

- **根因**: `msgstress01` 会大量 fork 并消耗 SysV message queue slot。heap_trace QEMU 下即使所有消息最终收齐，LTP 仍可能先打印 `Out of runtime during forking` 并以返回码 4 计失败，表现为测试预算/压力环境不满足，而不是普通 `msgsnd/msgrcv/msgctl` 语义错误。
- **修复**: 将长耗时压力项加入默认过滤表，保留普通 message queue 和 semaphore case 继续覆盖 IPC 创建、收发、stat、权限和删除路径。
- **教训**: stress 用例出现 runtime/fork budget 警告时，先看是否有真实 `TFAIL/TBROK/PANIC/OOM`；如果只是压力预算不足，应按长耗时项处理，避免为了单个 stress case 放大全量 LTP 超时风险。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`

## LTP madvise 的 memcg/proc/config 前置条件

- **根因**: `madvise06/09/11` 依赖 cgroup/memcg 或内核配置探测，`madvise07` 依赖 memory-failure 配置，`madvise08` 依赖 `/proc/self/coredump_filter`。这些用例会在 setup 阶段 TCONF/TBROK，suite runner 按非 0 记失败，但同窗口的 `madvise01/02/03/05/10` 已覆盖当前支持的 madvise 语义。
- **修复**: 将这类配置/procfs 前置项加入默认过滤表，避免为了提分伪造 cgroup/procfs 文件或错误声明内核配置。
- **教训**: mm syscall 窗口里出现 TCONF/TBROK 时要先区分“madvise 行为错误”和“测试环境探测失败”；前者改 `sys_madvise`，后者过滤并保留基础 madvise case 覆盖。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`, `os/src/syscall/process/mm.rs`

## LTP seccomp 探测必须接真实执行路径

- **根因**: `prctl04` 先通过 `PR_GET_SECCOMP`/`PR_SET_SECCOMP` 探测支持情况；如果内核只让探测返回成功，却没有在 syscall 入口执行 strict/filter 策略，测试会从 `TCONF` 变成真实 `TFAIL`，例如 GET_SECCOMP/close/exit/fork 继承不按预期触发 `SIGKILL`/`SIGSYS`。
- **修复**: 保存 per-task seccomp mode/filter，fork/clone 继承过滤器，并在 syscall 分发入口按模式提前拦截；filter 安装时从用户态复制 cBPF 指令并做最小 verifier，避免保存用户指针或放行未知指令。
- **教训**: 看到 “kernel doesn't support PR_GET/SET_SECCOMP” 这类探测型 TCONF 时，不要只改 `PR_GET` 或 `/proc/config`。一旦宣称支持，就必须补齐用例后续会触发的真实 ABI 行为和 wait status 信号语义。
- **相关文件**: `os/src/syscall/process/ids.rs`, `os/src/syscall/mod.rs`, `os/src/task/task.rs`

## LTP syscall 用例可能依赖 /proc/sys 限额节点

- **根因**: `mq_open01` 这类 syscall 用例会先通过 `/proc/sys/fs/mqueue/queues_max` 保存/调整/恢复内核限额，再验证 syscall 的 `ENOSPC` 等边界错误码。若只实现 `mq_open/mq_send/mq_receive`，但缺失对应 sysctl 节点或写语义，用例会在 ProcFS 前置步骤 `TBROK`，表现为 `ENOENT` 或 `EPERM`，并没有真正覆盖到目标 syscall 的边界路径。
- **修复**: 将 sysctl 节点接到真实内核 limit 状态，读写路径复用同一份 getter/setter；syscall 创建和参数校验也读取该动态状态，而不是给 ProcFS 伪造只读常量。
- **教训**: 遇到 IPC/mm/process syscall 用例在 `/proc/sys/...` 处 broken，先判断它是不是目标 ABI 的配置面。若是，应实现最小可写 sysctl 并接入真实行为；不要只加只读文件，也不要把它简单归类成通用 FS 问题跳过。
- **相关文件**: `os/src/syscall/process/ipc.rs`, `os/src/fs/procfs/files/sys.rs`, `os/src/fs/procfs/files/mod.rs`

## IPC namespace ID 不等于 IPC 对象隔离

- **根因**: `IpcNamespace` 只提供唯一 ID 时，`setns(CLONE_NEWIPC)`/`CLONE_NEWIPC` 可以通过 procfs namespace fd 切换成功，但 SysV IPC registry 若仍是全局查找，子进程会在新 IPC namespace 中用旧 namespace 的 `shmid` 成功 `shmat()`。
- **修复**: IPC 对象创建时记录 namespace id，`shmget/shmat/shmctl` 和 `/proc/sysvipc` snapshot 只匹配当前 namespace；`CLONE_NEWIPC` 不继承父进程 SHM attach 元数据，普通 fork 继承保持不变。
- **教训**: namespace 测例通过 `setns01` 这类 fd/errno 校验不代表隔离语义正确；遇到 `setns02`、`*_nstest` 要检查 registry 可见性，而不是只看 namespace fd 类型是否存在。
- **相关文件**: `os/src/task/ipc_namespace.rs`, `os/src/syscall/process/ipc.rs`, `os/src/task/task.rs`

## 网络

### 硬编码 IPv4 地址替换为 net_core 动态查询
- **根因**: 多处硬编码 `127.0.0.1` / `10.0.2.15` / `10.0.2.2`，QEMU 环境变更时需逐处修改
- **修复**: 用 `net_core::loopback_iface()` / `net_core::default_iface()` / `net_core::default_gateway()` 动态查询，`unwrap_or` 保留硬编码 IP 作为防御性回退
- **模式**:
  ```rust
  crate::net::net_core::loopback_iface()
      .and_then(|d| d.ip_addrs.first().map(|c| c.address()))
      .unwrap_or(IpAddress::v4(127, 0, 0, 1))
  ```
- **关键**: 必须保留 `unwrap_or` 回退，因为接口在 net_core 初始化前可能未注册；`unwrap()` 会导致过早调用 panic
- **相关文件**: 所有 `net/socket/inet/` 下引用 IP 的文件

### 临时 Arc 值上的 MutexGuard 生命周期问题

- **根因**: `current_netns()` 返回 `Arc<NetNamespace>`。链式调用 `current_netns().router.lock()` 创建一个临时 `Arc`，然后在同一语句中获取 `MutexGuard`。当 `MutexGuard` 赋值给变量时，临时 `Arc` 在语句结束时被释放，但 `MutexGuard` 仍然借用临时值 → `temporary value dropped while borrowed`
- **修复**: 将 `Arc` 绑定到变量，确保其存活时间超过 `MutexGuard`：
  ```rust
  let ns = current_netns();
  let mut router = ns.router.lock();  // OK
  ```
- **注意**: 如果 `MutexGuard` 不赋值给变量（仅用于链式调用如 `.clone()`、`.fill_default()`），临时 `Arc` 在 `MutexGuard` 被丢弃后释放，是安全的
- **相关文件**: `os/src/net/socket/netlink/route/route.rs`（lines 127, 169）

## LTP umask/create mode 用例要区分用户创建入口和内核内部文件

- **根因**: `umask()` 是进程 FS 状态，不是全局常量；`openat(O_CREAT)`、`mkdirat()`、`mknodat()` 等用户创建入口必须用当前 umask 清除权限位。若 `sys_umask()` 只返回 0 或创建路径不套掩码，`umask01` 会表现为新文件/目录模式总是原始 `mode`，且旧掩码返回值错误。
- **修复**: 在 `FsStatus` 保存 `umask`，让 fork/clone/unshare 复用既有 FS 状态复制/共享语义；只在用户态创建入口应用 `mode & ~umask`，避免影响 `memfd_create()` 等内核内部固定模式文件。
- **教训**: 创建模式类失败不要只看 inode 后端；先确认 syscall 层是否已经按 Linux ABI 处理进程级状态、旧值返回和内部对象例外。
- **相关文件**: `os/src/task/task.rs`, `os/src/syscall/fs.rs`

## LTP vma01 要同时检查 mmap hint 与 fork 继承 VMA 合并

- **根因**: `vma01` 先用 `mmap(NULL, 9*page)` 制造空洞，再用非 `MAP_FIXED` 的 `mmap(addr_hint, 3*page)` 期望落到该空洞中；fork 后子进程再在相邻地址新建匿名私有 VMA。若内核忽略可用 hint，初始映射可能落到别的空洞并和前驱合并；若 fork 继承 VMA 仍允许和子进程新 mmap 合并，则 `/proc/self/maps` 只看到单个 6 页 VMA。
- **修复**: 非 fixed mmap 在 hint 区间完整空闲时优先按 hint 放置；fork 复制到子进程的用户 VMA 标记为继承来源，并在匿名私有 lazy mmap merge 条件中排除这类 VMA。
- **教训**: VMA 类 LTP 失败不要只看 `/proc/self/maps` 输出格式；先确认 mmap 放置策略、VMA 起点以及 fork 后的合并条件。glibc/musl 的既有映射布局不同，忽略 hint 的 bug 可能只在其中一个 libc 暴露。
- **相关文件**: `os/src/mm/mmap.rs`, `os/src/mm/vma.rs`, `os/src/mm/vma_set.rs`, `os/src/mm/address_space.rs`

## la64 页级 TLB invalidate 必须携带当前 ASID

- **根因**: LoongArch `invtlb 0x5` (`INVTLB_ADDR_GFALSE_AND_ASID`) 的 `rj` 操作数是目标 ASID；若传 `$zero`，只会刷新 ASID 0 的非 global 页。用户进程使用非 0 ASID 时，COW/dirty/write 权限更新后的旧 TLB 项仍保留，表现为同一用户地址反复 Store page fault。
- **修复**: `tlb_invalidate_page()` 读取当前 ASID 并传给 `invtlb 0x5`；kernel page table 修改走单独的 global-page invalidate (`invtlb 0x6`)。
- **教训**: “PTE 已改且 full TLB flush 能修复，但 page flush 不能修复”时，应优先检查页级 flush 的 ASID/global 操作数，而不是继续放大全局 flush。
- **相关文件**: `os/src/hal/arch/loongarch64/tlb.rs`, `os/src/hal/arch/loongarch64/laflex.rs`

## COW 源帧 helper 返回克隆 Arc 时 strong_count 基准要加一

- **根因**: `cow_source_frame()` 返回的是克隆后的 `Arc<FrameTracker>`。如果 VMA 中只有一个真实 owner，函数返回后也会有 VMA entry + 本地变量两个强引用；用 `strong_count == 1` 判断唯一页会永远失败，导致本可原地恢复写权限的 COW 页不断走复制/换页路径。
- **修复**: 在该 helper 语义下把唯一 owner 判断改为 `strong_count <= 2`，并在原地恢复 PTE 写权限后执行对应架构的页级 TLB 刷新。
- **教训**: 用 `Arc::strong_count()` 判断共享/唯一时，必须把当前函数已经克隆出来的临时强引用计入阈值；否则调试现象会像 TLB/COW fault storm，但根因是引用计数基准错。
- **相关文件**: `os/src/mm/vma.rs`

## timer queue deadline 与 wall-clock 绝对目标要分离

- **根因**: timerfd/POSIX timer 的内核队列使用 monotonic deadline 驱动，但 `CLOCK_REALTIME` 绝对 timer 又需要在 wall-clock 跳变时按原始 realtime 目标重定位。若同一个字段混存 wall-clock deadline 和 monotonic deadline，相对 timer 会被 `clock_settime()` 提前/延后触发，绝对 realtime timer 又无法在 clock 跳变后重算。
- **修复**: 内核队列字段只保存 monotonic deadline；只对 realtime absolute timer 额外保存原始 wall-clock 目标。`clock_settime/settimeofday/adjtimex(ADJ_SETOFFSET)` 后扫描 active timer，重算 monotonic deadline 并通过 generation 让旧队列节点失效。周期 realtime absolute timer 到期推进时要同步推进保存的 wall-clock 绝对目标，不能在首次到期后丢弃。没有持久 timer 对象的 `clock_nanosleep(CLOCK_REALTIME, TIMER_ABSTIME)` 需要独立的 clock-change generation 与等待队列，收到 wall-clock 跳变通知后立即重判定。`read/poll/gettime/sweep` 等到期判定路径必须全部使用 monotonic now。
- **教训**: 修复 clock-domain bug 时要检查所有到期判定入口，不只检查 arm/sweep 路径；本次 rv64 对照中 `timerfd_settime()` 已写入 monotonic deadline，但 `read_at()`/`poll()` 仍用 realtime now，导致 `clock_settime(+2s)` 后 80ms 相对 timer 2ms 到期。绝对 sleep 这类“一次性等待”也不能只在入睡时把 realtime 目标换算为相对时长，否则 clock 前跳不会唤醒。周期 timer 修复还要检查“第一次到期后”的状态推进，否则后续 clock 跳变又会退化成相对 monotonic timer。
- **相关文件**: `os/src/fs/timerfd.rs`, `os/src/syscall/process/time.rs`, `os/src/task/sleep.rs`, `os/src/task/task.rs`, `os/src/task/manager.rs`, `user/src/bin/initproc.rs`

## I/O fallback timer `fallback_ms: None` 导致 stale timer 无法 re-arm，任务永久阻塞

- **根因**: `wait_event_impl` 的 I/O fallback 路径（1ms 安全网）调用 `wait_with_timeout()` arm timer，但该函数始终创建 `TimerAction::WakeTask { fallback_ms: None }`。`run_timer()` 中 stale fallback re-arm 逻辑只在 `fallback_ms: Some(ms)` 时生效。当 fallback timer 到期后任务重新阻塞，旧 timer 因 generation 不匹配被静默丢弃 → 任务永久阻塞，ready_queue 为空，scheduler trace 0 条目但系统不 crash。
- **症状**: 多进程通过 pipe 相互唤醒的 benchmark（如 lmbench context switch 测试）运行到某个点后整个系统挂死，不 panic、不 trace abort、不响应，但 trace 触发仍可工作，调度 trace 条目为 0。
- **修复**: I/O fallback timer 必须直接调用 `add_kernel_timer(TimerAction::WakeTask { fallback_ms: Some(ms) })`，不要复用 `wait_with_timeout()`（后者生成 `fallback_ms: None`，专用于有 deadline 的单次超时）。
- **机器差异原因**: 快/慢主机（或 KVM vs TCG）的 pipe I/O 时序差异决定任务是否在 1ms fallback 到期前收到数据，从而命中或绕过重新阻塞的竞态窗口 → 同一 Docker 镜像在不同机器表现不同。
- **排障线索**: (1) 系统不 crash 但无 task 切换 → 检查 ready_queue 是否为空；(2) trace 0 条目 → 非活锁，纯空闲阻塞；(3) 机器相关 → 很可能时序/竞态 bug；(4) 检查是否所有任务都在 interruptible 等待状态。
- **相关文件**: `os/src/task/manager.rs`

## bind mount 根 ".." 解析跨越挂载边界逃逸到源文件系统

- **根因**: `MountFSInode::find("..")` 通过 `do_find()` 直接调用 `inner_inode.find("..")`，在 bind mount 根上穿透 mount 边界回到源 FS 的父目录。该 inode 编号与 VFS 全局根（通过 `current_root_inode()` 获取）不同，导致依赖 inode 一致性判断根位置的调用者（如 musl getcwd manual walk）误判未到根。
- **症状**: musl `getcwd()` 报 "cannot access parent directories: Invalid argument"（`EINVAL`）；`fstatat("/")` 与 `fstatat("..")` 从根子目录返回不同 inode。
- **修复**: 在 `do_find()` 开头 special-case `name == ".."`，调用 `lookup_dotdot()`：挂载点根时先拿到 `self_mountpoint`（在父 FS 中的 backref），再对该 backref 的 `inner_inode.find("..")` 求值 → 结果在父 FS 的 MountFS 上下文中。全局根返回自身。普通目录走原路径。
- **教训**: VFS mount-boundary 穿越需要区分两个方向 — (1) 正向：`overlaid_inode()` 覆盖子挂载；(2) 反向 ".."：需通过 `self_mountpoint` backref 回到挂载点所在父 FS。不能用同一个 `inner_inode.find("..")` 处理这两种语义；`do_parent()`（服务于 `absolute_path()`）有不同需求，不应混用。
- **相关文件**: `os/src/fs/vfs/mount.rs` (`do_find`, `lookup_dotdot`, `do_parent`)

---

## Batch I/O 连续性假设破裂 — cache eviction 空洞导致数据错页

- **根因**: `sync_batch_read_pages()` 在收集 pending pages 时跳过已缓存的页（`continue`），但后续调用 `backend.read_pages(start, &bufs)` 时假设所有 pending 索引连续。当 clock eviction 或其他回收机制在 page cache entries 中留下 `None` 空洞后，pending 集合可能变成 `[N, N+2, N+5]` 这样非连续序列。后端 `read_pages(start, bufs)` 把 `bufs[i]` 解释为 `start + i` 的磁盘内容 → 第 i+1 页得到错误数据。

- **症状**: 文件读取返回错位数据；executable pages 被垃圾数据覆盖 → `InstructionNonDefined`（la64）/ `IllegalInstruction` 异常，所有通过该页执行的子命令反复在同一 VA 崩溃。

- **修复**: 将 pending 按索引连续性拆成多个 run，每个 run 独立调用 `backend.read_pages(run_start, &run_bufs)`。用 `while` 循环扫描 pending，当 `pending[i].index != run_start + run_bufs.len()` 时结束当前 run 并开启新 run。

- **教训**: 
  1. 任何"收集候选 → 跳过部分 → 批量操作"的模式，必须在批操作前**验证连续性**（即使是"调用者保证"也需要在 callee 中加 debug_assert）
  2. 回收子系统（eviction/reclaim）和预取子系统（read-ahead）的交互是**跨 commit** 的潜在 bug 源 — 单看每个 commit 正确，合在一起触发空洞
  3. 双架构测试不能替代架构针对性测试 — la64 和 rv64 的 eviction 模式不同，rv64 正常不代表 la64 也正常

- **相关文件**: `os/src/fs/page_cache.rs`（`sync_batch_read_pages`、`evict_clean_pages_clock`）
## ext4 可变长目录项：先画 `rec_len` 算术，再判定 rename 根因

- **反例审计**: 不要从“调整 rename 顺序后测试通过”直接倒推出“旧删除跨过 slack 中的新项”。
  设旧记录总长为 `R`、实际占用为 `S`，插入新项后布局是 `S + (R-S)`；再删除这个非块首
  旧记录时，前驱只增加 `S`，其新边界恰好等于新项起点，并不会越过新项。有效干预只能证明
  改动与结果相关，不能替代记录区间的算术证明。
- **已锁定机制**: 块内首记录没有前驱。若搜索结果用 `prev_offset=0` 表示“无前驱”，删除路径
  却仍从偏移 0 读取前驱 `rec_len`，就会把记录自身长度加回自身，使 `R -> 2R`，下一次扫描
  直接跳过后继。块首删除必须保留原 `rec_len`、只清 inode/body；只有非块首记录才合并到
  紧邻前驱。
- **身份约束**: ext4 metadata checksum 的 inode 输入是目录自身 inode，而不是块内第一条
  目录项的 inode；第一条记录可能是普通孩子，也可能已经被清空。
- **事务约束**: rename 的先移除源、覆盖目标处理、发布目标、失败回滚和链接计数延后仍是
  有价值的运行期事务加固，但应与目录 framing 根因分开陈述；无 journal 时仍不具备掉电原子性。
- **测试要求**: 同时覆盖块首/非块首删除、空目标/覆盖目标、成功后的 namespace/content、
  重挂载与 checksum；源码里存在回归函数不等于日志已证明它被单独执行。
- **相关文件**: `os/src/fs/ext4/direntry.rs`、`os/src/fs/ext4/ext4fs.rs`、`os/src/fs/ext4/test.rs`

## Make 包装目标必须验证 feature 已传到最终 cargo 命令

- **现象**: `make ... EXTRA_FEATURES=perf_diag` 成功退出，生成的内核却没有诊断节点；生产版与诊断版运行行为近似，容易被误当成“探针零开销”。
- **根因**: 顶层 Make 目标接收了变量，但调用架构子 Make 时没有显式转发；参数在 wrapper 层被静默丢弃，编译成功只能证明生产配置可构建。
- **修复**: 所有通用 build/all 包装目标显式传递 `EXTRA_FEATURES="$(EXTRA_FEATURES)"`；诊断构建完成后读取 `/sys/kernel/stats/features`，并用计数器非零自检确认 feature 真正生效。
- **教训**: A/B 构建不能只比较命令行和退出码。应把“构建变量 → 最终 cargo feature → 目标运行时接口”串成三段证据链，否则探针税结论没有意义。
- **相关文件**: `os/Makefile`, `scripts/diag_smoke_test.sh`

## A/B rerun 前必须核对两侧内核构建指纹一致（后续构建会静默覆盖内核）

- **现象**: 首次 5+5 验收双侧公平（各日志均含 `perf_diag features: perf_stats=true perf_diag=true`），但随后补跑的 rerun 数据出现不对称：rerun-baseline 从 `/tmp/read-batch-baseline/os` 运行（内核含 perf_stats），rerun-candidate 却从 `/app/os` 运行（内核被 12:02 未带 `EXTRA_FEATURES` 的 counter 构建覆盖，已无 perf_stats）。用不对称 rerun 推导 candidate 对比结论会失真。
- **根因**: `/app` 与工作树是同一份产物目录；后续任意一次不带 `EXTRA_FEATURES` 的内核构建都会覆盖 `kernel-rv`，而 rerun 脚本从 `/app/os` 直接启动，拿到的是被覆盖后的内核。
- **修复**: 每次 rerun/补测前，`md5sum $(PRODUCT_ROOT)/kernel/kernel-rv` + `strings kernel-rv | grep -c "perf_diag features"` 核对两侧指纹与首次验收一致（基线 `2e2632af`=7 匹配，无 feature 内核=0，`perf_stats` 单 feature 内核=0，`perf_diag` 内核=1）。`EXTRA_FEATURES=perf_stats` 单独不足，`/sys/kernel/stats` 由 `#[cfg(feature="perf_diag")]` 门控（`os/src/fs/sysfs/files/mod.rs`），需传 `EXTRA_FEATURES=perf_diag`（`Cargo.toml` 中 `perf_diag=["perf_stats"]`）。
- **教训**: 跨时段补测不是同一实验。验收数据与 rerun 数据必须各自校验构建指纹，不能混用；"有 perf_diag 字符串"比"能构建成功"更接近真实运行特征。
- **相关文件**: `os/src/fs/sysfs/files/mod.rs`, `os/make/rv64.mk`

## 诊断开关税低不等于诊断构建与生产构建结构等价

- **现象**: 同一诊断二进制内 `stats_on=0/1` 差异低于 1%--2%，但相邻 production/diag-off 在高频陷阱 workload 上仍可能相差数十个百分点；普通用户态负对照却保持稳定。
- **根因类型**: 增加 feature 会改变 `.text` 大小、函数地址、trap/uaccess/page-fault 布局和链接结果。数百万次用户/内核往返可把代码/缓存布局差异放大到用户时间，即使计数器分支本身几乎没有运行时税。
- **门禁**: 探针报告必须分开两项：一是同一诊断构建内的 `stats_on=0/1` 运行时税；二是相邻 production/diag-off 的结构税。后者至少包含一个事件密集 workload 和一个事件稀少负对照，并固定 image、feature、initramfs、suite/runtime 哈希和文件系统路径。
- **证据边界**: 没有 PMU cache-miss 或 PC histogram 时，只能写“代码/缓存布局敏感为高概率”，不能指定具体 L1/L2 conflict。若结构门禁失败，诊断事件数和 handler 时间仍可用于机制归因，但诊断绝对 wall/user 时间不得替代 production。
- **相关文件**: `scripts/kernel_perf.py`, `docs/09_debug/python-performance-checkpoint-20260716.md`

## 串口 benchmark 事件必须有目标端副本和可恢复字段

- **现象**: workload 已 PASS，串口上的单行 JSON 却因繁忙输出丢失少数字符，宿主 analyzer 缺一条 sample；只依赖串口文本会把有效的数小时矩阵降级为不可用。
- **设计**: runner 在目标文件系统同步写 JSONL，再向串口输出同一事件；事件至少同时保存 benchmark、sample、elapsed_ns、user/sys、result token，末尾 summary 另存 median/min/max/sample count。宿主保留原始串口，不原地修复；只有完整 summary、关键 sample 字段和 rc/PASS 同时存在时，才允许生成带 `reconstructed` 标记的派生行。
- **介质约束**: target JSONL 应放在本次明确允许写入且已校验的测试目录；正式 ext4 结论中 suite、pycache、tmp、I/O payload 和事件副本都必须位于 ext4，不能只把脚本放在 ext4、数据仍落到 tmpfs/FAT32。
- **教训**: analyzer 的外层 wall 包含解释器启动、import、预热和串口控制，不得替代 workload 自报 elapsed。原始日志、target JSONL、派生 CSV 的证据等级必须在报告中显式区分。
- **相关文件**: `user/tools/cpython/bench/bench_runner.py`, `scripts/kernel_perf.py`, `scripts/run_cpython_bench_matrix.py`

## 自举运行时部署不要用待替换运行时解包自身

- **现象**: 在慢内核/VFS 上用旧 Python `tarfile` 解压新的完整 Python runtime，宿主超时后板端仍卡在不可中断的前台任务；改用原生 tar 后，又可能因 archive 显式包含根成员 `./` 而在最小 VFS 上失败。
- **设计**: 宿主先验证 archive 成员不含绝对路径、`..` 或链接逃逸，再传输并让板端校验 SHA-256；板端使用已有 BusyBox/native tar+xz 解包到同一文件系统的隐藏 staging，执行 runtime smoke，`sync` 后原子 rename 发布。确定性打包使用排序文件清单并省略合成根成员，只包含根下真实成员。
- **证据要求**: 规范化前后不能只比 archive 总哈希；应对路径归一化后的逐成员 type/mode/uid/gid/size/link/content 做无序比较，并保存 runtime 内部 manifest 哈希。部署 manifest 必须记录实际发布的 archive，而不是首次失败的构建包。
- **介质边界**: staging、canonical runtime、work、pycache 和结果必须全部落在本轮允许写入的目标分区；旧只读 runtime 只提供下载/解包工具时，也不得因此把目标分区误写成它所在的分区。
- **相关文件**: `scripts/build_cpython_runtime_la64_strict.sh`, `scripts/deploy_cpython_runtime.py`, `user/tools/cpython/strict_runtime_smoke.sh`

## 动态语言运行时替换必须关闭全部间接回退路径

- **场景**: 新解释器已经部署到持久分区，直接执行 `python3` 也命中新版本，但 pip、console
  script、chroot、`subprocess` 或启动期功能测试仍可能通过旧 shebang、继承的 `PATH`/
  `LD_LIBRARY_PATH`、旧用户目录别名或部署 bootstrap 间接执行退役运行时。只替换一个
  `/usr/bin/python3` 符号链接不能证明运行时已经完成切换。
- **设计**: 同时关闭五个面：①解释器入口固定指向验证 wrapper；②pip 和全部 console entry
  忽略 shebang，通过同一 wrapper 执行；③wrapper 覆盖子进程的 PATH/动态库路径，不继承退役
  分区；④chroot 只 bind 新 runtime 与新状态树，并复制相同 wrapper/profile；⑤部署只用基础
  BusyBox 解包和验 SHA，smoke 只执行刚部署的新 runtime。任何一环缺失都保留了隐式 fallback。
- **发布协议**: runtime 按 artifact hash 放入不可变 `releases/<id>`，manifest 与激活标记绑定
  artifact/ELF hash，staging smoke 通过后原子更新 `current`。代码、用户包、pyc、tmp 和测试
  输出放在同一目标文件系统；旧分区只作只读数据备份。
- **门禁**: 缺失或无效的 `current` 必须让默认命令 fail-closed，不能让 shell 继续搜索旧路径。
  验证应从默认命令启动，检查 `sys.executable/sys.prefix/sys.path/PATH/LD_LIBRARY_PATH`，并单独
  验证 pip、典型 console entry、chroot 和 subprocess。包依赖失败可以记录为“新问题已暴露”，
  但不能改回旧解释器把门禁做绿。
- **教训**: “新运行时能执行”与“系统只会执行新运行时”是两种不同结论。后者需要从命令解析、
  shebang、环境继承、文件系统视图和部署供应链给出闭包证据。
- **动态链接器闭包**: wrapper 显式启动 P4 loader 仍不够，`sys.executable`、multiprocessing 和
  pip build isolation 会直接 `execve` Python ELF。动态可执行文件的 `PT_INTERP` 必须固化到
  稳定的 P4 `current` loader，并由 artifact manifest、安装器和板端激活前全 ELF 复核共同门禁。
  `patchelf --set-interpreter` 对已绑定 ELF 未必字节幂等；打包脚本必须先读现值、仅在不同时
  改写，否则仅重复打包就可能改变 ELF 布局和 artifact hash。测试准备若操作 tracked ELF，
  还要同时核对 interpreter、RPATH 内容和 `DT_RPATH`/`DT_RUNPATH` 类型；确定性 runner 应把
  source-before/source-after 不一致视为失败，不能因 QEMU 功能用例通过而忽略污染。
- **环境与 console 闭包**: 除 PATH/库路径外还应清除 `PYTHONPATH/PYTHONHOME/PYTHONSTARTUP`、
  `LD_PRELOAD/LD_AUDIT` 等继承注入。console entry 要解析最终路径并限制在新状态树内；若历史
  安装产生 shell shim + `.real`，应由统一 wrapper 直接解释 `.real`，不可重新信任旧 shebang。
- **完整性成本分层**: 每次启动只检查 manifest/activation/artifact 身份，发布新 release 前用
  新 runtime 对 manifest 中全部 native ELF 做一次实物重哈希。这样可写 P4 release 既不会在
  每个 Python 进程上支付 94 ELF 哈希成本，也不能把被替换的 ELF 原子激活为 canonical runtime。
- **相关文件**: `user/tools/cpython/python3-wrapper-persist.sh`, `user/tools/cpython/python-entry-wrapper.sh`, `user/src/bin/initproc.rs`, `scripts/deploy_cpython_runtime.py`, `scripts/board/verify_persist_python.sh`, `os/make/tools.mk`

## 复杂度缺陷用“实际遍历步数”闭环，不只拟合 wall 曲线

- **场景**: 源码显示外层逐项删除、内层对剩余容器做全表扫描，wall time 看起来呈平方增长，但固定开销、cache、frame free 和 TLB 也会影响时间曲线。
- **设计**: 在现有内层扫描前累加当时容器长度，得到实际 visit/retain steps；同时记录调用数、requested/resident 数、容器初始/最大规模、累计/最大 ticks、错误和有界 size buckets。只在目标 profile + stats_on 窗口启用，不逐事件打印。
- **不变量**: 单个 N 项容器被逐项删除时，理论主扫描量是 `N(N+1)/2`。若 observed 与理论只差一个可解释的小辅助映射，复杂度证据比 `ns/page²` 拟合更强；修复后应先检查扫描步数降为近线性，再解释 wall time。
- **真实影响**: 目标端 runner 必须预热后 reset/on，只包 workload body。比较时同时看累计占比与最大单次时延；calls 很多但每次很小，可能弱于 calls 很少却包含一个大容器的 workload。
- **证据边界**: diagnostic ticks/body 只能作为路径归因，不能直接等同未来优化收益；production/diagnostic 结构差异和探针税仍需独立门禁。
- **相关文件**: `os/src/mm/vma.rs`, `os/src/task/perf.rs`, `scripts/analyze_anon_unmap.py`

## 交叉构建 Python 原生扩展必须同时验证编译命令、wheel tag 和最终 ELF

- **场景**: 目标 CPython 已交叉编译，继续为它构建 Pillow、MarkupSafe 等第三方扩展。
  `setup.py`/setuptools 在宿主 Python 中运行，容易读取宿主 `sysconfig`、头文件、平台名和
  wheel tag；仅设置 `CC=<cross-gcc>` 不能证明产物属于目标环境。
- **构建门禁**: 让目标 CPython header、目标库和目标 sysconfig 先于宿主路径；编译器
  wrapper 在调用参数末尾追加目标 flags，防止包构建系统后写的 CFLAGS 覆盖。保存完整
  compile log/database，逐条要求 target triple/ABI/strict flags，并禁止宿主编译器。
- **产物门禁**: wheel 文件名和内部 `WHEEL` tag 都必须精确匹配目标 ABI（例如
  `cp314-cp314-linux_loongarch64`）；解包后扫描所有 ELF，验证 machine、hash、NEEDED 和
  PT_INTERP，再并入运行时总 manifest。主机 QEMU-user import 只作预检，最终仍需实板
  执行真实功能和目标文件系统 I/O。
- **纯 Python 例外**: 如果选择纯 Python 降低 native 闭包，必须在构建前显式关闭扩展，
  要求 `py3-none-any`，并拒绝编译日志中的任何 C 编译命令和 wheel/安装树中的 ELF。不能
  接受“扩展编译失败后回退成功”，因为它可能留下宿主架构 tag 或不稳定的可选行为。
- **依赖闭包发现**: 从系统默认 console command 一直执行到成功。每次新增包后重新跑默认
  门禁；缺下一个依赖是闭包证据，不应通过旧运行时 fallback 掩盖。最终至少覆盖默认命令、
  包核心 API、运行时全 ELF hash 和既有语言功能矩阵。
- **相关文件**: `scripts/build_cpython_runtime_la64_strict.sh`, `scripts/deploy_cpython_runtime.py`,
  `user/tools/cpython/pillow_strict_smoke.py`, `user/tools/cpython/strict_runtime_smoke.sh`

- **相关文件**: `os/src/fs/vfs/mount.rs` (`do_find`, `lookup_dotdot`, `do_parent`)

## Buffered I/O 必须始终走 PageCache — 禁止 fallback 到 direct I/O

- **根因**: 当 `read_at`/`write_at` 使用 `if let Some(pc) = self.page_cache()` 模式时，PageCache 未创建时 fallback 到直接 open/seek/read/write/close。对于每次 I/O 都触发 open/close 的后端（如 lwext4 的 path-based API），这导致 read 吞吐降为 1/17、write 降为 1/15（vs 走 PageCache 的热路径）。

- **症状**: iozone 4 readers 从 8014 KB/s（走 PageCache）降到 1901 KB/s（fallback direct I/O）；4 writers 从 155 降到 116；单次 1KB write 触发完整 open→journal→write→commit→close 链路。

- **修复**: 用 `ensure_page_cache()`（懒创建）替代 `page_cache()`（只读查询）。I/O 永远路由到 PageCache，后端通过 `read_page`/`write_pages` 做实际 I/O。需配套 `logical_size` 共享计数器防止写回时 1 字节写入被 PageCache 整页写回扩成 4KB 文件。

- **教训**:
  1. 任何 buffered I/O 实现中，`page_cache()` 的 `Option` 返回只应用于 mmap 和内存管理路径；read/write 热路径必须用 `ensure_page_cache()` 确保缓存存在，没有直接 I/O fallback
  2. 写路径的 PageCache 必须配合 shared `logical_size` 跟踪 VFS 文件大小，否则 writeback 按整页粒度写回会破坏 POSIX 文件大小语义
  3. 当后端 I/O 有 per-call 事务开销（如 journal commit）时，仅 batch open/seek/close 不够 — 必须也 batch write 调用本身（write coalescing），否则事务次数 = 页面数而非批次数

- **相关文件**: `os/src/fs/ext4_lwext4/layout.rs`（`read_at`、`write_at`、`ensure_page_cache`、`logical_size_or_refresh`）、`os/src/fs/ext4_lwext4/page_cache.rs`（`LwExt4PageCacheBackend`、`LWEXT4_SIZE_UNKNOWN`、`ensure_size_known`）

## PageCache: logical_size 必须在触发 writeback 前更新（write-ordering）

- **根因**: `write_at()` 路径中 `note_logical_size()` 在 `pc.write()` **之后**调用，但 `pc.write()` 内部会调用 `balance_dirty_pages()` 唤醒 writeback。writeback 后端 `write_pages()` 以 `logical_size` 作为 EOF 夹钳 — 对于新文件 EOF=0，`total_bytes` 被夹钳为 0 → 返回 `Ok(0)` → PageCache 将脏页标记为 clean 但数据未落盘。之后再次写同一文件时 writeback 发现页面已是 clean 不再写，数据永久丢失。

- **症状**: `apk add` 安装的包文件（.so 库等）为 0 字节；`dd` + `chmod` + `mv` 序列后文件数据损坏。文件创建后第一次写入数据被静默吞掉。

- **修复**: 将 `note_logical_size(expected_new_end)` 移到 `pc.write()` **之前**。预发布预期 EOF（`expected_new_end = (offset + actual).max(old_size)`），确保 writeback 看到的 EOF 包含本次写入的数据。若写入部分成功，二次 `note_logical_size(actual_new_end)` 修正（fetch_max 语义下正常为 no-op）。

- **教训**:
  1. **状态更新必须在触发 side-effect 之前**：任何会触发 writeback/reclaim 的操作（`balance_dirty_pages`、`writeback_all`、`evict`）之前，被 writeback 回调依赖的状态（logical_size、inode 元数据等）必须已经反映最新的意图
  2. **fetch_max 语义是双刃剑**：`logical_size` 的 `fetch_max` 在顺序正确时是安全的（单调递增），但无法撤销，因此必须先发布预期值再执行可能有副作用的操作
  3. **writeback 的 Ok(0) 语义陷阱**：`write_pages` 返回 `Ok(0)` 被 PageCache 框架解释为"无数据需写入"并清理脏页标志；当 `logical_size` 为过时值导致夹钳归零时，这是错误的 — 应至少在 dirty pages > 0 但 total_bytes == 0 时记录警告（Safety Net Fix 2）
  4. **rename flush 是独立问题**：rename 前通过 `flush_one` 调用 `writeback_all()` 强制刷页，原实现用 `let _ =` 吞掉所有错误 — 如果 writeback 因 `logical_size=0` 而静默失败，rename 后数据就丢了。修复：错误传播到调用方返回 `EIO`

- **相关文件**: `os/src/fs/ext4_lwext4/layout.rs`（`write_at`、`rename`、`note_logical_size`）、`os/src/fs/ext4_lwext4/page_cache.rs`（`write_pages`）

## PageCache: registry 复用未刷新 backend 的 logical_size 引用（stale EOF 夹钳）

- **根因**: `ensure_page_cache()` 从全局 `fs.page_caches` registry 命中旧 PageCache 时，backend 内部的 `Arc<AtomicUsize>`（`logical_size`）仍指向先前的 `Ext4OSInode` 实例。新 inode 有自己独立的 `logical_size` atom。`write_at` 更新新 atom，writeback 仍读旧 atom → EOF 夹钳在旧大小上，扩展写入数据静默丢失。
- **修复**: registry 命中路径先克隆 `Arc<PageCache>`，释放 registry 锁，然后用当前 inode 的 `fs`/`path`/`logical_size`/`lw_path` 构造新 `LwExt4PageCacheBackend`，调用 `pc.set_backend(backend)` 替换。保留新 inode 的 creation path 不变。
- **教训**: 任何全局 registry 复用带 per-instance 内部引用的对象（`Arc<AtomicUsize>`、`Weak<>`、裸指针等）时，必须检查这些内部引用是否指向当前实例的数据。注册时不只保存对象本身，还要明确哪些字段是 per-instance 的、需要在复用点刷新。解决模式是：从 registry 克隆后先释放锁，再更新需要刷新的内部状态。
- **相关文件**: `os/src/fs/ext4_lwext4/layout.rs`（`ensure_page_cache`）、`os/src/fs/ext4_lwext4/page_cache.rs`（`LwExt4PageCacheBackend`、`logical_size`）

## LTP 驱动的批量语义修复工作流

### 问题特征

面对大量 LTP 失败（1300+ FAIL CASE），逐个修效率极低。需要一套系统化的"定位→分类→批量修复→验证"工作流。

### 工作流范式

```
┌─────────────────────────────────────────────────────┐
│ ① 跑基线：mask=0x800, ltp_suites=syscalls           │
│    输出 output-rv64.txt 作为当前状态快照               │
├─────────────────────────────────────────────────────┤
│ ② 分析：输入基线 log + 失败聚类                        │
│    参考 DragonOS + Linux 6.6 → 伪代码方案              │
│    按修复收益排序（解除最多下游 case 的优先）            │
├─────────────────────────────────────────────────────┤
│ ③ Subagent 分发：                                    │
│    quick: 单文件、纯 errno/常量修正、无新逻辑           │
│    deep:  跨文件、需新增数据结构/重构语义模型            │
│    关键：必须用 subagent，避免主会话上下文膨胀          │
├─────────────────────────────────────────────────────┤
│ ④ 聚焦验证：                                          │
│    ltp_include=case1,case2,... + ltp_runner=inline    │
│    35 秒跑完 → 立即看到修复效果                         │
├─────────────────────────────────────────────────────┤
│ ⑤ 审核：                                              │
│    检查 placeholder/hack、违规文件编辑、配置残留         │
└─────────────────────────────────────────────────────┘
```

### 教训

1. **基线先跑，不要猜**：7/9 log 里的 symlink ENOSYS 在 7/13 就修了——跑基线后才能确认当前真实状态，避免在已修复问题上浪费精力。

2. **umask 这类"全线错误"优先修**：一个错误的 umask 来源导致 384 个 TFAIL——修一处解百处。识别这类"单一根因→大量失败"的模式比逐个修收益高得多。

3. **score=0 ≠ 全部失败**：很多 case 被标记 FAIL 但 LTP 子测试大量 TPASS。例如 chmod01, fchmod01-04 等功能已正确，是 test setup 夹具（权限/路径）问题导致整个 case FAIL。读 TFAIL/TBROK 而非只看 FAIL LTP CASE。

4. **ltp_include 是迭代利器**：`ltp_runner=inline + ltp_include=case1,case2` 可以在 35 秒内验证修复，而不必每次等全量跑几小时。

5. **subagent 是强制要求**：主会话上下文膨胀后修复质量急剧下降。简单到 errno 常量修正、复杂到 11 项 fs.rs 批量修改，一律走 task()。

6. **FileMode::FMODE_PATH 的静默缺失**：`new_without_open()` 不设置 FMODE_PATH 导致 O_PATH fd 不被识别。这类"创建路径不一致"的 bug 很难从失败日志直接定位——需要追踪数据流。

### 基础设施利用

**os_test.conf — 测试控制中心**：mask 控制测试组（`0x800`=LTP only），`ltp_suites` 选子套件，`ltp_include/exclude` 精细控制用例。注入到镜像：

```bash
make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt CONF_FILE=../os_test.conf
```

注入后镜像持久化，QEMU 重启后配置仍在；如需换配置再 inject 一次即可。

**编译与运行分离**：`make rv64-run` 只跑 QEMU 不重新编译——必须先 `rv64-kernel-build-only` 再跑。Docker 内：

```bash
docker exec <container> make -C /app/os rv64-kernel-build-only
docker exec <container> sh -c "cd /app/os && make -f make/rv64.mk comp 2>&1 | tee qemu.log"
```

**perf_diag / syscall tracing**：`/sys/kernel/tracing/tracing_on` 默认开启，每个 syscall 入口写入 ring buffer（2048 entries）。调试单个 case 时：

```bash
echo 0 > /sys/kernel/tracing/tracing_on   # 先停
echo 1 > /sys/kernel/tracing/clear         # 清空
echo 1 > /sys/kernel/tracing/tracing_on    # 开启
./ltp_case                                  # 跑测试
cat /sys/kernel/tracing/trace              # 看追踪
```

Trace 输出显示每个 syscall 的 id、6 个参数、时间戳（µs），ret 事件带返回值和 err 标记。适合定位"哪个 syscall 在返回哪个 errno"这类问题。

**sysfs 统计计数器**：`/sys/kernel/stats/syscall` 提供 syscall 总数和耗时分布，`/sys/kernel/stats/resource` 提供内存/进程/网络/socket/pipe 等全局资源快照。

### 相关文件

- `os/src/syscall/fs/common.rs`（DAC 辅助、路径校验、fd_to_inode）
- `os/src/syscall/fs/sys_*.rs`（76 个按 syscall 拆分文件）
- `os/src/fs/vfs/file.rs`（new_without_open、get_dirent64）
- `os_test.conf`（mask/ltp_include/exclude/suites 配置范式）
- `os/src/fs/sysfs/files/diag.rs`（perf_diag 统计文件注册）
- `os/src/trace.rs`（syscall tracing ring buffer）

## 性能优化失败模式（2026-07-15 经验）

> 本轮尝试了 3 种性能优化方案，全部失败或回退。记录失败原因以防重复踩坑。

### 1. lazy DAC（延迟权限检查）— ❌ 破坏正确性

**思路**：`check_parent_search_access()` 与 `vfs_lookup()` 做双路径遍历。把权限检查移到 vfs_lookup 失败的错误分支中，成功路径只走一次 vfs_lookup。

**失败原因**：对于 `open()` 等操作，vfs_lookup 成功 ≠ DAC 权限检查通过。调用者可能在父目录有读权限但无搜索权限，vfs_lookup 仍能查找到目标 inode（通过 dentry cache 或 ext4 内部遍历），但 Linux 要求此时返回 EACCES。

**教训**：lazy DAC 仅对纯只读元数据操作（stat/fstat）理论安全，但实践中因 vfs_lookup 不区分"可读"与"可搜索"，仍可能漏检。**不推荐用于任何路径**。

### 2. O(n²)→O(n) path walk — ❌ 破坏 mount 穿越

**思路**：`check_parent_search_access()` 每次组件都重建全路径调用 `vfs_lookup`（O(n²)），用 `current.find(name)` 逐级下降（O(n)）。

**失败原因**：`find()` 不处理 mount point 穿越。当路径穿过挂载点时，`find()` 返回原始 inode 而非挂载后文件系统的 root inode，导致权限检查目标错误。

**教训**：所有路径遍历必须走 `vfs_lookup` 或等效的 mount-aware 路径。`IndexNode::find()` 不能替代 VFS 层路径解析。

### 3. fused DAC vfs_lookup — ❌ 无性能收益

**思路**：把 DAC 检查融入 `vfs_lookup` 内部（pre-lookup hook），单次遍历同时完成权限检查和路径解析。

**失败原因**：hook 每次调用 `inode.metadata()` 检查权限，但 vfs_lookup 内部已有 `current.metadata()` 检查目录类型。双重 `metadata()` 调用的开销抵消了消除双遍历的收益。

**教训**：融合方案要成功，必须共用 metadata() 结果——在 vfs_lookup 内部获取 metadata 后同时传给 hook，不能各自独立调用。

### 4. fork+shell 退化诊断 — 需 ring buffer trace

**问题**：fork+exit（+0.8%）和 fork+execve（+2.2%）正常，但 fork+/bin/sh -c 慢 84%（51ms vs 28ms 基线）。

**初步发现**（drift_window + STATS_ON）：shell 启动产生 55 次 write() = 总耗时 73%。但无法确定这 55 次 write 在基线中是否也存在、以及每次 write 的 fd 目标和阻塞来源。

**诊断计划**：需要内核级 ring buffer trace，记录 shell 启动期间每个 write 的 fd、长度、内容 hash、耗时、阻塞次数。配合 `env -i /bin/sh -c true` 和 `>/dev/null 2>&1` 等受控变体定位。

**当前状态**：ring buffer 基础设施未实现，遗留为已知问题。

### 5. 构建/测试基础设施陷阱

| 陷阱 | 教训 |
|------|------|
| `make rv64-run` 不编译内核 | 只跑 QEMU——必须先 `rv64-kernel-build-only` |
| `lang_items.rs` 被 subagent 修改 | 只能编辑 `.rv`/`.la` 变体；`git checkout` 恢复 |
| `os_test.conf` 被 lmbench 配置覆盖 | 每次测试后立即 `git show HEAD:os_test.conf > os_test.conf` |
| QEMU 串口超时截断 tee | 用 `docker exec -d ... > /tmp/qemu_out.log` + `sleep` + `docker cp` |
| `perf_diag` feature 需显式开启 | `EXTRA_FEATURES=perf_diag` 传给 kernel build；`make rv64-run` 不认 |
| drift_window 输出被自身 write() 污染 | `drift_snapshot()` 的 `write(1, ...)` 被计入 post-snapshot 计数器 |
| here-doc 判分器被 QEMU 当作串口输入 | QEMU/make run 显式 `</dev/null`，不要与后续 judge 共用 stdin |
| 直接调用架构 Makefile 丢失统一工具链环境 | 测试 runner 走根 Make facade；不要绕过其派生 `RUSTUP_HOME/CARGO_HOME` 合同 |

## 打点→拆分→抓大头性能优化循环（2026-08-02 全周期经验）

> 一轮完整的多轮性能优化循环（iozone 写/读 + lmbench），沉淀可复用方法论。

### 1. 可信基线必须先于一切优化

- **现象**：早期所有性能数据都带 `EXTRA_FEATURES=perf_diag` 探针，探针原子 RMW 污染热路径 5-20%。基于污染数据的优化决策可能完全错误。
- **修复**：建立 Wave 1 可信基线——无 `perf_diag` 干净构建 + 5 对交替 A/B + 中位数 + min/max + 方差。所有后续优化只对比这个基线。
- **教训**：`diag=0` + 无 perf_diag 是唯一可信测量；探针只用于诊断归因，绝不用于吞吐验收。诊断构建与生产构建必须分开。

### 2. 固定成本微优化几乎必然失败（Wave 2 三个优化全回退）

- **现象**：三个"理论上省一次 Arc clone / 一次锁 / 一次信号检查"的微优化（SEEK_SET fast path、PageCache entry 复用 + 条件 mark_referenced、跳过冗余信号检查）全部未达 ≥5% 门槛，且触发 −6.49% 无关回归。
- **根因**：这些优化移除的是总路径中的固定小成本。profile 显示 lookup 只占读路径 4-12%、lseek 单次 13k ticks 而 write 是 89k——省掉的部分占总成本 <5%，低于测量方差。
- **修复**：动手优化前用 perf_diag 量化"可移除成本占总成本比例"。若 <5-10%，不做。
- **教训**：**凭理论推断"省一次操作"就能提升吞吐是陷阱**。必须是算法级优化（改变复杂度）或主要阶段开销（>25% 的桶）才值得做。

### 3. 止损纪律：成本分散就换目标

- **现象**：random writers 3x 差距，拆完 write 前台发现无单一子桶 ≥25%——最大是 generic syscall residual ~23% + PageCache ~21%，其余都 ≤16%。
- **裁决**：Oracle 止损规则——"若某子桶稳定占 ~25% 以上且与吞吐相关再优化；若成本分散则停止该路径深挖"。成本分散证明该路径没有"大头"可抓。
- **修复**：止损转向更大的目标（lat-pagefault 25x > random writers 3x）。
- **教训**：**不是每个差距都能归因到单一热点**。拆分后成本分散 = 该路径的差距是许多小成本的叠加，逐个优化不划算，应转向余量更大的目标。

### 4. 结构性改善 ≠ 吞吐优化，需要重新定性

- **现象**：metadata 事务合并减少 journal transactions 14→12、flushes 76→68（10.5% 降幅），但 random writers 吞吐只 +0.3~1.5%。
- **根因**：flush 减少对前台吞吐影响小——4MiB 工作集低于 dirty 水位，前台 writer 不等待 writeback 压力；且"flush 次数 ≠ flush 时间"，删掉的 barrier 可能本来就便宜。
- **裁决**：保留但重新定性为"持久化路径效率优化"（减少 barrier/journal 放大，保持 crash-consistency），不宣称吞吐优化。
- **教训**：**优化可能有结构性价值（更少 flush、更小掉电窗口的间接影响）即使吞吐不移动**。区分"吞吐优化"与"结构性/持久化改善"，分别定性，不强行宣称吞吐达标。

### 5. JBD2 flush 拓扑：每 commit 固定 4 phase flush

- **现象**：random writers 诊断显示 76 次 device flush，初看以为是 76 次 commit。
- **修复**：拆分定位——14 transactions × 4 phase flushes（ActiveLog/CommitRecord/Checkpoint/TailUpdate）= 56 + 20 DurabilityBoundary = 76。
- **教训**：**减少 flush 的唯一安全路径是减少 transaction 数量**（JBD2 4-phase barrier 是设计，不能删）。聚合元数据事务（DirectMetadataBarrier → deferred）是可行方向，但需保留 data-before-metadata 顺序 + fsync/sync 语义。

### 6. "flush 更少 = 掉电窗口更小"不成立

- **现象**：合并事务后想宣称"减少持久化窗口"。
- **裁决**：Oracle 纠正——合并可能延后 commit 开始，只能声称减少 barrier/journal 放大并保持 crash-consistency，不能声称缩短 durability window。
- **教训**：**持久化语义的声称必须精确**。crash-consistency 保持 ≠ 窗口缩短。

### 7. 并行优化 agent 改共享文件会互相踩脚

- **现象**：两个 agent 并行实施（一个改 sys_lseek.rs、一个改 page_cache.rs）本应独立，但在 kernel_tests/mod.rs 注册处冲突，导致编译失败和验收缺失。
- **修复**：后续并行优化必须明确**测试文件注册区域（kernel_tests/mod.rs 等）的归属**；PageCache 相关优化合并串行实施（蓝图已预见）。
- **教训**：**并行度受共享文件约束**。改不同源文件可以并行，但共享的测试注册/模块入口必须串行或明确分工。

### 8. 相关文件

- `os/src/syscall/fs/sys_lseek.rs`、`os/src/fs/page_cache.rs`、`os/src/syscall/fs/common.rs`（Wave 2 微优化）
- `dependency/another_ext4/src/ext4/journal_transaction.rs`、`os/src/fs/ext4_another/`（metadata 合并）
- `docs/Work_Log/evidence/2026-08-02/clean-baseline-final-rv64/`（可信基线）
- `docs/Work_Log/evidence/2026-08-02/write-foreground-split-20260802T094340Z/`（止损拆分）

## 测试证据纪律

### 子任务交付的证据完整性

- **根因**: 子任务在完成 QEMU 测试后，仅凭临时容器内的 `/tmp` 日志声明"测试通过"。父 agent 无法验证该日志是否存在、是否对应当前代码版本、是否真正运行至结束。同时，既有测试产物时间戳可能早于最近改动，无法证明结果产自最新代码。
- **教训**:
  1. 容器 `/tmp` 随容器销毁而消失，不可作为证据持久化路径。所有测试日志、退出码、元数据必须写入跨容器可见的工作目录（如 `/app/os/` 下的结果目录）。
  2. 证据元数据是强制项：每次测试交付必须包含完整的执行元数据（commit hash、container id、挂载映射、注入配置、命令、exit status、日志首尾片段）。
  3. 新鲜性检查不可省略：父 agent 必须检查证据文件时间戳是否晚于代码或配置的最后修改时间。仅依赖子任务的"我跑了测试"声明是不够的。
  4. 如果环境限制导致证据不可保留，必须在报告中明确声明缺失哪些字段。
  5. 子任务临时工作区内的结果对父 agent 不可见，不作为有效交付。
- **相关文件**: 跨所有测试场景的通用纪律。

### LTP 用例体通过但清理破坏全局状态仍是 P0 失败

- **现象**: 某 LTP suite 的主体用例全部 TPASS，但清理阶段引入的全局状态破坏（如 `/tmp` 目录隔离被打破、文件系统元数据残留）导致后续所有非关联 suite 大面积失败。仅看 focal case 的 body pass 会误判修复成功。
- **根因**: LTP suite 不是独立用例的孤立集合。前序用例的清理阶段可能修改全局内核状态（如通过 ext4-bound `/tmp` 的写操作污染全局文件系统元数据）。body pass 只说明该用例的核心断言通过，不保证清理阶段无副作用。清理阶段的全局状态破坏属于 P0 级缺陷，因为它使整个测试窗口的后续结果不可信。
- **教训**:
  1. 验证时不能只看 focal suite 的 body 标签。必须检查 suite 完整输出（含 cleanup 阶段），以及后续无关 suite 是否出现非预期失败。
  2. 清理阶段修改了全局状态，不等于前序用例"几乎通过"。任何全局状态损坏都应作为 P0 缺陷记录，优先级高于新增 body pass。
  3. 诊断方向：当特定 feature 的 suite 单个通过但全量 LTP 非关联用例大面积失败时，优先怀疑该 suite 的清理阶段破坏了全局状态，而不是逐个排查后续失败用例。
  4. 隔离策略：对清理阶段有全局破坏风险的测试，应在独立 QEMU 窗口或隔离镜像中运行，不与其他 suite 混跑同一内核实例。
- **相关文件**: `os/src/fs/vfs/mount.rs`（挂载清理路径），通用测试隔离策略。

## 高爆炸半径内核回归编排工作流

### 现象特征

高爆炸半径问题（如 MountFS 生命周期、VFS 路径解析、PageCache 写回路径）通常表现为：
- 单个 LTP suite 的 body 全部 TPASS，但后续非关联 suite 大面积 RED
- 修复假设看似合理（对照 DragonOS/Linux 语义一致），但首轮实现被 Oracle 拒绝
- 拒绝原因涉及证据不可验证、逻辑路径遗漏或全局状态污染而非语义错误

### 可复用编排模式

- **根因**: 线性调试路径（观察 → 假设 → 实现 → 验证）在高爆炸半径问题中效率低。实现者容易在未确认 RED 基线的情况下直接写 patch，或在缺少参考语义的情况下凭直觉判断正确行为。Oracle 的拒绝反馈若不被视为设计信号而被视为失败，会跳过关键的差异清单修正步骤。
- **修复**: 采用多轨并行编排工作流，父 orchestrator 负责调度和合成，不转发子任务结论。
- **工作流拓扑**:

```
┌─ 用户假设 ─────────────────────────────────────────┐
│ "问题在 X 子系统，根因可能是 Y"                      │
└────────────────────┬────────────────────────────────┘
                     │ 分解
                     ▼
┌─────────────────────────────────────────────────────┐
│ ① 级联分解 → 最小有序 LTP 序列                       │
│   先确认 RED 基线，再进入并行轨道                      │
└──────────┬──────────────────────────┬────────────────┘
           │ 并行                       │ 并行
           ▼                            ▼
┌──────────────────────┐   ┌──────────────────────────┐
│ ②-A 本地代码分析     │   │ ②-B DragonOS/Linux 参考   │
│ explore: 扫描实现路径 │   │ librarian: 查阅参考语义    │
└──────────┬───────────┘   └─────────────┬────────────┘
           │ 汇合                         │
           ▼                              ▼
┌─────────────────────────────────────────────────────┐
│ ③ 差异清单：当前行为 vs 期望行为                       │
│   (oracle 审核差异清单完整性)                          │
└────────────────────┬────────────────────────────────┘
                     │ 审核通过
                     ▼
┌─────────────────────────────────────────────────────┐
│ ④ implementation → patch + 测试                      │
│   RED→GREEN 门禁：最小序列全 GREEN 后扩散             │
└────────────────────┬────────────────────────────────┘
                     │ 交付测试结果
                     ▼
┌─────────────────────────────────────────────────────┐
│ ⑤ oracle 验证                                       │
│   - 证据完整性 + 新鲜性                               │
│   - 逻辑完整性与边界条件                               │
│   - 全局副作用评估                                    │
└──────────┬──────────────────────────┬────────────────┘
  拒绝/需修订                         通过
     │                                  │
     ▼                                  ▼
┌──────────────┐             ┌──────────────────────┐
│ 回到③更新     │             │ ⑥ 父 orchestrator    │
│ 差异清单      │             │    合成 → Work_Log    │
└──────────────┘             │    后续任务分离       │
                             └──────────────────────┘
```

- **教训**:
  1. **先确认 RED 再修复**：不确认基线就直接写 patch 是最高频的返工原因。RED 基线也是级联假设的验证手段。
  2. **双轨参考不可省略**：本地实现可能已偏离 DragonOS 或 Linux 语义，单靠阅读当前代码无法发现差异。两条轨道独立产出后在 orchestrator 层交叉验证。
  3. **Oracle 拒绝是节约时间的设计信号**：将拒绝视为"又失败了"会跳过差异清单修正步骤，直接重写 patch 往往重复同一错误。拒绝后必须更新差异清单再进入实现。
  4. **父 orchestrator 不转发子任务结论**：子任务交付的"测试通过"必须经过父级的证据检查、交叉验证和合成才能成为最终结论。转发未经验证的子任务结论等同于放弃质量控制。
   5. **一轮只修一个问题域**：P0 全局状态污染修复后暴露的语义级缺陷应分离为后续任务。混在一起会导致编排焦点丧失和上下文膨胀。
   6. **扩散验证有先后**：最小序列 GREEN 后先跑相邻 suite（如同属 mount 组但测试不同 flag 的用例），再跑跨子系统 suite。跳跃式扩散（如直接从 mount 跳到 mm）会增加归因难度。
- **相关文件**: 通用编排模式，适用于 `os/src/fs/vfs/`、`os/src/fs/ext4/`、`os/src/mm/` 等高爆炸半径模块的回归调试。

## lwext4 inode 复用必须隔离 PageCache

- **现象**：顺序运行 sparse-file LTP 时，首用例通过，后续新建文件在空洞中读到稳定旧值；单独运行后续用例可通过。
- **根因**：`Ext4FileSystem::page_caches` 以 inode number 强引用缓存。unlink 后旧缓存仍保留，lwext4 复用 inode number 创建新文件时，新 inode 命中旧的 fully-valid 页面，绕过正确的 backend hole zero-fill。
- **修复**：regular file 创建成功并取得真实 inode number 后，仅移除该 key 的 registry entry；旧 inode 持有的 `Arc<PageCache>` 继续有效，新 inode 则创建独立 cache。不能在普通 lookup 或 rename 中全局清缓存。
- **教训**：缓存 key 若采用可复用的底层 ID，必须在对象 incarnation 边界解除旧 key→cache 映射；“后续 case 不创建 cache + 单跑通过”比偏移周期更能区分身份污染与底层读取算法错误。
- **相关文件**：`os/src/fs/ext4_lwext4/layout.rs`、`user/src/bin/ltprunner/lwext4_perf/`

## extent 范围删除的起点位于 hole 时不能直接判定为空操作

- **现象**：稀疏文件的 truncate/unlink 在运行期返回成功，inode 和目录项都消失，但
  离线 `e2fsck -fn` 报 block bitmap 多占用；泄漏的物理块属于首 extent 之前有 hole，
  或 range 起点位于两个 extent 之间的文件。
- **根因**：extent binsearch 往往返回“相邻候选”，不保证 query block 被该 extent 覆盖。
  删除器只检查 `from` 是否落在返回 extent 内，若不覆盖就返回成功，会跳过 range 内的
  下一已分配 extent。随后 inode bitmap 已释放，块 bitmap 却仍置位。
- **修复**：若候选 extent 起点大于 `from`，把 `from` 归一化到该起点；若候选结束小于
  `from`，显式查找下一 allocated block，确认仍在 `to` 内后重新构造 extent path。leaf
  remove 和底层 free 的错误必须继续向上传播。
- **门禁**：至少覆盖 leading hole、同 leaf inter-extent hole、跨 leaf hole、next extent
  超出 `to`、after-last no-op；不能只看冷 reopen 数据，还要正常 teardown 后逐镜像 fsck。
- **相关文件**：`dependency/lwext4_rust/c/lwext4/src/ext4_extent.c`、
  `user/src/bin/regression/regression_lwext4_truncate_hole.rs`

## 文件系统测试 PASS 必须包含后端 teardown 与离线一致性

- **现象**：TAP/用户回归全绿且 QEMU 已 halt，但 fixture 的 superblock summary 或 bitmap
  仍是旧值；若 ktest 直接调用 HAL shutdown，lwext4 挂载级 writeback cache 不会因为单个
  inode close 自动提交。
- **根因**：测试断言只覆盖内核内存态；PageCache flush、C block cache、journal stop、
  superblock 更新和设备 detach 是另一条可失败事务。进程 PASS 早于这条事务时，日志会
  给出假绿。
- **修复**：统一执行 PageCache writeback → filesystem sync → 可失败 `on_umount()` →
  backend detach → HAL halt；最终 PASS marker 放在 teardown 成功之后。失败 backend 保持
  Dying 并重试，回调期间不得持 lifecycle registry 锁。
- **门禁**：每轮使用全新 disposable fixture，保留完整串口、QEMU status、最终 marker、
  container/mount 元数据；关机后再运行只读 `e2fsck -fn`。容器 exit 0 不能覆盖 TAP 中的
  semantic FAIL。
- **相关文件**：`os/src/fs/vfs/file_system.rs`、`os/src/fs/vfs/mount.rs`、
  `os/src/kernel_tests/runner.rs`、`os/src/syscall/process/misc.rs`

## journal durability 测试必须同时证明特性与介质顺序

- **现象**：journal 代码、mount/recover API 和测试名称都存在，QEMU 用例也全绿，但 fixture
  实际由 `mke2fs -O ^has_journal` 创建；测试从未进入 journal commit/replay 路径。另一方面，
  仅排空 lwext4 block cache 也会造成假绿，因为数据可能仍停留在 VirtIO/SATA 的 volatile cache。
- **根因**：功能存在性、软件缓存可见性与介质持久性是三层不同断言。没有显式设备 flush 时，
  descriptor、commit、checkpoint 的提交顺序只在 CPU/软件缓存中成立；没有 fixture 特性证明时，
  即使 barrier 代码正确也可能完全未执行。
- **修复**：块设备统一提供可失败 `flush()` 并穿透所有 partition/adapter wrapper；journal 顺序固定为
  records 写入 → flush → commit 写入 → flush → home blocks 写入 → flush → tail advance。恢复时先
  flush replayed home blocks，再清 recovery marker 并二次 flush；所有失败必须阻止 tail/teardown 成功。
- **门禁**：每轮用全新 disposable 镜像；运行日志证明 mount、测试与完整 teardown；`dumpe2fs -h`
  明确出现 `has_journal`；同一镜像至少冷启动两次；最终 `e2fsck -fn` 五阶段干净。正常再挂载不能
  替代事务中途强制断电/故障注入，也不能替代 persistent orphan recovery 测试。
- **相关文件**：`dependency/lwext4_rust/c/lwext4/src/ext4_journal.c`、
  `dependency/lwext4_rust/c/lwext4/src/ext4_blockdev.c`、`os/src/drivers/block/`、
  `os/make/rv64.mk`、`os/make/la64.mk`

## persistent orphan 恢复要覆盖 journal ordering 与 ext4 block-size 边界

- **现象**：unlink 后仍打开的 fd 在运行期工作正常，但掉电后目录项已删除、zero-link inode 和数据块
  永久占用；普通再挂载可能看似成功，只有离线 fsck 报 zero dtime 与 inode/block bitmap 差异。
- **根因**：内存 open count 不是磁盘恢复协议。zero-link 前若未把 inode 加入 on-disk orphan chain，
  journal replay 只能恢复 namespace transaction，无法知道还要 truncate/free 哪个 inode。journal 第一笔
  待 checkpoint transaction 若没有先持久化非零 start pointer，掉电后甚至无法发现已提交事务。
- **修复**：采用 ext4 legacy orphan list（superblock `s_last_orphan` 为链头，inode `i_dtime` 为 next），
  add/del 与 inode/superblock checksum 同事务；mount 在 replay 后、开放写入前清理。清理先 O(n) 预校验
  inode 范围、bitmap/checksum、类型、link count、next/cycle，再逐次删除链头并 truncate/free，避免边恢复
  边发现损坏，也避免每项重新扫描形成 O(n²)。
- **边界**：ext4 superblock 在 4 KiB 文件系统位于逻辑块 0、offset 1024，在 1 KiB 文件系统位于逻辑块 1、
  offset 0；replay 不能硬编码 block 0。若实现仅支持 legacy list，fixture 和生产卷门禁必须显式拒绝
  `orphan_file` incompat feature。
- **门禁**：固定窗口首启强制截断，次启复用原镜像并断言 recovered count、namespace、写探针和 teardown，
  最后只读 fsck；至少覆盖 RV64/LA64 4 KiB，以及一个 1 KiB superblock-location case。
- **性能**：普通 read/write 不受影响；zero-link 多出必要的 inode/superblock journal metadata。mount cleanup
  应保持 O(n)，orphan chain 的 n 是同时存在的 zero-link open inode 数，不是全盘 inode 数。
- **相关文件**：`dependency/lwext4_rust/c/lwext4/src/ext4.c`、
  `dependency/lwext4_rust/c/lwext4/src/ext4_journal.c`、`dependency/lwext4_rust/src/blockdev.rs`

## 阻塞回归应隔离结果通道，并把破坏性探针放在最后

- **问题**: signalfd 阻塞测试若只依赖 child exit status，会把信号唤醒、wait4 状态编码和用户
  copyout 混成一个结论；前置 vfork/CLONE_VM 探针还可能共享调用者地址空间，失败后污染后续
  用例，使根因判断失真。
- **做法**: 用专用 result pipe 传递 child 的业务结果，`wait4` 只负责生命周期回收；不需要状态
  内容时传 NULL。用 ready pipe + 有意延迟证明 consumer 已进入阻塞窗口，再由 watchdog 提供
  有界失败。可能破坏父地址空间的探针固定为 suite 最后一项。
- **判定**: 同时记录 read count、事件字段、elapsed、send 结果、result byte 和 reap 结果。
  不能用“QEMU 没挂”或 child 已退出代替业务 marker。
- **相关文件**: `user/src/bin/regression/regression_signalfd.rs`,
  `user/src/bin/regression/main.rs`

## 生产者 I/O 的可写前缀证明不能变成物理页缓存

- **问题**：read/pread 若在文件对象产生数据后才发现后续用户页不可写，会让文件偏移、pipe
  head 等生产者状态超前；若为避免该问题而预 fault 完整输出区间，又会为 EOF/短读之后的页面
  制造无意义 lazy allocation、CoW 和 TLB shootdown。
- **做法**：在同一 VM 临界区扫描当前已有可写 PTE；只有前缀为空时 fault-in 首页，后续首个
  不可写页截断本轮生产者最大消费长度。锁外执行文件 I/O，实际 copy 时逐页重新验证映射。
  临界区只能带出 VA 描述符和长度，不能带出 PTE、PA、direct-map pointer 或用户页 slice。
- **门禁**：永久跨页用例把第一页末尾设为可写、第二页设为只读，向 pipe 写入跨边界 payload
  后关闭 writer。第一次 read 必须只返回可写前缀，第二次必须读到未消费尾部；关闭 writer 可把
  过量消费确定性转化为 EOF，避免失败用例挂死。
- **教训**：构造期 proof 是瞬时容量上界，不是 pin，也不能替代使用点校验。性能优化迁移时应
  迁移“减少副作用”的意图，而不是照搬与当前 SMP 地址空间生命周期冲突的数据表示。
- **相关文件**：`os/src/mm/uaccess.rs`、`os/src/syscall/fs/common.rs`、
  `user/src/bin/regression/regression_usercopy_pipe.rs`

## 用户态 pseudo-fs 回归必须由专用 PID1 显式挂载

- **问题**：normal PID1 会挂载 `/proc`、`/sys` 等 pseudo-fs，但精简 regression initramfs
  往往使用另一套 PID1。直接加入 `/proc` 用户回归会得到 `ENOENT`，容易被误判成 proc 节点或
  open syscall 故障。
- **做法**：在 regression PID1 启动被测进程前创建 mountpoint 并挂载测试依赖的 pseudo-fs，
  输出 mount 结果。不要让单个用例通过内核私有函数绕过 VFS，也不要默认 normal init 的副作用
  自动存在于测试 profile。
- **门禁**：先保留首轮 `ENOENT` 作为环境 RED；修复后日志必须同时出现 mount success、目标
  ABI 业务字段、suite PASS 和稳定源码指纹。挂载失败不能由“其余用例通过”掩盖。
- **相关文件**：`user/src/bin/regression_init.rs`、`user/src/bin/regression/`

## Agent 验证能力由执行 profile 决定，不能由自然语言任务扩权

- **问题**：把“请运行 Docker 构建/QEMU”写进只读审查任务，不会让模型获得测试能力；
  `read-only-review` 仍只有 Read/Glob/Grep。模型即使静态判断补丁可提交，也必须把真实命令
  标为 NOT RUN，不能用文字结论替代退出码。
- **做法**：源码审查使用 `cc-job.py`；需要模型自主选择并归纳 Docker 测试时使用
  `cc-agent-test.py` 的 `agent-docker-validation`，只开放受限 gateway。里程碑矩阵把每个必跑
  recipe 显式列为 `--require-recipe`，并令 `min-runs == max-runs == 必跑项数`。
- **门禁**：最终 PASS 必须核对父 job 状态、每个 child job ID、recipe、真实 exit、suite
  计数、forbidden marker 和 before/after 源码指纹。误派的只读结果保留为 NOT RUN 流程证据，
  不能覆盖后续正确 profile 的实测结果。
- **相关文件**：`cc-codex/bin/cc-job.py`、`cc-codex/bin/cc-agent-test.py`、
  `cc-codex/bin/cc-agent-tool.py`、`cc-codex/protocol/test-recipes.json`

## 并发诊断必须记录实际后端，且不能反向参与正确性协议

- **问题**：只在上层记录“请求精准刷新”会把 firmware/slot/full fallback 混成一个值；把
  mailbox publication 与 consumed bit 强求相等，又会把合法的同类位合并误判成丢中断。
  更危险的是，为了读取诊断而增加 Acquire/Release、handler 计时、普通锁或 panic，使观察
  设施本身改变被观察的 IPI/TLB 时序。
- **做法**：在真正做出 backend 选择、且已经脱离业务锁的发起侧记录互斥分类；目标侧复用
  既有 request/ack 判断完成度。计数器统一 Relaxed，只累计操作数、范围/fanout 和 raw timer
  delta，不持有资源引用。失败诊断必须发生在原有 fail-stop 之前，但不得改变资源泄漏/退休
  顺序；同类 mailbox 位可合并时明确说明 publication-consumption 差值不是正确性断言。
- **门禁**：逐个审计成功、固件错误、doorbell 错误、timeout、stopped、local-only 和无目标
  退出路径，证明每个真实远端操作恰好记一次且本地操作不冒充 shootdown。动态回归复用真实
  并发路径，不为计数器加入会改变生产状态的测试 hook；冻结指纹与协议 marker 仍是验收事实。
- **相关文件**：`os/src/smp.rs`、`os/src/mm/tlb.rs`、`os/src/panic_diag.rs`

## 并发测试优先复用生产 sequence/ack，避免为测试扩张生产状态

- **问题**：bring-up 阶段常用 PING、回包 pending 或测试 ack 快速证明 doorbell；如果功能
  协议已经具备正式 request/ack，这些字段继续留在 `PerCpu` 和 hard handler 中，就会形成
  第二套生命周期、增加 reason 编号与 idle 分支，并且测试只证明探针而没有证明生产路径。
- **做法**：在生产协议稳定后，把 focused test 迁移到真实的 mailbox、sequence、ack 和超时
  入口；测试所需的轮数、结果和 helper 状态只放在 test module。跨 CPU helper 发布结果后，
  还要等待其离开 current 槽再释放 TCB，不能把 `Zombie` 发布误当成已经切离 kernel stack。
- **门禁**：删除测试 reason 前先全仓证明没有生产调用方和汇编/硬件 ABI 依赖；动态测试至少
  覆盖 BSP→AP 单播、BSP→AP 广播和 AP→BSP，并从日志确认目标用例实际执行。验证通过后删除
  旧 handler/idle 分支和 Per-CPU 字段，不保留“以后可能调试”的双轨协议。
- **相关文件**：`os/src/smp.rs`、`os/src/kernel_tests/smp.rs`

## Fork 后共享 inode 状态突变

### 问题特征

fork 后父子进程共享 `Arc<File>`（通过 `FdTable::try_clone()` 克隆 Arc），但 inode 内部存储了 per-process 状态（如 `EventWaitQueue`）。子进程在 fork 后尝试"修正"该状态（如 rebind event queue），实际上突变了父子共享的 inode，导致父进程行为异常。

典型场景：
- signalfd 存储了进程的 `signal_event_queue`，fork 后子进程通过 `rebind_event_queue()` 将其指向自己的队列，但该操作同时改变了父进程的 signalfd。
- 任何在 `Arc<dyn IndexNode>` 中存储 per-process 指针/引用的 inode 类型都可能出现此问题。

### 根因

`FdTable::try_clone()` 克隆 `Arc<File>` 而非深拷贝 inode。这是正确的 POSIX 语义（dup'd fd 共享文件状态），但 inode 内部不应存储 per-process 状态。

### 修复模式

**不要突变共享状态。改为在访问时从当前进程动态解析。**

1. Inode 只存储 per-inode 的不可变元数据（mask、flags 等）。
2. 需要 per-process 状态的访问点（wait queue、event queue）在 `File` 层动态解析：检查 inode 类型，从 `current_task().process` 获取正确状态。
3. `PollWaitQueue` 和 `EventQueueHandle` 支持两种模式：
   - 静态模式：持有 `Arc<dyn IndexNode>` + 指向 inode 内部队列的原始指针（传统路径）。
   - 动态模式：持有 `Option<Arc<EventWaitQueue>>`，通过 Arc 直接访问（无原始指针）。
4. 移除 fork 路径中所有"修正"共享 inode 状态的代码。

### 教训

- 不要在共享 inode 中存储 per-process 状态。
- fork 路径中不应突变任何通过 `Arc<File>` 共享的对象。
- 动态解析（从 current_task 获取）比 fork-time rebind 更安全、更简单。

### 相关文件

- `os/src/fs/vfs/file.rs` — `PollWaitQueue`、`EventQueueHandle`、`File::read_wait_queue/read_event_queue`
- `os/src/syscall/process/signal.rs` — `SignalFd`
- `os/src/syscall/fs/sys_read.rs` — signalfd 阻塞读路径
- `os/src/task/task.rs` — clone 路径（移除了 rebind 循环）

## 驱动/硬件寄存器

### 寄存器命令索引位宽掩码过窄，高位命令被截断

- **现象**: VF2 实板 SD 卡初始化在 ACMD41 探测循环中 CMD55 恒超时（`CommandTimeout(55)`），但 CMD8 正常应答 0x1aa，mask=0x001 的 basic 冒烟通过，掩盖了故障。
- **根因**: DesignWare MMC 的 CMD 寄存器命令索引字段是 bit[5:0]（6 bit，0-63），但 `command_word()` 用 `& 0x1f`（5 bit）编码。CMD55=0x37 被截断为 0x17=23（SET_BLOCK_COUNT），卡收不到真正的 CMD55；CMD41=0x29 同样被截断为 9。CMD8=0x08、CMD17=0x11、CMD24=0x18 均 < 32 不受影响。
- **修复**: 掩码改为 `& 0x3f`；ktest 中同一 bug 的硬编码期望值（`command_word(41, R3) != 9 | ...` 里的 `9` 就是 `41 & 0x1f`）也要一并修正，否则断言在修复后失败。
- **教训**: 写硬件寄存器字段时以寄存器数据手册（或参考固件如 U-Boot）的位宽为准，不要想当然用"够用"的窄掩码。验证时如果测试把 bug 的截断结果当作"正确编码"硬编码进断言，修复生产代码后测试会先于硬件暴露错误——先检查测试断言是否复制了同一个 bug（此处 `41`→`9` 与 `0x3f`→`0x1f` 都是 5-bit 截断的痕迹）。
- **相关文件**: `os/src/drivers/block/dw_mshc/sd.rs`、`os/src/drivers/block/dw_mshc/ktest.rs`

### 分区表解析：GPT 保护性 MBR 被误当成真实 MBR 分区

- **现象**: GPT 盘 LBA0 的保护性 MBR（0x55AA 签名 + type 0xEE 条目）被 MBR 解析器当成真实分区表；解析出"分区1" start_lba=1 指向 GPT 头本身，`/dev/mmcblk0p1` 读出来是 "EFI PART"，mount 报 EINVAL（无文件系统）。
- **根因**: 分区表解析器只支持 MBR，未识别 GPT 的 protective MBR 约定——type 0xEE 条目是"整盘占位"不是真分区。Linux `block/partitions/core.c` 会依次尝试 msdos→efi 解析器。
- **修复**: 检测到 type 0xEE 后读 LBA1 验证 "EFI PART" 签名；有效则解析 GPT 头（分区数组 LBA @72、条目数 @80、条目大小 @84）并发布真实分区（first_lba @32、last_lba @40，last-lba 含末扇区）；无效则回退 MBR 且永不发布 0xEE 条目。
- **教训**: 磁盘格式探测不能只看 0x55AA 签名——GPT 盘必然有保护性 MBR，必须检查分区类型字节 0xEE 并升级到 GPT 解析；否则会把 GPT 头当成分区发布。诊断技巧：dd 读"分区1"若以 "EFI PART" 开头即命中此 bug。
- **相关文件**: `os/src/drivers/block/partition.rs`、`os/src/kernel_tests/block_device.rs`
## 顺序生产者不能重复从分段目标起点扫描

- **根因**：PageCache 按文件页升序产出连续数据，但每一页调用带逻辑 offset 的 `UserBuffer::write_at()` 时，Multi 分支都会从第一个 segment 重新寻找目标位置。页数与用户 segment 数同阶时，复制阶段退化为 O(pages × segments)。
- **修复**：保留随机访问 `write_at()` 的语义；为严格顺序的生产者提供独立 cursor，保存 segment index 和 segment 内偏移，每个 chunk 只向前推进。调用方在整次多页请求前创建 cursor，整次请求只扫描目标 segments 一次。
- **验收**：同时覆盖首尾非页对齐、源页边界与用户 segment 边界错位、空/短 segment、短目标和单 segment；性能 A/B 必须将每日志的 hot pass 聚合后做同编号配对，且 baseline/candidate 除被测开关外使用相同源码与构建输入。
- **相关文件**：`os/src/mm/uaccess.rs`、`os/src/fs/page_cache.rs`、`os/src/fs/ext4_another/inode.rs`、`os/src/kernel_tests/page_cache/user_read.rs`

## 无诊断生产基线的 benchmark runner 可执行性门禁

- **现象**: `diag=0`、不含 `perf_diag` 的生产镜像中，runner 看似完成全部 case，但每项可能是 `exit_code=127` 或因 `/sys/kernel/stats/*` 缺失而被标记失败；QEMU 本身仍可正常退出。
- **根因**: runner 不能把诊断快照当作 workload 的必需前置条件；此外直接调用 raw `exec()` 时，相对程序路径可能缺少 payload 所需的 shell 解析。即使 runner 已改为 shell，测试镜像仍可能缺少 lmbench 二进制，必须与 runner bug 分开归因。
- **修复**: 建立干净 A/B 前先以 payload 中的实际 libc 目录启动一个 runner smoke：确认每个 subtest 的 child exit 为 0、输出可解析、stats 在 clean mode 只是可选元数据。runner 使用 `/bin/sh -c` 执行静态 payload 命令，并把 stats feature 缺失记为 `unavailable` 而非 workload failure；若 shell 报目标文件不存在，先修复测试镜像内容再统计。
- **教训**: QEMU exit 0 或 runner 外层完成都不能证明性能样本有效；只要任一 child 为 127，就必须停止统计，不能把脚本直跑的一次结果混入 runner 基线。诊断 profile 的 runner 验收和无诊断生产 runner 验收必须分开。
- **相关文件**: `user/src/bin/bench_runner/mod.rs`, `user/src/bin/iozone_runner.rs`, `user/src/bin/lmbench_runner.rs`, `docs/Work_Log/evidence/2026-08-02/clean-baseline-20260802T000000Z/`.

## syscall 级微优化对基准吞吐的归因必须先量 syscall 调用次数与单次成本占比

- **现象**: 按 Oracle 建议实现 `sys_lseek` SEEK_SET fast path（fd-table 锁内 `get_file_ref` 借用、去掉 File Arc clone + SeekFrom/`File::lseek` 分发、单次 offset store）后，iozone random writers 五次全新启动 A/B 中位数 musl +0.18%、glibc -1.56%，未达预期 ≥5%，且在基线 ±10% 波动内不可分辨。
- **根因**: perf_diag `syscall_top` 显示 SEEK_SET 确实被调用（`lseek count:8054`），但 `write count:12234 avg:89441 ticks` vs `lseek avg:13283 ticks`——write syscall 成本约为 lseek 的 7 倍；且每个 syscall 的固定脚手架（trap 入口、`task.process.files()` 进程内锁 + fd-table Arc clone、fd-table 自旋锁）都未被 fast path 触及。fast path 只移除了 lseek 中最廉价的部分（≈13k ticks 中的百级 ticks），吞吐影响 <1%。
- **修复**: 对"X syscall 开销是基准差距根因"的假设，动手优化前先用 `perf_diag`（仅看 count 列，不用于吞吐）验证：① 该 syscall 在目标 workload 中确实被调用；② 其单次成本在每记录总成本中的占比；③ 每次调用的固定脚手架成本（进程内锁/表锁/trap）是否已被其他路径覆盖。微优化若只移除廉价部分，吞吐基准不会移动。
- **教训**: 基准归因必须以"每记录成本构成"而非"syscall 次数"为准；微优化验收前先估算可移除成本占总成本的比例。优化方向应从最贵项开始（此处为 `process.files()` 进程内锁与 fd-table 锁），而非 Arc clone。
- **相关文件**: `os/src/syscall/fs/sys_lseek.rs`, `os/src/task/process.rs` (`files()`), `docs/Work_Log/evidence/2026-08-02/lseek-fastpath-rv64/`

## 短并行波诊断必须拆分采样频率并做运行时 schema 握手

- **现象**：宿主机或 guest 的真实并行波只有十几秒，统一 30 秒采样最多捕获一个点；任务已退出或阻塞后，累计 CPU/blocked 时间无法还原波峰为何收缩。更隐蔽的是源码已经增加字段，但 QEMU 仍可能启动旧内核，得到格式合法却缺字段的旧 schema 日志。
- **做法**：scheduler/current/task identity 使用 5 秒轻快照；PageCache、MM、journal、block 和 VirtIO 使用 30 秒重快照。每任务同时保留 user/kernel/blocked/runnable-wait 累计时间、阻塞类别和阻塞 syscall。启动 monitor 前固定 expected schema，首个完整快照必须同时验证版本和关键字段；不匹配就只终止明确绑定 kernel/overlay/PID 的本轮 QEMU。
- **判据**：以 workload begin marker 重置 pre-timed 状态；连续两个轻快照 active≥4，或单点 active≥6 且下一窗口平均忙核≥3.5，才算真实峰值。峰后至少保留 36 个 5 秒快照（3 分钟），目标 60 个（5 分钟）；15 分钟无峰值本身就是前置串行放大的证据。
- **扰动边界**：不逐事件打印；精确等待域使用编译期诊断门控和任务原子字段，正式构建不执行 `current_task()` 克隆。原始日志、pcap、overlay 和镜像永不提交。
- **进程归属门禁**：停止后台 QEMU 时不能只在 `/proc/<pid>/cmdline` 搜索 kernel/overlay token；launcher shell 的 cmdline 同样包含整条 QEMU 命令，先杀 shell 会遗留孤儿 QEMU。遍历进程树时还必须解析 `/proc/<pid>/exe`，只接受 basename 与目标 `qemu-system-*` 完全一致且 kernel/overlay 同时匹配的进程，再发送信号并复查该 PID 已退出。
- **相关文件**：`user/src/bin/init.rs`、`os/src/fs/sysfs/files/diag.rs`、`os/src/task/task.rs`、`scripts/monitor_buildstorm_peak.py`

## MTTCG 下 mailbox pending 不等于 IPI payload 丢失

- **现象**：SMP 压力下 shootdown 等待超时；首次 `send_ipi` 没有报错，目标 CPU 仍在线，超时快照中目标 CPU 的 mailbox reason bit 仍保持 pending，但对应 ack 未推进。
- **判定**：如果 payload/sequence 已经发布、reason bit 仍在且发送 API 无错误，应优先判断为 QEMU MTTCG 对一次性硬件 doorbell 的延迟或合并，而不是覆盖 payload、丢失 request 或目标 CPU 停止。三类故障要用 pending reason、ack sequence、online/stopped mask 和 send error 联合区分。
- **修复模式**：在原 payload 和 request sequence 保持不变期间，只向 `targets & !stopped & !acknowledged` 周期性重发幂等 doorbell；使用子系统独立且有界的超时预算。目标 handler 依据 mailbox bit 和 sequence 幂等执行，全部确认前不得复用 slot、回滚 generation 或提前释放待退休 frame。
- **安全边界**：重试只能修复通知交付，不能掩盖真实 handler 卡死；超出预算仍需 fail-stop 并打印 missing/ack/pending 快照。不要通过不断改写 payload 或无限等待规避同步错误。
- **相关文件**：`os/src/smp.rs`
