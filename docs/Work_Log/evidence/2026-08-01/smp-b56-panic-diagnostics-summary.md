# B56 panic 诊断去阻塞与逐 CPU 状态证据

## 结论

B56 修复了 B55 raw console 之后仍残留的两条 panic 自锁链：heap 精确统计和空闲 frame
统计不再无界等待普通 allocator 锁。诊断同时输出全部 configured CPU 的启动、current、
队列、active MM、IPI、timer、TLB 与 membarrier 状态；所有值只作 best-effort 事后观察。

## 修复前的确定阻塞链

```text
panic_diag::print_kernel_memory
  -> heap_stats
     -> HEAP_ALLOCATOR.inner.lock

panic_diag::print_kernel_memory
  -> unallocated_frames
     -> FRAME_ALLOCATOR.read
```

panic 若发生在同 CPU 已持有对应锁时会不可重入自锁；若其它 CPU 持锁后停止，panic CPU
也会永久自旋。接口名称里的“stats/read”不构成无锁证明。

## 生产实现边界

- `try_heap_stats()` 只做一次 heap `try_lock()`；成功时与普通 `heap_stats()` 共用同一计算
  helper，失败时 panic 输出原子 charged/peak/capacity。
- `try_unallocated_frames()` 只做一次 frame allocator `try_read()`；失败打印 `<locked>`。
- local current 和 task.inner 沿用 `try_current_task()` / `try_inner()`。
- `CpuTaskDiagnostics` 读取 current/queue/zombie 原子 hint；active-MM 槽只 `try_lock()` 并复制
  不取 VM 锁的稳定 MM ID，失败输出 `busy=1`。
- `CpuDiagnostics` 读取 online/scheduler/STOP、pending IPI、timer、TLB 与 barrier 原子状态。
- 快照不参与调度、owner、TLB ack 或 frame 释放决策，也没有新增生产热路径写字段。

当前尚无权威 IRQ/preempt depth，输出没有伪造这两项。多个字段也不承诺同一时刻一致；远端
CPU 在 panic 扫描期间仍可能继续执行。

## 冻结源码与本地协作

- 基线 HEAD：`73a0f2106422d3992d303983ff3c0058ac76de35`
- 冻结 tracked diff SHA-256：
  `9238ddcf17be091512e50aae643d4dfaedea6f64770b789c4bf66c1f08608adf`
- 前置只读审查：`smp-b56-panic-audit`
- 验证与复核：`smp-b56-panic-gate`
- DeepSeek 原始 task、analysis、manifest 和 QEMU 日志只保存在本地忽略的 `cc-codex/`，
  不上传 GitHub。

DeepSeek 正确识别两条 P0 阻塞链，并确认最终调用链不等待。GPT/Codex 拒绝其“直接从
panic_diag 访问 allocator 私有实现”的结构建议，也拒绝省略 active MM：当前 exec/TLB
开发需要区分同一 PID 在替换前后的地址空间，非零 MM ID 的收益明确。

## Docker 门禁

- 容器：`mangocore-smp-integration-20260725-os-dev-1`
- Container ID：
  `a99062375fdbde7b8989f6b9622438229a8609991a3aad86443a5eafcc4acfca`
- 挂载：`/home/lzm/projects/MangoCore-smp-integration-20260725 -> /app`

| Child | 配置 | 结果 |
|---|---|---|
| `agent-5e2234856e3e-r01-rv64-ktest` | RV64，8 核，`KTEST=smp` | 34/34，`online_mask=0xff`，exit 0，134.6s |
| `agent-5e2234856e3e-r02-la64-ktest` | LA64，8 核，`KTEST=smp` | 34/34，`online_mask=0xff`，exit 0，137.7s |

两项均无 required marker 缺失、forbidden marker、panic、fatal 或 timeout；before/after
HEAD、status 和 diff 指纹完全一致，`mutation_detected=false`。focused recipe 已包含对应
架构编译，因此没有机械重复 build-only；panic-only 修改不影响用户 ABI，本批也未重复初赛。

## 证据边界

双架构编译、8 CPU 启动和全部既有 SMP 语义已经动态运行。为避免生产代码残留测试 hook，
本批没有注入“取得 heap/frame 锁后主动 panic”；try API 的无等待性质由实际源码调用链和
spin `try_lock`/`try_read` 语义审查证明，不能表述成该故障注入已经动态通过。
