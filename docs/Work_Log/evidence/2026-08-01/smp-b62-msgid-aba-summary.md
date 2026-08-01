# B62 SysV message queue ID 防 ABA 证据

## 1. 结论

B62 通过。旧实现按最小空洞复用 message queue ID，可能让跨 `MSG_REGISTRY` 解锁等待的
`msgsnd/msgrcv` 在 `IPC_RMID` 后命中同号新对象。最终实现把一次性 requested ID 与自动
cursor 分开，并在队列发布前永久登记 ID；本次内核运行期间不再复用，旧 waiter 醒来只能
得到 `EIDRM`。

有效验证任务为本地 `smp-b62-msgid-final-r5`。四个 Docker child 全部 exit 0、8 核在线、
无 panic/fatal/timeout、forbidden marker 或源码 mutation。精确 RMID→同号重建动态竞态为
**NOT RUN**，不得由现有 LTP 外推。

## 2. 旧竞态

旧 `alloc_id()` 每次从 1 扫描当前 `queues` 的最小空洞：

```text
CPU0: msgrcv(id=7) 发现队列为空，释放 MSG_REGISTRY 后进入 WaitQueue
CPU1: msgctl(id=7, IPC_RMID)，删除队列并唤醒 waiter
CPU2: msgget(...) 扫描空洞，再次发布 id=7 的新队列
CPU0: 醒来后只携带数值 7，重新查表并进入新队列
```

这里 `Arc` 或消息唯一摘取都不能解决问题，因为 waiter 跨等待点只保存数值 `msqid`；必须
让这个数值携带 incarnation，或保证其在相关生命周期内不复用。

## 3. 官方实现与方案裁决

