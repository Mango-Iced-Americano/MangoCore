# B71 sigtimedwait 睡眠登记窗口证据摘要

## 1. 结论

状态：`pass`

本批关闭 `sigtimedwait()` 第二次条件检查与调度器登记睡眠意图之间的丢唤醒窗口。waited
pending signal 作为 owner 锁保护的持久条件参与 `Blocking` 后最终复查；WaitQueue 非 Ready
返回后再做一次唯一 dequeue，才决定 signal、`EINTR` 或 `EAGAIN`。

## 2. 竞态与修复协议

旧交错为：

```text
CPU A：第二次 condition 未发现 signal
CPU B：发布 pending；看到 CPU A 仍为 Running，wake 不留下未来唤醒
CPU A：登记 Blocking 并切走
```

新协议为：

```text
发布 signal_wait_mask
  -> condition 在 signal owner 锁内尝试 dequeue
  -> 登记 Blocking
  -> has_waited_signal / has_actionable_signal 最终复查
  -> 非 Ready 返回后在 owner 锁内最后 dequeue
  -> 清理 wait mask，锁外 copyout
```

- `has_waited_signal()` 只观察、不领取；线程和进程 pending 分别短暂取得自身 owner 锁；
- ignored-signal 清理排除 `signal_wait_mask`，防止旁路删除消费者声明要领取的对象；
- Interrupted 与 TimedOut 都会最后 dequeue，让边界上已 pending 的 waited signal 优先；
- 没有新增 TaskStatus、锁或 condition 副作用，也没有改变普通 WaitQueue 用户的调用次数。

## 3. AI 协作与独立裁决

| Job | 结果 | GPT/Codex 裁决 |
|---|---|---|
| `smp-b71-sigtimedwait-wakeup-review-r1` | 建议窄修复 | 采纳 Blocking 后精确谓词；改为独立 `has_waited_signal`，补上 ignored 清理与 timeout 后重领 |
| `smp-b71-sigtimedwait-final-review-r1` | PASS，P0/P1=0 | 静态锁序与三类到达窗口结论可采纳；locked/on_queues 变体属于未来使用边界 |
| `smp-b71-sigtimedwait-validation-r1` | REVIEWED，4/4 PASS | 独立核对 child exit、权威 LTP marker、online mask 与源码 before/after 指纹 |

DeepSeek 报告把 8 核运行描述成已在压力下证明没有登记竞态，这一表述过强：普通 LTP 没有
精确控制纳秒级到达位置。本证据只把它记作双架构功能非回归，竞态正确性主要来自上述状态转换
和锁所有权证明。

## 4. 冻结源码

```text
base HEAD: eee91aad9deb1e2b15e08366e86c92d3ca7b0f99
tracked code diff SHA-256: 3ca4db3276428efe45dc65950748321bc4a241e7abc416a2773845a03741ec05
c706c3608207552948622676358a8eddd09f554df1b00a7893d53b613e367641  os/src/task/manager.rs
1c96527b3e39ef02a1c206a557c708c11c9cb498958721521d93604c34e6410c  os/src/task/signal/mod.rs
0059892d5c804a783ded8c64b999bb4c2778dddee238103d63fd43c90f641857  os/src/task/signal/wait.rs
```

四个 child 的 source-before/source-after 均为该 HEAD 与 tracked diff，且
`mutation_detected=false`。

## 5. Docker 验证

| Child | Recipe | CORE_NUM | 原始结果 | 时长 |
|---|---|---:|---|---:|
| `agent-99c2131e0e38-r01-rv64-kernel-build` | RV64 kernel build | 8 | PASS, exit 0 | 124.210 s |
| `agent-99c2131e0e38-r02-la64-kernel-build` | LA64 kernel build | 8 | PASS, exit 0 | 134.318 s |
| `agent-99c2131e0e38-r03-rv64-sigtimedwait-gate` | RV64 suite focused | 8 | 11 TPASS, exit 0 | 128.462 s |
| `agent-99c2131e0e38-r04-la64-sigtimedwait-gate` | LA64 suite focused | 8 | 11 TPASS, exit 0 | 139.724 s |

两架构均打印 `online_mask=0xff` 与
`LTP CASE RESULT sigtimedwait01 : PASS (0)`；LTP Summary 均为 11 passed、0 failed、
0 broken、0 skipped。musl `sigtimedwait01` 与 `rt_sigtimedwait01` 命中既有排除，均为
**NOT RUN**，不是 PASS。

B69 已在相同祖先完成双架构 8 核 `mask=0x003` 初赛，本批按变更风险复用，不重复全量。

## 6. 证据边界

本轮没有实现精确 interleaving 注入，以下动态场景仍为 **NOT RUN**：远端 CPU 严格在第二次
condition 返回后、`Blocking` 发布前发送目标 signal。代码通过持久 pending、Blocking 后最终
谓词及返回后唯一 dequeue 闭合该窗口；后续若增加可控调度 hook，应把该交错加入 SMP ktest。

原始 child 日志与模型报告保存在本地 ignored `cc-codex/runtime/jobs/`，不提交、不上传。
