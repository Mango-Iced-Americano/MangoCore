# develop Batch 4 WaitQueue 无损通知证据

## 结论

状态：`partial`。

Batch 4 已把普通 WaitQueue 收敛为“短锁登记/清理、锁外 condition、通知 token 复查”的协议，
并删除 EventWaitQueue 中会在锁冲突时静默丢 wake 的接口。双架构 8 核构建、WaitQueue 重复测试
和初赛功能标记均通过；但初赛 recipe 对四个受版本控制的工具二进制重复执行 `patchelf`，确定性
runner 因 source mutation 正确拒绝整轮，因此本证据不写成 `pass`。

## 基线与环境

- 基线 commit：`857666db`
- 工作树：`MangoCore-smp-integration-20260725`
- 容器：`mangocore-smp-integration-20260725-os-dev-1`
- 镜像：`zhouzhouyi/os-contest:20260510`
- image ID：`sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- repo digest：`sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- RV64/LA64 QEMU：`10.0.2`
- 生产语义测试 diff SHA-256：
  `16c53720feb81b071c9a98a61eb0a8f1ae17e849bc311722eca1c05563e6f36a`

## 设计不变量

1. WaitQueue 锁只保护 waiter 链表，普通 condition 不在该锁下运行。
2. 生产者先以 Release/CAS 领取本轮 `WaitEntry`，再尝试唤醒 Blocking task。
3. 消费者在 context switch 前复查 token、信号、deadline 和生命周期请求；若通知已到达，
   撤销 Blocking，不进入睡眠。
4. condition 可领取 owner 对象，因此 checked block 不重复执行通用 condition。
5. packet socket 在通知前释放 inner 锁；通知路径不新增普通锁嵌套。
6. TaskStatus 继续只表达 CPU/runqueue 所有权，通知边沿不编码为新调度状态。

本机 Linux 6.8 `include/linux/wait.h` 的 `___wait_event` 同样先调用
`prepare_to_wait_event()`，再检查 condition，之后才 schedule；该对照支持本批锁职责拆分。

## DeepSeek 执行结果

父任务：`develop-batch4-validation-r1-20260803`，effort=`max`。

| Recipe | 结果 | 用时/关键证据 |
|---|---|---|
| RV64 kernel build | PASS | 132.069s |
| LA64 kernel build | PASS | 131.369s |
| RV64 8 核 waitqueue ×20 | PASS | 120/120，`online_mask=0xff`，133.572s |
| LA64 8 核 waitqueue ×20 | PASS | 120/120，`online_mask=0xff` |
| RV64 8 核初赛 | 功能 PASS / runner FAIL | QEMU exit 0，marker 完整，无 forbidden marker；检测到 source mutation |
| LA64 8 核初赛 | 功能 PASS / runner FAIL | QEMU exit 0，marker 完整，无 forbidden marker；检测到 source mutation |

两架构初赛均完成 basic 四组，basic 128/128、busybox 118/119；唯一非满分项为既有
`busybox kill 10`。该项不是 runner 失败根因，runner 的直接失败原因是 source mutation。

## mutation 根因与处置

`os/make/tools.mk` 每次准备工具时都会对下列 tracked ELF 重新执行 `patchelf`：

- `user/tools/riscv64/sbin/mke2fs`
- `user/tools/riscv64/sbin/mkfs.ext4`
- `user/tools/loongarch64/sbin/mke2fs`
- `user/tools/loongarch64/sbin/mkfs.ext4`

测试前 source manifest 证明这四个文件没有本地修改，因此测试后只对这四个明确目标执行精确
`git restore --source=HEAD -- <paths>`，没有覆盖其他用户修改。幂等化 `patchelf` 属于测试基础
设施修复，将单独实现、复跑并提交，不与 WaitQueue 生产语义混合。

## 未完成边界

- generic 10ms I/O fallback 仍保留；splice 等剩余 producer 尚未完成事件驱动迁移。
- 本批动态测试证明现实路径与精确 same-queue 用例，不等价于穷举所有 SMP interleaving。
- 初赛整轮的确定性状态保持 `partial`，不得仅依据 QEMU exit 0 改写为 PASS。
