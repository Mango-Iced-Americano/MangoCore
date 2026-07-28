# SMP B21 kernel-global 撤映射与内核栈回收证据摘要

## 状态

- 阶段：B21
- 日期：2026-07-28
- 基线 commit：`145220d8ba85e5a5eee1a3c0301c69a5a5da0030`
- 工作树状态：源码与文档未提交，等待人工批准
- 最终受测 tracked diff：
  `bf196ab9c03e08a82eea3ed43a27a0ae7e3c9a99fa2b4e3b80c823f2004d1496`
- 本地 `cc-codex` job、模型原始输出和完整 QEMU 日志不纳入 Git；本文件只归档可复核摘要。

## 目标与安全不变量

B21 删除 AP TCB 永久保留 workaround，使动态 kernel-global mapping 可以安全撤销：

1. PTE 清除后、全部目标 CPU 完成 TLB invalidate 前，frame 保持强引用。
2. kernel-stack slot 在 frame 释放与 shootdown ack 之前不得回到 allocator。
3. `KernelStack::drop` 不获取 MM 锁、不等待 IPI、不分配堆内存。
4. handler 按 request snapshot → invalidate → ack 的顺序工作。
5. 等待 ack 时不得持 MM/PTE/runqueue 或其他普通锁，并能处理本地 IPI。
6. publish 不接受 stopped CPU；unmap 可把 terminal STOP 视为不再访问的 ack。

## 实现摘要

- 两架构统一 full kernel TLB invalidation：RV64 `sfence.vma`，LA64 `invtlb 0`。
- MM 撤映射拆成 no-flush detach 与 synchronized retire；frame 由 retired mapping 保持到 ack。
- 固定容量、无堆 kernel-stack retire queue 接住 `Drop`，CPU0 idle 安全点每轮最多回收 16 项。
- init/exec/interpreter 临时 kernel mapping 使用统一同步撤映射入口。
- 删除 `AP_TASK_RETAINED`；zombie TCB 在 idle 栈回收。
- byte address 经 `VirtAddr::floor()` 明确转换为 VPN；LA64 allocator 保持 `id - 1/id + 1` 对称。

## 失败迭代

| 轮次 | 现象 | 根因 | 处理 |
|---|---|---|---|
| build R1 | 3 个旧撤映射调用点编译失败 | API 收口后调用点遗漏 | init/exec/interpreter 全部改用同步入口 |
| focused R1 | 双架构 `AreaNotFound` | kernel-stack byte address 被直接当作 VPN | 显式 `VirtAddr::from(bottom).floor()` |
| focused R2 | 栈回收两轮通过，后续 timer 超时 | shootdown IRQ 窗口接住 one-shot；ktest 不经过 trap-return 安全点 | 测试闭环调用既有 task safe-point；MM 层不执行 timer callback |
| preliminary r1-r3 | 空输出、工具链环境缺失、judge 输入被吞 | runner 缺 `docker exec -i`、绕过 Make facade、QEMU 继承 here-doc stdin | 使用 stdin-capable exec、根 facade，并给 QEMU 显式 `</dev/null` |

这些失败均保留在本地 job 中，不以最终 PASS 覆盖失败历史。

## 最终验证

### Docker normal build

| 架构 | Job | 结果 | 时长 |
|---|---|---:|---:|
| RV64 | `smp-b21-rv64-build-r2-20260728` | PASS | 127.522 s |
| LA64 | `smp-b21-la64-build-r2-20260728` | PASS | 130.145 s |

normal build 后只补充 ktest 对既有 timer safe-point 的显式调用，生产源码未再变化。

### 8 核 SMP focused

配置均为 `CORE_NUM=8 KTEST=smp KREPEAT=2`。

| 架构 | Job | 结果 | 时长 |
|---|---|---:|---:|
| RV64 | `smp-b21-rv64-ktest-r3-20260728` | 27/27 PASS | 134.107 s |
| LA64 | `smp-b21-la64-ktest-r3-20260728` | 27/27 PASS | 132.601 s |

两个 job 均 exit 0、无 timeout、无 forbidden marker、无 source mutation。新用例每轮创建
129 个 AP kernel-only 任务，真实触发 cache overflow，并验证全部 AP ack、TCB 析构、
frame/slot 回收以及第二轮重新映射。

### 8 核初赛 basic + busybox

| 架构 | Job | basic-musl | basic-glibc | busybox-musl | busybox-glibc | 总分 |
|---|---|---:|---:|---:|---:|---:|
| RV64 | `smp-b21-prelim-01-rv64-r4-20260728` | 102/102 | 102/102 | 54/55 | 54/55 | 312/314 |
| LA64 | `smp-b21-prelim-02-la64-r3-20260728` | 100/102 | 100/102 | 54/55 | 54/55 | 308/314 |

- RV64 仅两组既有 `busybox kill 10` 失败。
- LA64 仅两组既有 `test_brk` 1/3 与两组 `busybox kill 10` 失败。
- 两架构 failure multiset 精确匹配既有允许集合；四组均有 START/END、exit code 0，
  online mask 为 `0xff`，无 panic/fatal trap/timeout/source mutation。

## DeepSeek 审查与人工裁决

- 最终冻结源码审查：P0/P1 为 0，建议进入人工验收。
- 初赛独立复核：两架构 PASS，failure multiset 未扩大。
- 采纳：STOP race 区分、等待期间 IPI 可达性、LA64 global invalidation 风险。
- 拒绝/修正：不在 MM 层执行 timer callback；`AreaNotFound` 不是重复入队；init ELF 清理的
  AP 时序描述不准确。模型结论由 GPT/Codex 对照源码和原始结果逐项复核后才写入本摘要。

## 证据边界

本阶段证明动态 kernel-global 撤映射和内核栈回收在当前受控 AP kernel-task 生命周期中闭环，
并证明 8 CPU online 时 CPU0 普通用户路径未退化。以下仍为 NOT RUN / 未实现：

- user MM active CPU mask、generation 与 range shootdown；
- LoongArch MM-owned ASID/epoch rollover；
- 普通用户任务跨 CPU 运行与迁移；
- FS、NET、driver 多核并发；
- 8 核 30 分钟混合压力。
