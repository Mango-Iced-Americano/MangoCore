# B81 shared signal hint 锁内发布证据

## 结论

状态：`partial`。

B81 关闭了 shared signal queue mutation 与派生原子 hint 之间的锁外旧值覆盖窗口。所有 writer
现在由同一个 process signal mutex 串行，队列修改后在解锁前 Release store 完整 pending
位图；fast reader 使用 Acquire load。双架构 8 核构建和 sigtimedwait focused 均通过。精确
三 CPU store-order 注入没有运行，因此证据状态保持 `partial`。

## 旧竞态

```text
CPU A：signal lock -> dequeue A -> 计算 hint=0 -> unlock -> 暂停
CPU B：signal lock -> enqueue B -> 计算 hint=B -> unlock -> store(B)
CPU A：恢复 -> store(0)
结果：权威队列含 B，hint 却长期为 0
```

旧实现中的 atomic store 虽然没有数据竞争，但它已经逃出 mutex 定义的 writer 顺序。将其单独
升级成 Release 不改变 CPU A 可以最后执行的事实。

## 新不变量

```text
process.signal lock
  -> mutation
  -> 读取权威队列的完整 pending bits
  -> shared_pending_hint.store(bits, Release)
process.signal unlock
```

审计到的四个 mutation：

1. `remove_queued_posix_timer_signal()`：精确移除 timer event；
2. `enqueue_process_signal()`：加入进程 shared pending；
3. `take_shared_signal()`：按单一 signal dequeue；
4. `take_shared_matching()`：按集合 dequeue。

仓库内没有第五个 shared queue mutation 旁路。两个 dequeue 在离开临界区后才分别执行 POSIX
timer discard/finalize，因此 signal lock 与 timer owner 不嵌套。

mutex 负责 writer 全序；Release/Acquire 负责原子 hint 的发布与读取。hint 只用于 fast path，
真正 dequeue 时仍在 signal owner 锁内重验队列。

## 冻结源码

- 基线 HEAD：`d87374dfef43fb52acdf00b9f6d769db5aa3ae18`
- tracked diff SHA-256：
  `e81919e2a0904ecd52047f4db968ca54110ec3e49140adcb0a5b2d83f0d862a0`
- source status SHA-256：
  `d41f2af17890c1c84427003218db86a562a03088cc5f9517cd66095856a3697f`
- 四项 accepted job 的 before/after 指纹一致，`mutation_detected=false`。

## Docker 验证

| Job | 配置 | 时间 | 结果 |
|---|---|---:|---|
| `agent-d94f9e85586e-r01-rv64-kernel-build` | RV64, `CORE_NUM=8` | 134.984s | PASS |
| `agent-d94f9e85586e-r02-la64-kernel-build` | LA64, `CORE_NUM=8` | 134.831s | PASS |
| `agent-d94f9e85586e-r03-rv64-sigtimedwait-gate` | RV64, `CORE_NUM=8` | 140.681s | PASS |
| `agent-d94f9e85586e-r04-la64-sigtimedwait-gate` | LA64, `CORE_NUM=8` | 139.029s | PASS |

四项均 exit 0，无 forbidden marker、required marker 缺失或 timeout。两架构 QEMU 均打印
`online_mask=0xff`；glibc `sigtimedwait01` 各 11 TPASS，TFAIL/TBROK/TCONF 均为 0。

## 未覆盖边界

- 三 CPU stale-store 精确顺序注入：`NOT RUN`。
- musl `sigtimedwait01` 与 `rt_sigtimedwait01`：沿 focused recipe 既有过滤策略，`NOT RUN`。
- POSIX timer、初赛、FS/Net/Driver 矩阵：与本批修改无直接关系，`NOT RUN`。

普通 QEMU 功能测试不能证明必然命中纳秒级旧 writer 暂停窗口；本批以全 mutation 搜索和 mutex
writer 顺序证明补足设计证据，不把 11/11 TPASS 外推成精确交错覆盖。
