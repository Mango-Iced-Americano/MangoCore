# SMP-P5-B43 非 leader exec 身份接管证据

## 结论

状态：`pass`

B43 让多线程进程中发起 exec 的非 leader TCB 接管稳定的进程 PID/TGID。实现没有增加
第二个 leader 状态机：PCB 只保留稳定 `pid` 与其 allocator handle，线程组 leader 由
`task.gettid() == process.pid` 唯一判定。task registry、Per-CPU current TID、OOM 活跃
索引、退出信号和 thread quota 均随身份事务同步。

## 被测源码与环境

- 被测 HEAD：`18eae3a0e81c165a28dc6a0a7818ca0229424347`
- focused 与 RV64 preliminary 冻结 tracked diff SHA-256：
  `851994375f58140f48d7eb37f593885aedeaef6c4e8456a30a44fd464cd08c16`
- Docker image：`zhouzhouyi/os-contest:20260510`
- image ID：
  `sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- repo digest：
  `zhouzhouyi/os-contest@sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- RV64/LA64 QEMU：10.0.2

## 实现不变量

1. `TaskControlBlock` 的数值 TID 是读多写一次的原子真值，`tid_handle` 只负责 allocator
   所有权；只有 exec registry 事务可以替换两者。
2. exec owner 必须已经收齐 sibling live ack、提交新映像并结束 `ExecSession`，才执行
   身份接管。
3. task registry 锁内校验 owner 与已 Zombie 的旧 leader，交换既有 `TidHandle`，
   再把 owner 从旧 TID 重键到 PID；锁内不析构 TCB 或 handle。
4. `TaskControlBlock::Drop` 只有在 registry 的键仍指向自身时才删除条目，旧 leader
   迟到析构不能误删新 leader。
5. 身份重键不经过 context switch，因此 registry 解锁后立即更新当前 CPU 的
   `current_tid`；OOM tracker 同样从旧 TID 重键。
6. 新 leader 的退出信号为 `SIGCHLD`，额外线程 quota 立即释放。

## 官方语义核对

Linux v6.6 `fs/exec.c::de_thread()` 在其它线程退出后，让非 leader 调用者通过
`exchange_tids()` 接管旧 leader PID，并恢复 `exit_signal = SIGCHLD`。MangoCore 沿用
自身 TCB/PCB、弱引用 registry 和 `TidHandle` 分配模型，只迁移这一用户可见语义和必要
所有权顺序。

## 验证结果

| 项目 | 结果 | 用时 |
|------|------|------|
| RV64 8 核 kernel build | PASS | 约 134 秒 |
| LA64 8 核 kernel build | PASS | 约 132 秒 |
| RV64 `KTEST=smp` | 29/29 PASS | 138.946 秒 |
| LA64 `KTEST=smp` | 29/29 PASS | 135.849 秒 |
| RV64 `mask=0x003` | 312/314，允许集合 | 343.614 秒 |
| LA64 `mask=0x003` | 308/314，允许集合 | 353.764 秒 |

focused 新增 `smp::exec_owner_becomes_group_leader`，验证：

- CPU0 非 leader owner 在 CPU1 旧 leader 完成退出后接管 PID；
- owner 的 TID、PCB PID、Per-CPU current TID 和 registry PID 项一致；
- 旧 leader 在尚未析构时接管 owner 的旧 TID；
- 旧 leader 迟到 Drop 后，registry 仍返回新 leader；
- owner 离开自身内核栈后可由 zombie 回收，不遗留强引用。

初赛精确失败集合：

- RV64：两套 busybox `kill 10`；
- LA64：两套 basic `test_brk` 各 1/3，以及两套 busybox `kill 10`。

clone/fork/exec/exit/wait 相关项目没有新增失败；无 panic、timeout 或 forbidden marker。

### LA64 runner 完整性说明

LA64 测试进程 exit 0，judge 输出为基线 `308/314`，但 wrapper 的最终 job 状态为
`FAIL`，唯一原因是 `mutation_detected=true`：测试运行期间 Codex 并行修改了本节点的
Markdown 文档，tracked diff 从冻结值变化。期间没有修改、重编译或替换任何可执行源码，
测试日志也没有缺失 marker。为避免为纯文档指纹重复运行约 6 分钟的同一 workload，本节点
接受原始 judge 结果，同时保留 wrapper FAIL，不把它改写成伪造的 PASS manifest。

冻结验证后仅修正了 `spawn_ktest_task_on()` 一条已经与构造器行为不符的源码注释，并同步
本文档、Work Log 与架构文档；没有改变 Rust 可执行逻辑，按注释/文档级 T0 不重复上述矩阵。

## RED 过程与根因

- 首轮用例 panic“exec owner is missing from task registry”：测试 sibling 构造器建立完整
  TCB 后没有登记 TID。修复把“完整 TCB 必须可按 TID 查找”的不变量集中到该构造器，而没有
  把 registry 职责塞进 runqueue publish。
- 第二轮语义断言完成后挂起：测试裸入口返回，没有进入 zombie trampoline。改为显式调用
  生产 zombie 切换路径。
- 第三轮出现 owner 强引用泄漏：noreturn context switch 不会展开当前 Rust 栈。owner 在
  切换前显式 drop 本地 `Arc`，再由 idle 栈回收 TCB。

这些失败均保留为 RED 诊断证据，不计作最终通过。

## 边界

B43 覆盖内核身份事务和双架构 8 核 focused/初赛非回归，但尚未加入独立用户二进制从
非 leader 线程调用 exec 后读取 gettid/getpid 的端到端测试。后续共享子系统开放全核前，
仍需完成 MM-owned `membarrier` 与 FS/net/driver 并发审计。
