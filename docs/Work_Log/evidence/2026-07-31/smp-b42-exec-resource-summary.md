# SMP B42 exec 跨 PCB 共享资源隔离证据

## 1. 被测源码

- 分支：`smp`
- 基线 HEAD：`59548ccd5f0734388f6b1419dd2eda5b9f4e645a`
- tracked diff SHA-256：
  `d8a35abd316d307901acce6febee716028c68d4718188e42cbe30fd386ce22ae`
- 六个测试 child 的 HEAD、tracked diff 与工作树状态 before/after 均一致
- `mutation_detected=false`

冻结测试覆盖 B42 的四个生产/测试文件；本证据和架构文档在测试完成后补写，不改变 Rust
token 或可执行逻辑，因此按 T0 不重复运行矩阵。

## 2. 环境

- Docker image：`zhouzhouyi/os-contest:20260510`
- image ID：
  `sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- repo digest：
  `zhouzhouyi/os-contest@sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- RV64 QEMU：10.0.2
- LA64 QEMU：10.0.2
- CPU 数：8
- QEMU 拓扑：单 socket、8 core、单 thread

## 3. 最终冻结验证

所有构建和 QEMU 命令均由本地 DeepSeek worker 在 Docker 内串行执行；GPT/Codex 依据
child manifest、原始日志和源码指纹独立验收。

| 顺序 | 架构/任务 | 结果 | 用时 |
|---|---|---|---:|
| 1 | RV64 kernel build | PASS，exit 0，无新增 warning | 132.190 秒 |
| 2 | LA64 kernel build | PASS，exit 0，无新增 warning | 133.725 秒 |
| 3 | RV64 `CORE_NUM=8 KTEST=smp KREPEAT=1` | PASS，28/28 | 138.224 秒 |
| 4 | LA64 `CORE_NUM=8 KTEST=smp KREPEAT=1` | PASS，28/28 | 135.807 秒 |
| 5 | RV64 `CORE_NUM=8 mask=0x003` | PASS，312/314 | 344.522 秒 |
| 6 | LA64 `CORE_NUM=8 mask=0x003` | PASS，308/314 | 344.320 秒 |

focused 第 27 项 `smp::exec_does_not_mutate_shared_resources` 在双架构通过。它持有 fd
table、sighand 和 futex 的旧 `Arc`，调用生产 `reset_exec_resources()` 后验证：

1. 当前 PCB 安装的三个对象都不再与旧对象指针相同；
2. 旧 fd table 的 CLOEXEC probe fd 仍存在，当前 PCB 副本中已经关闭；
3. 旧 sighand 仍保留 probe action，当前 PCB 副本已经按 exec 语义清除；
4. private futex table 已换新，没有清空旧地址空间可能继续使用的对象。

初赛精确失败集合与 B41 基线一致：

- RV64：仅 `busybox-glibc`、`busybox-musl` 的 `busybox kill 10`；
- LA64：上述两项，以及 `basic-musl`、`basic-glibc` 的 `test_brk` 各 1/3；
- clone/fork/exec/exit/wait/waitpid 均无新增失败；
- 无 panic、timeout、forbidden marker。

## 4. 审查中发现并修正的问题

首轮只读审查发现初稿在同一个 tuple 中先 clone `Arc`、再读取 `strong_count()`。Rust
从左到右求值，因此辅助函数自己的临时 clone 会把唯一资源误判成共享资源。该初稿没有进入
最终冻结测试。修正后先在 `process.inner` 内读取两个共享布尔值，再取得对象快照。

GPT/Codex 继续沿析构链审查，发现替换 futex 时若让旧 `Arc` 在 `process.inner` 内 drop，
WaitQueue 可能释放任务引用并回入进程锁。最终实现先把旧 futex 移出，显式释放
`process.inner` 后再析构。

## 5. 证据边界

- focused 用例通过额外 `Arc` 制造与跨 PCB 相同的共享所有权条件，但没有从用户态执行
  一次完整的 `clone(CLONE_FILES/CLONE_SIGHAND) -> execve`。
- B42 只解决资源对象隔离，不解决非 leader exec 后的 TID/TGID 身份接管；该语义留给 B43。
- 本轮没有开放普通用户任务的默认全核 affinity。
