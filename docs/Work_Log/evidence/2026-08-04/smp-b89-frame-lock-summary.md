# B89 单页帧分配锁外清零证据

## 冻结对象

- 基线 HEAD：`6ecbec2ec65e3e45c1425388b895e934f3779d89`
- tracked source diff SHA-256：
  `879b8ceab969239e098eeb3d1aeec293362cd4bed7dad2f9627032f45a8d7c72`
- 执行环境：项目 Docker；`CORE_NUM=8`，`KTEST=smp`
- DeepSeek 设计/补丁审查与完整日志仅保存在本地忽略的
  `cc-codex/runtime/jobs/smp-b89-frame-lock-*/`，不上传 GitHub。

## 所有权与锁边界

旧 `FrameAllocator::alloc()` 在 `FRAME_ALLOCATOR.write()` guard 下同时领取 PPN 并写入
4 KiB 清零，8 核并发缺页会把本可并行的不同物理页写入串行化。新路径为：

```text
write lock -> reserve_one(PPN metadata) -> unlock
           -> FrameReservation::into_tracker(clear if needed)
           -> Arc::new(FrameTracker)
```

`FrameReservation` 是未发布 PPN 的唯一中间 owner。`Option::take()` 在构造 tracker 前
把回收责任移出 reservation，因而消费后的 Drop 是 no-op；未消费 reservation
的 Drop 则回滚 PPN。recycled 与 linker reclaimed 页始终重新清零；只有
`zero_init` 已在 BSP 预清零的 fresh 页跳过写入。失败领取的 perf 计数和
成功计时不包含 `Arc::new()` 都与旧实现一致。

OOM/非 OOM 调用点均先用独立 `let` 语句接住 reservation；该语句结束时
临时 write guard 已析构，所以清零、`Arc` 分配和异常回滚都不会重入该锁。

## AI 审查与人工裁决

1. DeepSeek 设计审查确认 recycled/fresh+zero_init、perf 计时、IRQ 调用域和连续帧
   边界，建议 reservation + Drop 回滚。
2. 其示例未在成功转交后解除 reservation，存在双重回收；GPT 未直接采纳，
   首先引入 `Option<PhysPageNum>` 并在发布后 disarm。
3. 补丁审查进一步指出 `PhysPageNum: Copy` 使 `expect()` 仍保留副本；改用
   `take()` 从类型上表达一次移交。同时采纳 `reserve_one()` 命名，避免与公开
   `frame_reserve(num)` 的“尽力确保余量”语义混淆。
4. 审查对 perf 口径的两个疑问通过直接对照基线 `alloc()` 排除；未为追求表面
   一致而顺带改造连续帧。

## DeepSeek 四项冻结门禁

| 子任务 | 配方 | 结果 | 证据 |
|---|---|---|---|
| `agent-0a6446847b4c-r01-rv64-kernel-build` | RV64 normal build | PASS | exit 0，131.789 s |
| `agent-0a6446847b4c-r02-la64-kernel-build` | LA64 normal build | PASS | exit 0，136.298 s |
| `agent-0a6446847b4c-r03-rv64-ktest` | RV64 8 核 SMP | PASS | 34/34，135.558 s |
| `agent-0a6446847b4c-r04-la64-ktest` | LA64 8 核 SMP | PASS | 34/34，140.366 s |

四项 `source_before/source_after` 一致且 `mutation_detected=false`；无禁止标记、超时或 TAP
缺项。两架构第 24 项的用户页故障是真实 mprotect 降权预期结果。

## 验收边界

B89 证明普通单页 PPN 在锁内唯一领取、锁外初始化和安全发布。它不改变
`frames_alloc*()` 的连续区间所有权，也不审计 Driver DMA、PageCache 或 FS 路径。
