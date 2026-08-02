# B63 SysV semaphore/shared-memory ID 生命周期证据

## 1. 结论

B63 通过。最终实现没有为 semaphore 再堆叠一份身份状态，而是删除可能 OOM 的
`removed_ids`：私有等待条件只有一个入口，入口已在同一把 registry 锁下确认对象存在且
操作必须阻塞；semaphore ID 又单调不复用，所以等待期间再次缺失只能来自 `IPC_RMID`，应
直接返回 `EIDRM`。shared-memory ID 从饱和加法改为 checked 单调耗尽，排除极限回绕覆盖
活段以及两阶段 `shmat` attachment 归错对象。

有效证据为双架构 8 核 focused 52/52、RV64 初赛 312/314、LA64 初赛 308/314。所有有效
child 均 exit 0、`online_mask=0xff`、无 panic/fatal/timeout、forbidden marker 或源码
mutation。`i32::MAX` 动态耗尽为 **NOT RUN**，不以静态控制流冒充动态压力。

## 2. semaphore：为什么不需要 tombstone

旧删除路径先移除 set，再调用：

```text
removed_ids.try_reserve(1)
  成功 -> push(id)
  OOM  -> 静默跳过
```

等待者醒来后用 `removed_ids.contains(id)` 区分 `EIDRM/EINVAL`。如果 reserve 失败，真实被
删除的对象会被误报为 `EINVAL`。删除操作又不能因记录 tombstone 失败而回滚已经发生的
`IPC_RMID`，所以这个数据结构在最需要它的内存压力场景下不可靠。

源码全库审计确认 `sem_wait_condition()` 是私有函数，仅由 `sys_semtimedop()` 的 timeout/
无 timeout 两个模板分支调用，而进入模板前已经发生：

```text
持有 SEM_REGISTRY
  -> sets.get_mut(semid) 缺失：首次调用返回 EINVAL
  -> try_apply_sem_ops 成功/错误：直接返回
  -> 只有 Blocked：释放同一锁并进入等待模板
```

`SemRegistry::alloc_id()` 使用 `Option<i32> + checked_add()`，成功发布的 ID 永不复用。因此
等待模板后续持同锁再查找时，缺失不可能表示“从未存在”或“同号新对象”，唯一来源就是
`IPC_RMID`。直接 `EIDRM` 同时保留首次坏 ID 的 `EINVAL`，无需 `max_allocated` 或发布历史。

ncnt/zcnt 的清理也闭合：成功、超时和信号路径在 set 仍存在时清理；EIDRM 时整个 set 已
被移除，计数随对象销毁，不存在独立泄漏。

## 3. shared memory：饱和 ID 会破坏两阶段身份

旧 `saturating_add(1).max(1)` 到达 `i32::MAX` 后会永久重复返回同一 ID。`sys_shmget()`
随后无条件 `BTreeMap::insert`，会替换同号活段。虽然约 21 亿次分配在比赛中不可现实跑到，
但它破坏了生产控制流的身份不变量：

```text
CPU0: shmat(id=X) 锁内 clone 旧段 frames，随后解锁
CPU0: 锁外建立 VMA
CPU1: shmget 饱和分配再次返回 X，insert 替换旧段
CPU0: 按 X 重锁登记 attachment，错误写入新段
```

最终 `ShmRegistry::alloc_id()` 返回 `Option<i32>`，每次成功后 checked 前进；最后一个正 ID
发出后状态变为 `None`，后续 `shmget` 返回 `ENOSPC`，不会覆盖或回绕。全库只有
`sys_shmget()` 一个 `segments.insert` 发布点。

`ShmSegment.frames` 与 VMA 都保存 `Arc<FrameTracker>`。因此 `IPC_RMID`/最后 attachment
清理先删除 registry 元数据时，现存 VMA 的引用仍保证 frame 存活；失败回滚在 registry
解锁后执行 `munmap` 和 TLB shootdown。B63 没有改动 MM/frame 释放协议。

## 4. 官方实现与测试边界

