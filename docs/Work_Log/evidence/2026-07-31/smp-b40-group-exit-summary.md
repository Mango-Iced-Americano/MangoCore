# B40 跨 CPU 线程组退出证据摘要

## 冻结对象

- 日期：2026-07-31
- 分支：`smp`
- 基线 HEAD：`4b679747afbdd53a7c9c8450bf3bb8e6ca19adba`
- 被测生产 diff SHA-256：
  `4e9da109b605d8eafd84e88cdc38a2a747a566e10a8e2a9b5f09c31baf453e8f`
- 最终任务：`smp-b40-final-validation-v2`
- Runner 状态：`SUCCEEDED/REVIEWED`，六个 child 均 exit 0、未超时，源码前后指纹一致。

最终冻结后仅修改了少量源码注释，并同步本文档、架构文档和 Work Log；可执行逻辑没有变化。
`cc-codex/runtime/` 中的 request、raw log、runner/result JSON 属于本地协作材料，不上传仓库。

## 变更要解决的不变量

1. 发起 group-exit 的 CPU 不得释放仍由远端 CPU 使用的内核栈、用户映射或 TCB。
2. 线程组退出 gate 的关闭与新线程进入成员表/runqueue 必须具有一个共同线性化点。
3. `Running -> Blocking` 与 stop 请求交错时，至少一侧必须负责唤醒或继续退出。
4. live-thread 归零必须表示线程级用户资源和 TLB 清理已经完成，而不只是收到退出请求。
5. trap-return 的 IRQ-off 安全点进入退出路径时，TLB shootdown 等待期间仍必须能处理 IPI。

实现采用：

- 一个私有线程组锁保护成员表和发布 gate；
- 原子退出码供安全点快速读取；
- 不可忽略信号、wake 和 RESCHEDULE 只负责推进 owner；
- 每个 sibling 在自己的安全点执行统一本地清理；
- live token 作为清理完成 ack，最后一个 ack 负责 PCB 收尾。

该协议没有增加新的 `TaskStatus`，也没有引入第二套 owner 真值。

## 官方设计依据

- Linux `kernel/exit.c` 与 `kernel/signal.c`：group-exit 共享最终退出状态并通知线程组，但每个
  线程仍从自身执行流进入 `do_exit()`；多线程 exec 的 `de_thread()` 具有不同的等待语义。
- Linux `kernel/fork.c`：线程创建与退出需要在共同的线程组同步边界上决定是否允许发布。
- DragonOS `kernel/src/process/manager/exit.rs`：当前执行线程完成自身资源退出并转入调度器，
  不由另一个 CPU 远程析构仍在运行的上下文。

参考：

- <https://github.com/torvalds/linux/blob/master/kernel/exit.c>
- <https://github.com/torvalds/linux/blob/master/kernel/signal.c>
- <https://github.com/torvalds/linux/blob/master/kernel/fork.c>
- <https://github.com/DragonOS-Community/DragonOS/blob/master/kernel/src/process/manager/exit.rs>

## Docker 环境

- 容器：`mangocore-smp-integration-20260725-os-dev-1`
- Image ID：
  `sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- Repo digest：
  `zhouzhouyi/os-contest@sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- Image created：`2026-05-10T08:46:16.065707166Z`
- QEMU：RV64/LA64 均为 10.0.2
- 拓扑：`CORE_NUM=8`，运行时 online mask 均为 `0xff`

## 最终串行验证

| 顺序 | Recipe | 结果 | 耗时（约） | 关键证据 |
|---:|---|---|---:|---|
| 1 | RV64 kernel build | PASS | 138 s | exit 0，无 mutation |
| 2 | LA64 kernel build | PASS | 135 s | exit 0，无 mutation |
| 3 | RV64 `KTEST=smp` | PASS | 138 s | 26/26；新 #25 与 STOP #26 PASS |
| 4 | LA64 `KTEST=smp` | PASS | 134 s | 26/26；新 #25 与 STOP #26 PASS |
| 5 | RV64 preliminary `mask=0x003` | PASS | 344 s | 312/314 |
| 6 | LA64 preliminary `mask=0x003` | PASS | 347 s | 308/314 |

初赛明细：

- RV64：basic glibc/musl 均 102/102；busybox glibc/musl 均 54/55，失败仅
  `busybox kill 10` 两项。
- LA64：basic glibc/musl 均 100/102（两套 `test_brk` 各 1/3）；busybox glibc/musl
  均 54/55，失败为两项 `busybox kill 10`。
- 失败集合与 B38/B39 已接受基线一致；无新增 fork/clone/exit、panic、timeout 或非法状态。

## 新 focused 用例的证明范围

`smp::group_exit_stops_remote_sibling` 直接验证：

- CPU1 上的 Running sibling 只能由 CPU1 owner 安全点完成退出；
- 一个已经稳定进入 Blocked 的 sibling 能被 group-exit 唤醒并退出；
- gate 关闭后的 late sibling 返回精确 `EAGAIN`，保持 `New` 且执行计数为零；
- 所有已发布 sibling 成为 Zombie，live count 归零且 PCB 完成进程级收尾；
- STOP 用例仍保持终端测试，排除遗留任务污染停机流程。

该用例没有用 barrier 确定性卡在 `Running -> Blocking` 的单条指令交界。因此这个瞬态目前的
正确性证据是：阻塞登记受 `TASK_MANAGER` 保护，登记完成后释放锁，再 Acquire 复查永久退出码；
退出侧若先观察 Running，则阻塞侧负责自唤醒，若先观察 Blocking/Blocked，则退出侧负责 wake。

## 协作过程与失效证据

- 初轮 `smp-b40-validation` 完成双架构 build 和 focused 26/26，支持主体并发审查。
- 第一次最终任务 `smp-b40-final-validation` 被人工主动终止，不计入 PASS。原因是发现
  `bool::then_some(encoded - 1)` 会无条件求值 `encoded - 1`，零值时可能 debug panic 或
  release 回绕。改成显式零值分支后创建全新的冻结任务，避免接受旧源码上的测试。
- DeepSeek 对最终矩阵作只读执行和总结；GPT 独立核对源码指纹、raw 分数与证明边界。模型把
  “稳定 Blocked”扩大成“覆盖 Blocking 瞬态”的结论已被人工降级，不写入验收结论。

## 尚未覆盖

- `CORE_NUM=1/2/4` 未在 B40 重跑；本节点按风险选择双架构 8 核 focused 与初赛非回归。
- 没有确定性压力测试 clone 发布与 gate 关闭的每一种指令交错；共同组锁提供线性化证明。
- 多线程 `exec` 仍使用旧 sibling 清理思路，不能因 B40 通过而宣称安全；由 B41 单独实现
  “调用者继续、其他 sibling 停止并 ack”的临时会合协议。
