# develop Batch 7 procfs CPU 拓扑证据

## 协议选择

MangoCore 的 `configured_cpu_count()` 是构建期 1/2/4/8，且 BSP 在进入用户态前要求
`online_cpu_mask == expected_online_mask`；缺少 AP 会超时 panic，不存在部分上线继续启动。因此
procfs 使用 configured 逻辑 CPU 编号，和 `getcpu()`、affinity、runqueue 使用同一命名空间。

Linux 官方 procfs 文档规定 `/proc/stat` 首行 `cpu` 是全部 `cpuN` 行的汇总，CPU 时间字段单位
为 USER_HZ。本批只补全 topology，时间字段仍为 0；没有把 timer IRQ、调度次数或 wall time
混入不同量纲的 CPU 时间。

## 只读审查

DeepSeek 任务 `develop-batch7-proc-cpu-review-r1-20260803` 状态 `SUCCEEDED/REVIEWED`，确认：

- configured/online 选择符合启动门禁，QEMU 1/2/4/8 与 2K1000 单核路径一致；
- PlatformInfo 在 procfs 挂载前由 BSP 发布，QEMU FDT model 与 2K1000 fallback 生命周期安全；
- `/proc/stat` aggregate/per-CPU 顺序和十字段格式正确；
- 用户回归不会把 aggregate 行误计为 cpuN，4096-byte buffer 足以容纳 8 核输出；
- 无阻塞项；既有动态 btime 与真实 per-CPU USER_HZ 记账应独立处理。

## 首轮环境失败

父任务 `develop-batch7-proc-cpu-validation-r1-20260803` 的 RV64 child
`agent-2f7c3fb07fa7-r01-rv64-regression-8core` 运行 139.528s 后 exit 2。8 核均上线，原七项
通过，但 proc 用例在 open `/proc/cpuinfo` 时得到 `ENOENT(-2)`。根因是 regression 使用独立
`regression_init`，没有执行 normal PID1 的 pseudo-fs 挂载。LA64 在修改前未运行，不能记为通过。

修复为 regression PID1 显式创建 `/proc` 并挂载 procfs；没有让用例绕过 VFS 调内核私有 API。

## 冻结验证

父任务：`develop-batch7-proc-cpu-validation-r2-20260803`

| 架构 | child job | 耗时 | 结果 | online | L4 | proc topology |
|------|-----------|------|------|--------|----|---------------|
| RV64 | `agent-fe7b06cb0045-r01-rv64-regression-8core` | 139.452s | PASS / exit 0 | `0xff` | 8/8 | 8 / 8 |
| LA64 | `agent-fe7b06cb0045-r02-la64-regression-8core` | 141.699s | PASS / exit 0 | `0xff` | 8/8 | 8 / 8 |

两架构都出现：

```text
[regression_init] procfs mount result=0
proc cpu detail: processors=8 stat_cpu_rows=8
[regression_proc_cpu] PASS
```

原 usercopy、mmap、timer、rename、lwext4、signalfd 与最后的 destructive clone probe 全部通过。
两次测试均无 forbidden marker、timeout 或源码变异。

## 源码指纹

- HEAD：`c0d36b40b68db2773add8b2a2ca89a44284d43be`
- status SHA-256：`a189e24edab46f1afad644cd7e814b8a3e4529c93c9d211131471e4319c3c6fa`
- tracked diff SHA-256：`4a270080fe57d0174b1177ea82d3cada404f2c0dbe8f1ead09eabfc805bd2454`
- untracked content SHA-256：`e329f189f13f11cf1eb3982ef87e58e9cd633ccabc3010cc775ea0ebf89e1eeb`

DeepSeek prompt、分析和 Docker/QEMU 原始日志只保存在本地忽略的 `cc-codex/`，不上传 GitHub。
