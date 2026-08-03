# B88 帧清零 raw pointer 边界证据

## 冻结对象

- 基线 HEAD：`289b37fe72f2e6194a8ac1166bf5bc78c3bef38c`
- tracked source diff SHA-256：
  `c44c38628f4d14c5da2576bcb721aa23d143a4cbc64ed65d265145ce5ee95330`
- 执行环境：项目 Docker；`CORE_NUM=8`，`KTEST=smp`
- DeepSeek 完整产物保存在本地忽略的
  `cc-codex/runtime/jobs/smp-b88-frame-zero-r1/`，不上传 GitHub。

## 所有权与等价性

`get_dwords_array()` 的唯一调用者是 `FrameTracker::new()`。分配器在持写锁时先从 recycled
栈弹出 PPN 并清除 free bit，或先推进 fresh cursor；到清零开始时该 PPN 已不能被其它 CPU
再次领取，又尚未通过 `Arc<FrameTracker>` 发布，因此当前执行流拥有整页独占权。

物理页首按 4 KiB 对齐，满足 `u64` 对齐。`WORDS_PER_PAGE=512`；既有 8-word 展开覆盖
0..512，尾部循环保持泛化边界。新 raw pointer 与旧 slice 的 `as_mut_ptr()` 指向同一个
direct-map 地址，但不再先制造可逃逸的 `'static mut` 引用。清零算法和代码路径未改变。

## DeepSeek 四项冻结门禁

| 子任务 | 配方 | 结果 | 证据 |
|---|---|---|---|
| `agent-6f4c70d2af93-r01-rv64-kernel-build` | RV64 normal build | PASS | exit 0，137 s |
| `agent-6f4c70d2af93-r02-la64-kernel-build` | LA64 normal build | PASS | exit 0，142 s |
| `agent-6f4c70d2af93-r03-rv64-ktest` | RV64 8 核 SMP | PASS | 34/34，143 s |
| `agent-6f4c70d2af93-r04-la64-ktest` | LA64 8 核 SMP | PASS | 34/34，141 s |

四项 `source_before/source_after` 指纹一致且 `mutation_detected=false`；无 panic、timeout、
fatal marker 或 TAP 缺项。test 24 的架构缺页异常属于 mprotect 门禁预期行为。

## 验收边界

本节点证明 frame-zero 不再依赖安全 `'static mut` helper，并保持原清零语义和性能结构。
它不改变 allocator 写锁当前覆盖 4 KiB 清零的事实，也不审计 MM/PageCache/FS 共用的
`get_bytes_array()`；前者是后续 SMP 锁临界区优化，后者由共享子系统负责人协同处理。
