# B68 futex compare/requeue 原子化证据摘要

## 1. 结论

状态：`pass`

`FUTEX_CMP_REQUEUE` 的 source nofault load/compare 与 wake/requeue 现在由同一
`FutexTable` 临界区串行化。shared 普通 REQUEUE 也在 table 锁内复核 source/target
backing、PTE 和读权限。VM 锁忙或映射改变只能在任何队列副作用发生前产生内部 Retry。

本结论由源码锁协议、DeepSeek 只读复核、双架构 8 核构建、focused futex LTP 和初赛
`mask=0x003` 非回归共同支撑；没有把普通功能回归描述成精确并发交错测试。

## 2. 根因与修复

旧 `FUTEX_CMP_REQUEUE` 先在 syscall 层无锁读取并比较 `*uaddr`，之后才取得 futex table
锁执行 wake/requeue。另一 CPU 可以在比较与队列修改之间改写 source word，使已经失效的
条件仍然搬动 waiter。

最终协议为：

```text
锁外 fault-in source/target，并解析两个 key
  -> 对应 FutexTable 锁
       -> AddressSpace::try_read()
       -> shared 两端 backing/PTE 复核
       -> CMP source nofault load/compare
       -> 同一 table 临界区 wake/requeue
  -> 解锁
```

- VM try-lock 失败、PTE 消失或 backing 改变：队列尚未修改，返回内部 Retry；
- syscall 释放 table 后重新 fault-in，并重新解析两个 key；
- ordinary private REQUEUE 的 source key 是当前 MM+VA，不新增 source PTE 要求；
- CMP private 必须读取 source，因此仍在锁内复核 source PTE；
- target 保留改动前已经存在的可读映射校验；
- 删除旧 `FutexTable::requeue()`，避免绕过受检入口。

## 3. 官方实现对照

- Linux 6.6 `kernel/futex/requeue.c`：解析 key、锁定 hash bucket 后，以 nofault 方式读取并
  比较 source，再在相同 bucket 锁域内 wake/requeue。
- Linux 6.6 `kernel/futex/core.c`：private futex key 使用当前 MM 与用户 VA；shared key 才
  需要解析实际共享 backing。

参考：

- <https://github.com/torvalds/linux/blob/v6.6/kernel/futex/requeue.c>
- <https://github.com/torvalds/linux/blob/v6.6/kernel/futex/core.c>

MangoCore 没有照搬 Linux 的 bucket/fault retry 细节，而是复用既有的
`FutexTable -> AddressSpace::try_read()` 条件式非阻塞锁边，避免持自旋锁等待 VM 锁。

## 4. AI 协作与裁决

| Job | 结果 | 采纳边界 |
|---|---|---|
| `smp-b68-cmp-requeue-review-r1` | reviewed | 确认锁外 compare/requeue 窗口，建议锁外 fault-in、锁内 nofault compare+mutation |
| `smp-b68-final-review-r1` | canceled | 提交审查后源码又做可维护性重构，主动取消，不计证据 |
| `smp-b68-final-review-r2` | reviewed | 最终 diff 五项不变量通过，源码未被模型修改 |
| `smp-b68-validation-r1` | canceled | 双架构 build 后发现首稿错误要求 ordinary private source PTE，主动取消，不计 PASS |
| `smp-b68-validation-r2` | reviewed/pass | 修正后的冻结源码完成六项串行 Docker 门禁 |

GPT/Codex 对模型输出做了两项关键纠正：

1. private futex key 不解析 VMA/PTE，ordinary private REQUEUE 不应因统一流程而被收紧；
2. futex wake syscall 本身不负责写用户 word；通常是用户态先 store 再 wake，不能把模型的
   简化表述写成内核事实。

`requeue_waiters()` 中 map 可能分配内存是既有实现边界，不在 B68 冒充已解决。

## 5. 冻结源码

```text
base HEAD: 8edead8142dea0d18aad19c445f4428ea8327593
tracked diff SHA-256: 13911e538decf8617d9d6bf02b5a60255e5c272f07d3fe7023e4611ef7575814
```

`smp-b68-validation-r2` 的六个 child job 均记录相同 source-before/source-after：HEAD、status
hash、tracked diff hash 和 untracked hash 全部一致，`mutation_detected=false`。

## 6. Docker 验证

| Run | Recipe | CORE_NUM | 结果 | 时长 |
|---|---|---:|---|---:|
| R01 | `rv64-kernel-build` | 8 | PASS, exit 0 | 130.7 s |
| R02 | `la64-kernel-build` | 8 | PASS, exit 0 | 134.7 s |
| R03 | `rv64-futex-ltp` | 8 | PASS, exit 0 | 319.0 s |
| R04 | `la64-futex-ltp` | 8 | PASS, exit 0 | 319.3 s |
| R05 | `rv64-preliminary` | 8 | PASS, exit 0 | 346.9 s |
| R06 | `la64-preliminary` | 8 | PASS, exit 0 | 342.8 s |

### focused futex LTP

每个架构分别执行 musl、glibc 各 13 个选择用例，合计：

```text
20 PASS
6 SKIP
0 FAIL
0 BROKEN
```

两套 libc 的 `futex_cmp_requeue02` 均 PASS。六个 SKIP 都是 `futex_waitv01/02/03` 因用例
要求 Linux 5.16，而当前内核向测试报告 5.10；SKIP 不作为 waitv 动态通过证据。

### 初赛 basic + busybox

| 架构 | online mask | 语义得分 | 基线比较 |
|---|---|---:|---|
| RV64 | `0xff` | 312/314 | 精确失败集合未扩大 |
| LA64 | `0xff` | 308/314 | 精确失败集合未扩大 |

原始日志、child `result.json`、parent `runner.json` 与 DeepSeek 汇总仅保存在本地 ignored
`cc-codex/runtime/jobs/`，不提交、不上传。

## 7. 未覆盖边界

以下状态保持 `NOT RUN`：

- 多 waiter 的精确 compare/write/requeue 多核交错；
- VM 锁持续竞争下 Retry 的公平性与性能；
- compare 后并发 unmap/remap 的专项时序；
- file-backed shared futex 遇到 truncate/page-cache backing 替换；
- futex table 内既有 map 分配的独立锁序/OOM 审计。

`futex_cmp_requeue02` 主要验证错误路径和基本 ABI，不能替代上述动态竞态 harness。
