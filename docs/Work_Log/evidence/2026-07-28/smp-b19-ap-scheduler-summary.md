# SMP B19 AP 本地调度验证摘要

## 受测边界

- Git HEAD：`4177456d831e675a75e621f0e91ed9455dc502c5`（B18）。
- B19 是该 HEAD 上的未提交 tracked diff；本地 `cc-codex` artifacts 被忽略，不上传 GitHub。
- Docker 容器：`mangocore-smp-integration-20260725-os-dev-1`。
- focused 最终受测 `status_sha256`：
  `fde5ab690772adc76436cdec6808afb79d60daba94e0c7339576c106dc6d714a`。
- focused 最终受测 `tracked_diff_sha256`：
  `084a1d55afcb58d32ffe9ae65d0a7810867532e9798e4c78f68d3982877d8a17`。
- RV64/LA64 child 的 before/after 指纹一致，`mutation_detected=false`。

## RED 与根因

首轮 `smp-b19-rv64-ktest-r1-20260728` 进程 exit 0，但内核明确输出
`KTEST RESULT: FAIL`：16 passed、7 failed。第一个失败是远程 kernel task 超时；
之后第二轮所有需要 AP 实时响应的 PING、广播、round-trip、syscall IRQ window 和
terminal STOP 才级联失败。

DeepSeek 冻结只读分析发现，`mm::init()` 只在 CPU0 激活 kernel page table；AP 的
bootstrap 只配置 trap/IPI，从未写本 CPU 的 RV64 `satp` 或 LA64 `PGDH`。AP 早期只访问
恒等映射的 text/data/idle stack，因此 IPI 正常；首次 `__switch` 把 `sp` 换成高虚拟地址
kernel stack 后发生不可恢复故障。GPT/Codex 对照双架构 activate、入口和 switch 汇编后
确认该根因。

最终修复在 scheduler-ready 后、scheduler-entered 前由每个 AP 安装 kernel page table；
对随后新建的动态 kernel stack，CPU0 还必须在入队前发送目标 TLB sync 并等待 ack。

## Docker 串行 focused 验证

| Child job | Recipe / 配置 | 结果 | 用时 |
|---|---|---|---:|
| `smp-b19-rv64-ktest-r2-20260728` | RV64，`CORE_NUM=8 KTEST=smp KREPEAT=2` | 23/23 PASS | 134.696 s |
| `smp-b19-la64-ktest-r1-20260728` | LA64，`CORE_NUM=8 KTEST=smp KREPEAT=2` | 23/23 PASS | 132.978 s |

两者均达到 `configured=8`、`online_mask=0xff`；两轮
`configured_cpus_enter_scheduler`、`scheduler_state_has_unique_owner` 和
`remote_kernel_tasks_run_on_target_cpus` 全部通过，最终 STOP terminal 通过。无 panic、
timeout、forbidden marker、required marker 缺失或源码漂移。

随后对最终源码和同步文档快照串行执行 normal kernel build：

| Child job | Recipe / 配置 | 结果 | 用时 |
|---|---|---|---:|
| `smp-b19-rv64-build-final-20260728` | RV64 normal kernel，`CORE_NUM=8` | PASS，exit 0 | 128.215 s |
| `smp-b19-la64-build-final-20260728` | LA64 normal kernel，`CORE_NUM=8` | PASS，exit 0 | 134.597 s |

两项 `status_sha256` 均为
`efc79a9eea5c8d1b815b4e22c92dce78cf0c0cdab35a1223171ed6d4a97042b4`，
`tracked_diff_sha256` 均为
`47a40d8b97c626810794bb39267b3ec2a7f3e22ed763cac6fdb49595af9e8404`；
before/after 一致，`mutation_detected=false`。

## 证据边界

B19 证明全部 AP 能进入自己的 scheduler，并能被 `RESCHEDULE` 唤醒后各运行一个受控、
短生命周期、kernel-only 任务。它不证明以下能力：

- 普通新任务或 blocked wake 的生产目标选择；
- 用户任务跨 CPU、affinity、迁移或 work stealing；
- AP timer、console、FS、NET、设备和用户 MM 并发安全；
- 用户 MM active mask/range shootdown、LoongArch MM-owned ASID；
- AP 使用过的 kernel stack 解除映射、远端失效完成后的安全 frame/VA 复用。

测试为最后一项临时保留 AP TCB/stack 到 terminal STOP 后关机；这不是通用回收协议。

## 冻结只读审查裁决

`smp-b19-final-review-20260728` 在源码保持冻结时完成，结论为
`ACCEPT_WITH_BOUNDARIES`，无 P0。其 scheduler barrier、TLB 发布链、锁序、lost-wakeup、
TCB-local entry 和双架构 ABI 正向结论经 GPT/Codex 复核后采纳。

报告把 sequence wrap 列为 P1，但其时序计算有误：若 `fetch_add` 返回 `usize::MAX`，
`expected = usize::MAX.wrapping_add(1)` 为 0，现有断言会在发送 IPI 前 fail-stop；不会进入
报告描述的 `MAX >= 1` 提前成功路径。该 finding 未采纳。scheduler-ready 前 sync 在当前
唯一调用图不可达；LA64 `INVTLB op=0x3` 已用官方手册确认清除全部 `G=0` 表项。