- [Linux ipc/util.c](https://github.com/torvalds/linux/blob/master/ipc/util.c) 的 SysV IPC ID
  allocator 使用 index 与 sequence 组合，并在 lookup 时验证 sequence，从结构上区分同一
  index 的不同对象 incarnation。
- [LTP msgget04](https://github.com/linux-test-project/ltp/blob/master/testcases/kernel/syscalls/msgget/msgget04.c)
  写入 `msg_next_id`，验证下一次创建精确取得 requested ID，并验证该 sysctl 随后恢复 `-1`。
- [LTP msgget05](https://github.com/linux-test-project/ltp/blob/master/testcases/kernel/syscalls/msgget/msgget05.c)
  验证 requested ID 已被活对象占用时必须分配不同 ID。
- [LTP msgrcv06](https://github.com/linux-test-project/ltp/blob/master/testcases/kernel/syscalls/msgrcv/msgrcv06.c)
  验证阻塞 receiver 在 `IPC_RMID` 后得到 `EIDRM`，但没有在唤醒前立即重建同号对象。

MangoCore v1 没有把 Linux 的 IDR/sequence 编码整体搬入当前 `BTreeMap<i32, MsgQueue>`。
当前活队列上限为 `MSGMNI`，比赛场景不要求长期内核运行下的高 churn；因此选择更小且容易
证明的“本次启动期间不复用”协议。若以后需要长期 churn，再独立迁移 index+generation，
不能让性能结构与当前并发修复互相缠绕。

## 4. 实现不变量

`MsgRegistry` 现在区分：

- `next_id`：`/proc/sys/kernel/msg_next_id` 的一次性 requested 值；每次分配尝试都会消费并
  重置为 `-1`；
- `next_auto_id: Option<i32>`：只向前推进的自动 cursor，`None` 表示正 ID 空间耗尽；
- `published_ids: Vec<i32>`：本次内核运行中所有成功进入发布协议的 ID 历史。

分配顺序固定为：

```text
消费 requested ID
  -> requested 从未发布且当前未占用：选 requested
  -> 否则从自动 cursor 开始跳过历史/活对象
  -> published_ids.try_reserve(1)，失败返回 ENOMEM
  -> 推进自动 cursor（checked overflow 后置为 None）
  -> published_ids.push(id)
  -> sys_msgget 将新队列插入 queues
```

历史必须在对象插入前具备容量并登记；否则可能出现“对象已发布，但 OOM 导致身份记录失败”，
删除后再次产生 ABA。反过来，登记后若不可恢复的 map allocation panic，最多留下一个永不
复用的保守历史项，不会把旧身份错误地交给新对象。

`IPC_RMID` 只执行 `queues.remove` 和 `wake_all`，不再尝试分配 `removed_ids` tombstone。
`was_removed(id)` 定义为 `published_ids.contains(id) && !queues.contains_key(id)`。自动 cursor
同时跳过显式 requested ID 留下的稀疏历史；`checked_add` overflow 后，后续自动分配明确
返回 `ENOSPC`。requested 值即使等于已删除 ID也只会回退自动路径。

源码审计确认 message queue 只有 `sys_msgget` 一处 `queues.insert`，不存在绕过 allocator 的
第二发布点。semaphore、shared memory 与 POSIX mqueue 使用不同 registry，本节点不外推。

## 5. DeepSeek 审查与人工裁决

### 5.1 设计与实现审查

- `smp-b62-msgid-aba-design-r2` 确认最小空洞复用构成确定 ABA，并建议单调 ID；但其首版
  算法没有处理“显式 requested=1 先发布、自动 cursor 随后仍返回 1”的碰撞，也把
  `msgget04` 误述为权限测试。GPT/Codex 依据官方 LTP 拒绝该算法，加入统一发布历史。
- `smp-b62-msgid-implementation-review` 对最终 diff 给出 ACCEPT，逐项覆盖 requested 低/高值、
  活对象/删除历史、自动稀疏跳过、`i32::MAX`、`try_reserve`、发布顺序、wake 与唯一 insert。
  唯一维护性建议是把局部变量 `was_removed` 改为 `already_removed`，已采纳。

### 5.2 最终报告纠正

`smp-b62-msgid-final-r5` 最终结论为 ACCEPT，但报告对原始日志有两处误读，不能原样作为
数字证据：

1. 它只看 recipe 的 required marker 去重要求，声称每架构只跑 glibc 13 case。人工直接
   统计原始日志：RV64、LA64 都明确出现 `ltp-musl` 与 `ltp-glibc` START/END，每边均有
   26 个 `RUN LTP CASE` 和 26 个 `LTP CASE RESULT ... PASS`，所以真实结果是每架构
   26/26、双架构 52/52。
2. 它没有解析完整 preliminary JSON，误称 LA64 只失败 `busybox kill 10`，也没有识别
   RV64 `test_pipe` 的已知行拼接。最终裁决以 child 原始 stdout 和完整 JSON 为准，见下节。

模型的静态审查结论、四个 child 的真实 exit/marker/fingerprint 仍有效；测试计数与初赛
失败集合由 GPT/Codex 独立复核。

## 6. 有效 Docker/QEMU 结果

受测冻结状态：

```text
HEAD                         3e20414e7d6a504b9763f2a890f5b2c04dd2dc47
status_sha256                a5819df602da88a05abc8f8dcb2aecf430fea2de84561d5c37ce2360a4731cc0
tracked_diff_sha256          383f3b4aec2a3a64d831f0d9c2a895565807e6e329c881aaa61de24afe0775bd
untracked_content_sha256     e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
```

| child | recipe | 结果 | 耗时 | 关键证据 |
|---|---|---:|---:|---|
| `agent-f4a44324b504-r01-rv64-msg-ltp` | RV64 focused | PASS | 336.968s | 26 RUN、26 PASS；`online_mask=0xff` |
| `agent-f4a44324b504-r02-la64-msg-ltp` | LA64 focused | PASS | 380.540s | 26 RUN、26 PASS；`online_mask=0xff` |
| `agent-f4a44324b504-r03-rv64-preliminary` | RV64 mask=0x003 | PASS | 428.316s | 四组 END；有效 312/314 |
| `agent-f4a44324b504-r04-la64-preliminary` | LA64 mask=0x003 | PASS | 423.373s | 四组 END；308/314 |

focused 的 `msgget04`、`msgget05`、`msgrcv06` 在 musl/glibc 两组、双架构共 12 个对应
case invocation 全部 PASS。RV64 日志例如显示 requested ID 精确取得后 sysctl 为 `-1`，
以及 requested ID 已存在时返回不同 ID；`msgrcv06` 明确得到 `EIDRM (43)`。

RV64 judge 原始表为 309/314：`basic-glibc/test_pipe` 被 parser 记为 1/4，另有两套
`busybox kill 10`。原始块是：

```text
========== START test_pipe ==========
cpid: 112cpid: 0

  Write to pipe successfully.

========== END test_pipe ==========
```

它同时含正 PID、子进程 0、write-success 与完整 END；按项目既有 §8.2 语义账本应为 4/4，
恢复 3 分后是 312/314，真实失败仅两套 `busybox kill 10`。这不是为 B62 新设宽免规则。

LA64 judge 为 308/314，失败精确为：

- `basic-musl/test_brk`：1/3；
- `basic-glibc/test_brk`：1/3；
- 两套 `busybox kill 10`：0/1。

四个 child 的 `source_before == source_after`，`process_exit_code=0`，
`required_markers_missing=[]`、`forbidden_markers_found=[]`、`mutation_detected=false`、
`timed_out=false`。

提交前在 Docker 内对本次唯一 Rust 修改 `os/src/syscall/process/ipc.rs` 执行定向
`rustfmt --check`，并在宿主工作树执行 `git diff --check`，两者均通过。仓库级
`cargo fmt --check` 会命中大量与 B62 无关的既有格式漂移，因此没有执行会改写全树的
`cargo fmt`。

## 7. 作废的环境轮次

这些轮次均未计入 PASS：

- 首个 `smp-b62-msgid-final` 在共享容器被另一性能任务占用时主动取消，B62 QEMU 未启动；
- r2 的独立容器缺少 pinned nightly，preflight 正确 fail-closed；
- 工具链 setup 后的 r3 缺少 linked-worktree submodule，RV64 在依赖解析处失败；
- 中止 r3 时，已进入 Docker 的 LA64 子进程没有随外层 DeepSeek 退出；r4 与该孤儿构建发生
  双架构并行，故立即作废；
- 临时容器 stop/start 清空所有执行进程，补齐 submodule 并再次核对 toolchain/diff 后，r5
  才从干净状态严格串行运行四项。

这些问题属于本地验证隔离与取消传播，不是 B62 代码失败；也没有通过延长超时、删除日志或
放宽 forbidden marker 获得 PASS。

## 8. 未覆盖边界

- 精确“旧 waiter 阻塞 → `IPC_RMID` → 立即请求同一 ID 创建 → 旧 waiter 醒来”的动态场景
  **NOT RUN**。当前实现使同一 ID 根本不能再次发布，正确性证据是锁内发布历史不变量；
  `msgrcv06` 只证明删除唤醒后的 `EIDRM` ABI。
- `published_ids` 查询为 O(n)，历史随本次启动的 churn 增长；这是当前明确接受的 v1
  正确性/维护性权衡，不代表长期高 churn 性能已经验收。
- semaphore 与 shared-memory ID 生命周期、其余 IPC WaitQueue/删除竞态仍需独立节点。
- FS/Net/Driver 按分工未审计。

本节点未增加生产测试字段、临时代码或新模块；DeepSeek/runner 协作文件仅保存在本地忽略的
`cc-codex/`，不上传 GitHub。