- [Linux ipc/util.c](https://github.com/torvalds/linux/blob/master/ipc/util.c) 用 index/sequence
  区分 SysV IPC 对象 incarnation。Mango v1 没有搬入完整 IDR，只使用当前结构能完整证明的
  单调不复用策略。
- [Linux ipc/sem.c](https://github.com/torvalds/linux/blob/master/ipc/sem.c) 的删除路径会销毁
  set，并让受影响操作以 `EIDRM` 完成。
- [Linux ipc/shm.c](https://github.com/torvalds/linux/blob/master/ipc/shm.c) 将 segment 删除与
  attachment 生命周期分开处理。
- [LTP semop03](https://github.com/linux-test-project/ltp/blob/master/testcases/kernel/syscalls/semop/semop03.c)
  覆盖阻塞 `semop/semtimedop` 在 `IPC_RMID` 后得到 `EIDRM`，以及信号中断得到 `EINTR`；
  它不覆盖 ID 穷尽。

focused recipe 还覆盖 `semctl01/02/03/04/05/06/07/09`、`shmat01`、`shmctl01/07` 和
`mq_open01`。这些用例验证 ABI 和回归，但不能外推为 `i32::MAX` 动态耗尽。

## 5. DeepSeek 审查与人工裁决

- `smp-b63-sysvipc-id-design` 找到 tombstone OOM 静默丢失、SHM 饱和覆盖和两阶段身份风险，
  并确认 VMA frame `Arc` 生命周期安全。它最初建议额外保存 `max_allocated`，用于假想的未来
  绕过入口；GPT/Codex 认为这会维护冗余状态。
- `smp-b63-sysvipc-id-implementation-review` 对最终 diff 给出 ACCEPT，确认 helper 唯一调用、
  初次存在性证明、单调 ID、等待计数清理、SHM 唯一发布点及两阶段重验，因此接受不增加
  `max_allocated`。
- `smp-b63-sysvipc-id-final` 的前两个 focused child 与 RV64 preliminary 有效；DeepSeek 在
  第四槽误选第二次 RV64 preliminary。GPT/Codex 解析精确进程组后在 QEMU 启动前发送
  SIGTERM，作废 child `agent-7cd37a95fb6b-r04-rv64-preliminary` 的 exit 143 只表示主动取消，
  不计入代码失败或 PASS。
- 补充任务 `smp-b63-la64-preliminary-supplement` 只允许一次 LA64 recipe，真实 child PASS。
  DeepSeek 报告把四组有效总分 308/314 误加成 302/308；GPT/Codex 直接解析 stdout 尾部完整
  judge JSON，确认真实总分与失败集合，拒绝模型的错误总数。

为避免再次浪费槽位，本地 ignored 的 agent runner 增加带次数的 `required_recipes`：网关
拒绝超过配额的重复 recipe，并为尚未执行的矩阵项保留 run slot；未声明必跑项时仍允许模型
自主选测。自测已证明重复 RV64 被拒绝、随后 LA64 可正常领取。该本地协议不进入 Git。

## 6. 有效 Docker/QEMU 结果

受测冻结状态：

```text
HEAD                         3d01af50c894c3ff98bd8ac501ca0bd636386c3c
status_sha256                41d33d3a3a70cc8d9148a14777ff1b69fe7e472c6c6663372e9c7ba2daa18b30
tracked_diff_sha256          57a61c1c6a2c179e21bf1d5d70055940698452a12eebf51777fba4cf996673e0
untracked_content_sha256     e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
```

| child | recipe | 结果 | 耗时 | 关键证据 |
|---|---|---:|---:|---|
| `agent-7cd37a95fb6b-r01-rv64-ipc-ltp` | RV64 focused | PASS | 297.373s | 26 RUN、26 PASS；8 条 EIDRM、8 条 EINTR |
| `agent-7cd37a95fb6b-r02-la64-ipc-ltp` | LA64 focused | PASS | 327.586s | 26 RUN、26 PASS；8 条 EIDRM、8 条 EINTR |
| `agent-7cd37a95fb6b-r03-rv64-preliminary` | RV64 mask=0x003 | PASS | 353.870s | 312/314；仅两套 `busybox kill 10` |
| `agent-239cbe63032d-r01-la64-preliminary` | LA64 mask=0x003 | PASS | 355.591s | 308/314；两套 `test_brk` 与 `busybox kill 10` |

focused 两架构各有 musl/glibc 13 个 case，故每架构 26/26、合计 52/52。`semop03` 每架构
日志包含 8 条 EIDRM 与 8 条 EINTR 结果；`shmat01/shmctl01/shmctl07` 每架构各运行两次并
PASS。四个有效 child 的 `source_before == source_after`，`process_exit_code=0`，
`required_markers_missing=[]`、`forbidden_markers_found=[]`、`mutation_detected=false`、
`timed_out=false`。

提交前在 Docker 内定向执行：

```text
rustfmt +nightly-2026-05-10 --edition 2018 --check os/src/syscall/process/ipc.rs
```

并在宿主工作树执行 `git diff --check`，均通过。focused/preliminary recipe 自身已串行完成
RV64、LA64 kernel build 和 QEMU 运行，没有在宿主机编译内核。

## 7. 未覆盖边界

- `i32::MAX` semaphore/SHM ID 动态耗尽 **NOT RUN**；当前只证明 checked 控制流不会回绕。
- 现有 LTP 不制造数十亿次分配，也不直接复现 `shmat` 映射窗口内的饱和覆盖交错。
- B63 不处理 futex requeue 的 waiter 成员关系或 process-shared futex backing identity；它们
  是后续独立节点，不能由 IPC 52/52 外推。
- FS/Net/Driver 按队伍分工未审计。

本节点未增加生产测试字段、临时代码或新模块；DeepSeek/runner 协作文件仅保存在本地
ignored 的 `cc-codex/`，不上传 GitHub。
