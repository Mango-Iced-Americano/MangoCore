# B54 MM/HAL 单核安全假设收口证据

## 结论

B54 删除了两个真实依赖单核执行的安全假设：LoongArch 恒等映射 dirty side table 不再使用
并发读写的 `static mut [bool]`；slab 不再给内部 raw-pointer 容器授予无必要的 `Sync`。
冻结源码在双架构 8 核构建、SMP focused 和初赛 `mask=0x003` 门禁中均无新增回归。

## 设计裁决

1. dirty 位不承担页表内容发布，只记录恒等映射的软件状态。位图使用 relaxed 原子 load/RMW，
   而映射创建、撤销和生命周期继续由 `KERNEL_SPACE` 锁线性化。
2. `fetch_or`/`fetch_and` 保证多个 CPU 更新同一 atomic word 的不同 bit 时不会互相覆盖；
   `identity_dirty_bit()` 先校验 VPN 上界，再计算 word 与 mask。
3. 全局堆由 `KernelAllocator.inner: Mutex<KernelHeapInner>` 串行化。只需让其内部顶层
   `SlabAllocator` 可跨 CPU 移交，即 `Send`；内部 page/list/cache 和 allocator 均无需 `Sync`。
4. 未机械删除其它 `static mut`：堆后备区、heap_trace 缓冲和每 CPU 静态栈分别已有启动期
   唯一移交、全局锁或不相交 CPU 槽的所有权证明。uaccess 的 `'static mut` 另列高风险节点。

## 冻结源码

- 基线 HEAD：`7bba0086b9bdfb17e64e5ac3deaa3785f26f2007`
- tracked diff SHA-256：
  `575e9d0241b690774770c080ab3f0fa639d2079d37a059e22551829beaa530eb`
- DeepSeek 汇总任务：`smp-b54-mm-unsafe-gate`
- 六个 child 均为 `mutation_detected=false`，被测前后源码指纹一致。

## Docker 门禁

| Child | 配置 | 结果 |
|---|---|---|
| `agent-f85e1c181e4f-r01-rv64-kernel-build` | RV64 normal build | PASS，exit 0，133.5s |
| `agent-f85e1c181e4f-r02-la64-kernel-build` | LA64 normal build | PASS，exit 0，138.2s |
| `agent-f85e1c181e4f-r03-rv64-ktest` | RV64，8 核，`KTEST=smp` | 34/34，`online_mask=0xff` |
| `agent-f85e1c181e4f-r04-la64-ktest` | LA64，8 核，`KTEST=smp` | 34/34，`online_mask=0xff` |
| `agent-f85e1c181e4f-r05-rv64-preliminary` | RV64，8 核，`mask=0x003` | 312/314 |
| `agent-f85e1c181e4f-r06-la64-preliminary` | LA64，8 核，`mask=0x003` | 308/314 |

RV64 只缺 musl/glibc 两项 `busybox kill 10`。LA64 额外保留两套 `test_brk 1/3`，其余同 RV64；
失败身份与 B53/B50 基线完全一致。六项均无 forbidden marker、panic、fatal 或 timeout。

任务模板误把 B53 `KREPEAT=2` 的 67/67 沿用到本次 `KREPEAT=1`。本次原始 TAP 的正确
总数为每架构 34/34；该模板数字未触发无意义重跑，也未被写成虚假证据。
