# B83 exec inactive ack 与 noreturn Arc 生命周期证据

- 状态：`pass`
- 基线 HEAD：`6535bf17f3e31747f035633ccce74e50091ebf96`
- 生产代码 diff SHA-256：`788465c908bae96b5cdd849442ecd64ec671c9b72abfa81ec810454d1db23b04`
- Docker container：`mangocore-smp-integration-20260725-os-dev-1`
- QEMU：RV64/LA64 均为 `10.0.2`

## 根因与实现

B82 后的完整 SMP 套件稳定卡在非 leader exec 旧 leader TCB 回收。分阶段
证据确认：owner 已执行、TID 交换已完成、旧 leader 已为 Zombie 且离开 CPU，
但仍有唯一强引用。临时诊断进一步证明它不在 current、runqueue 或 zombie
queue。最终定位到 `run_task_safe_point()`：它先克隆 current `Arc`，然后直接进入
noreturn 退出；context switch 不会展开已废弃的 Rust 栈帧，该 `Arc` 因而永不
析构。

生产修复包含两层：

1. 安全点先统一计算退出码，显式 drop current `Arc` 后才进入 noreturn 退出。
2. exec 不再把 live token 当作“已离开 CPU”。TCB 新增 `exit_inactive`，idle 收尾在
   清空 current 后才发布；`ExecState.pending_inactive` 全部归零后才唤醒 owner。

该顺序对照 [Linux v6.6 `de_thread()`](https://github.com/torvalds/linux/blob/v6.6/fs/exec.c)：
非 leader exec 在交换身份前不仅等待其它线程退出，还必须等待旧 leader 不再运行。

## DeepSeek 协作与人工裁决

- 首轮冻结验证确认 RV64 精确失败阶段为“旧 leader TCB 未回收”。
- DeepSeek 某次只读分析超时，未把模型推测写成证据；原始 runner 日志仍保留。
- GPT/Codex 根据 `strong_count=1` 且三类调度容器均为空的证据审计 noreturn
  调用链，定位并修复安全点局部 `Arc`。
- DeepSeek 最终报告误称 TID 替换发生在 `finish_switch_out()`；人工按源码纠正为：
  该处只发布 inactive ack，TID/registry 事务由 exec owner 醒来后执行。

DeepSeek prompt、manifest 和完整 stdout/stderr 仅保存在本地忽略的 `cc-codex/`，
不纳入 Git 或上传。

## Docker/QEMU 验证

1. RV64 `CORE_NUM=8 KTEST=smp KREPEAT=1`
   - job：`smp-b83-exec-arc-fix-r1`
   - child：`agent-94f3a499cec8-r01-rv64-ktest`
   - 34/34 PASS，`exec_owner_becomes_group_leader` PASS。
2. LA64 normal kernel build
   - child：`agent-98ec771f60c0-r01-la64-kernel-build`
   - 退出码 0，约 138 秒，mutation false。
3. LA64 `CORE_NUM=8 KTEST=smp KREPEAT=1`
   - child：`agent-98ec771f60c0-r02-la64-ktest`
   - 34/34 PASS，`exec_owner_becomes_group_leader` PASS。

最终两架构都没有 panic、timeout、fatal trap 或 stale-TLB marker，源码
`mutation_detected=false`。诊断用计数和打印已在最终验证前删除。
