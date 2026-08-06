# develop Batch 3 WaitQueue 通知 token 核心融合证据

## 1. 范围与基线

- 工作树：`/home/lzm/projects/MangoCore-smp-integration-20260725`
- 分支：`codex/smp-develop-integration`
- 基线 HEAD：`74a5a6787e4db11d8b15006b4289efcfc034714a`
- 受测生产 diff SHA-256：
  `cfad6e72757ebc42c1a834ca7a5db9b08cfc888d4341a6cf0687aefa0300ab28`
- Docker：`a99062375fdbde7b8989f6b9622438229a8609991a3aad86443a5eafcc4acfca`
- `/app` mount：当前集成工作树
- QEMU：RV64/LA64 均为 10.0.2

本批只迁移通用 WaitQueue 的登记级通知协议，不合入 FS/Net/Driver producer 修复，也不删除
generic 10ms I/O fallback。未新增生产源码文件，未扩张 TaskStatus 状态机。

## 2. 设计裁决

旧实现按 wake 时的 TaskStatus 判断是否需要唤醒。若任务已经注册条件队列但仍为 Running，
wake 会被消费；任务随后登记 Blocking 并切走，边沿通知永久丢失。新 `WaitEntry` 将“一轮等待
是否已收到通知”与“任务属于哪个 CPU/runqueue”分离：前者由 CAS token 保存，后者仍只由
TaskStatus 管理。poll/epoll 的多个 queue 共享同一个 entry，因此多个 producer 只有一个通知
赢家；清理先关闭 token，再逐队列摘除，不持两把 queue 锁。

develop 的一次性 waiter-state 方向成立，但其旧 TaskStatus 与当前 SMP 所有权模型不兼容，
因此没有直接 cherry-pick。DeepSeek 首轮建议把私有 `wake_one()` 改为 public 以便测试，最终
拒绝扩大 API，永久测试调用已有公开 `wake_at_most(1)`。

## 3. 最终 focused 门禁

DeepSeek job：`batch3-waitqueue-validation-r2-20260803`。四项严格串行：

| 项目 | CORE_NUM | 结果 | 耗时 | 关键证据 |
|---|---:|---|---:|---|
| RV64 kernel build | 8 | PASS | 131.9s | exit 0，无 mutation/timeout |
| LA64 kernel build | 8 | PASS | 136.5s | exit 0，无 mutation/timeout |
| RV64 `KTEST=waitqueue` | 8 | PASS 5/5 | 136.9s | `online_mask=0xff`，新增 early-wake 用例通过 |
| LA64 `KTEST=waitqueue` | 8 | PASS 5/5 | 135.5s | `online_mask=0xff`，新增 early-wake 用例通过 |

新增用例精确构造“entry 已注册、任务仍为 Running、wake 先到、随后尝试 Blocking”的窗口，
并通过真实 checked-block API证明 token 会撤销 Blocking。没有 test-only 生产字段或临时 IPI。

## 4. 初赛非回归

DeepSeek job：`batch3-waitqueue-preliminary-r1-20260803`。

| 架构 | 进程 | 启动/四组 | judge | 裁决 |
|---|---|---|---|---|
| RV64 | exit 0，341.785s | `online_mask=0xff`，四组完整 | 312/314 | 与基线一致 |
| LA64 | exit 0，354.972s | `online_mask=0xff`，四组完整 | raw 305/314 | semantic 308/314，与基线一致 |

RV64 失败只有两套 busybox `kill 10`。LA64 两套 basic 的 `test_brk` 各为 1/3、两套 busybox
`kill 10` 各失败；musl `test_pipe` 额外 raw 丢 3 分的日志为：

```text
cpid: cpid: 0
64
Write to pipe successfully.
```

两个完整 `cpid` 输出发生字符级交织，pipe 数据和 END 标记正常；这是 B18 已登记过的同一 judge
假阴性，按既有 raw/semantic 双账本恢复 3 分，不新增宽免规则。

两个 preliminary child runner 都因构建覆盖仓库内四个已跟踪 mke2fs/mkfs.ext4 二进制而标记
`mutation_detected=true`。这些文件测试前不在 status 中，测试后已精确恢复到 HEAD；所以初赛
日志只作为功能非回归证据，完整“同指纹 PASS”仍由上一节 focused 四项承担。

## 5. 未覆盖与后续

- 两 CPU 同时对同一个 multi-queue entry wake：NOT RUN；CAS/逐队列锁序已有静态证明。
- deadline timer 在下一轮无 timeout 等待中到达：NOT RUN；generation 失效已有静态证明。
- FS/Net/Driver 所有 producer 通知完整性：NOT RUN，由对应负责人合入后验收。
- generic 10ms I/O fallback 退役：未实施；必须等 producer 审计完成后再删除。
