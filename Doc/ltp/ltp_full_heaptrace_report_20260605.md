# LTP Full Heap Trace Analysis Report - 2026-06-05

## 背景

本报告汇总 2026-06-05 在 `ltp` 分支上进行的双架构 LTP 全量扫描结果，并结合近期 focused LTP、heap_trace 与 Work_Log 记录，对当前内核稳定性、资源回收、性能瓶颈和后续适配方向进行分析。

本轮测试使用 Docker/QEMU 运行，内核开启 `heap_trace`，LTP runner 使用 suite 模式，范围为：

- `mask=0x800`
- `ltp_runner=suite`
- `ltp_libc=both`
- `ltp_suites=syscalls,fs_bind`
- `group_timeout_secs=850`

日志文件：

- `logs/ltp-20260605-full-heap-rv64.log`
- `logs/ltp-20260605-full-heap-la64.log`

需要注意，本地环境为 Apple Silicon 上的 amd64 Docker 转译环境，再叠加 QEMU guest 和 heap_trace。该环境可以有效暴露 panic、OOM、Bad address、对象泄漏等稳定性问题，但不适合直接估算最终评测分数或 900 秒内真实覆盖率。

## 本轮 LTP 覆盖结果

每个 libc 轮次原始 suite 约 1506 个用例，默认 exclude 后约 1460 个可执行用例。本轮四个轮次均被 850 秒 group deadline 截断，没有自然跑完全部用例。

| 架构/libc | executed | passed | failed | skipped/remaining bucket | 截止点 | rate |
|---|---:|---:|---:|---:|---|---:|
| rv64/glibc | 818 | 502 | 176 | 782 | `profil01` | 57 cases/min |
| rv64/musl | 903 | 536 | 205 | 713 | `rename08` | 63 cases/min |
| la64/glibc | 424 | 297 | 71 | 1092 | `gettimeofday02` | 29 cases/min |
| la64/musl | 687 | 400 | 149 | 905 | `msgrcv01` | 48 cases/min |

这里的 `skipped` 包含未执行的 remaining bucket，不能直接理解为全部 TCONF/unsupported skip。当前本地结果更适合判断稳定性和趋势，而不是最终得分。

## 稳定性结论

本轮双架构全量 LTP 中没有发现内核级严重异常：

- `PANIC=0`
- `KERNEL EXCEPTION=0`
- `HEAP OOM=0`
- `HEAP ALLOCATION FAILED=0`
- `BUG=0`

rv64 日志中出现的 `Bad address` 均为 LTP 期望的 `EFAULT` 正向测试，例如 `mincore01`、`pipe05`，不是 fork/clone 类内核异常。la64 本轮没有出现 `Bad address`。

这与早期问题形成明显对比：此前 la64 曾有 fork/clone `EFAULT`、getrusage panic、pthread/TLS 用户态异常和对象积累问题。本轮在 full heap_trace 下没有复现这些 P0 级表现。

## fork/clone/getrusage 生命周期分析

本轮 full LTP 中以下关键用例均通过：

- `getrusage01`
- `getrusage02`
- `getrusage03`
- `getrusage04`
- `fork07`
- `fork08`
- `fork09`
- `fork10`
- `fork11`
- `pipe11`
- `pipe12`
- `pipe13`

历史 focused 日志 `ltp-getrusage-*-after-allocfix.log` 显示，`getrusage03` 的 100MB/300MB/400MB children rusage 子项已经全部 TPASS；Work_Log 记录的 zombie PCB auto-reap 修复与本轮 full LTP 结果一致。

本轮 heap_trace 资源曲线也支持这一点：

| 指标 | rv64 | la64 | 结论 |
|---|---:|---:|---|
| `zpcb` peak | 47 | 82 | fork/getrusage/pipe 压力态正常升高 |
| `zpcb` final | 0 | 0 | zombie PCB 已回收 |
| `stale` peak/final | 0/0 | 0/0 | WaitQueue stale 引用未积累 |
| `pipe io_buf` peak | 128K | 128K | pipe 压测正常占用 |
| `pipe io_buf` final | 0 | 0 | pipe buffer 已释放 |

因此，当前分支上 fork/clone/getrusage/pipe 生命周期不再是首要风险。

## 内存与缓存观察

heap_trace 显示 heap used 有增长，但没有失控：

| 架构 | heap used 初始 | 峰值 | 末态 |
|---|---:|---:|---:|
| rv64 | 3950K | 11317K | 8704K |
| la64 | 2754K | 9131K | 8331K |

page cache / inode / mnode 类计数会随 full LTP 增长。例如：

- rv64 末态 `ic=2461`、`mnode=411`
- la64 末态 `ic=1852`、`mnode=280`

这些更像 full LTP 文件访问后的缓存留存，而不是已经确认的泄漏。需要继续关注，但当前没有触发 OOM、panic 或对象生命周期残留。

## 性能与 deadline 分析

四个轮次均被 `group_timeout_secs=850` 截断，说明本地全量 LTP 的主要限制是性能覆盖率。

当前本地环境为：

```text
Apple Silicon ARM64
 -> amd64 Docker translation
 -> qemu-system guest
 -> rv64/la64 kernel
 -> LTP
 -> heap_trace alloc/free instrumentation
```

该路径会严重放大 syscall 密集、fork/exec 密集、timer/epoll 短睡眠类用例的耗时。因此，本轮 full heap_trace 的 deadline 覆盖率不能直接映射到云端评测分数。

从本轮速度看：

- rv64 明显快于 la64。
- la64/glibc 仅执行 424 个用例，说明本地 la64 + heap_trace 的性能损耗很大。
- rv64/musl 覆盖最多，执行到 903 个用例，但仍只覆盖约 62% 的可执行 suite。

建议后续用接近评测环境的 x86_64 Linux 主机或云端跑一轮不开 heap_trace 的 full LTP，用于评估真实分数；本地 heap_trace full LTP 主要用于稳定性审计。

## 失败类型分布

按 unique failed case 粗分：

### rv64

| 分类 | unique failed cases | 代表用例 |
|---|---:|---|
| fs/path/permission | 57 | `access02`, `chdir01`, `chmod05`, `chown04`, `chroot01` |
| fcntl/other misc | 46 | `close_range01`, `fcntl07`, `fcntl14`, `fcntl17`, `fcntl22` |
| xattr | 24 | `fgetxattr*`, `flistxattr*`, `removexattr*` |
| inotify/fanotify | 20 | `inotify03`, `fanotify01`, `fanotify09` |
| module/ioctl/unsupported | 17 | `delete_module*`, `init_module*`, `ioctl04` |
| new mount API | 12 | `fsconfig*`, `fsmount*`, `fsopen*`, `open_tree*` |
| mm/file-backed | 11 | `dirtyc0w_shmem`, `fallocate*`, `mmap04`, `msync04` |
| process/signal/time | 8 | `execveat03`, `nanosleep01`, `ptrace*` |
| mount | 8 | `mount01`-`mount07`, `pivot_root01` |
| epoll | 2 | `epoll01`, `epoll_wait05` |
| socket/net | 3 | `getsockopt01`, `recvmsg01`, `recvmmsg01` |

### la64

| 分类 | unique failed cases | 代表用例 |
|---|---:|---|
| fs/path/permission | 35 | `access02`, `chdir01`, `chmod05`, `chroot01` |
| fcntl/other misc | 29 | `close_range01`, `fcntl07`, `fcntl14`, `fcntl17` |
| xattr | 22 | `fgetxattr*`, `flistxattr*`, `removexattr*` |
| inotify/fanotify | 20 | `inotify03`, `fanotify01`, `fanotify09` |
| module/ioctl/unsupported | 16 | `delete_module*`, `init_module*`, `ioctl04` |
| new mount API | 12 | `fsconfig*`, `fsmount*`, `fsopen*`, `open_tree*` |
| mount | 7 | `mount01`-`mount07` |
| mm/file-backed | 6 | `fallocate*`, `mmap04`, `mmap12` |
| epoll | 2 | `epoll01`, `epoll_wait05` |
| process/signal/time | 2 | `execveat03`, `gettimeofday02` deadline kill |

失败最多的是 fs/path/xattr/mount/inotify/fanotify。这与当前“非 fs/net 优先，避免与队友冲突”的策略一致：大头分布在暂时不宜直接动的大块子系统。

## 关键异常点解释

### epoll01

`epoll01` 在四轮中均 timeout：

- rv64/glibc: 30s timeout
- rv64/musl: 30s timeout
- la64/glibc: 60s timeout
- la64/musl: 60s timeout

日志显示 `epoll_create` 大部分子项可以通过，但进入 `epoll_ctl` 大组合后超时。rv64/musl 中还出现过 `epoll_create(-1)` unexpectedly succeeded，说明仍有明确 ABI 语义缺口。

这不是 panic/OOM 类问题，但会稳定消耗 deadline，影响 full LTP 覆盖率。

### epoll_wait05

`epoll_wait05` 双架构双 libc 均失败，原因明确：

```text
EPOLLRDHUP has not been received
```

该问题处在 epoll 与 socket hangup/readiness 交界，涉及 net/socket event mask 传播。它比单纯 syscall 参数校验更宽，建议在协调 net 负责人后再深入。

### pselect01_64

focused 日志中 `pselect01_64` 双架构通过；本轮 rv64 full 里出现一次失败。具体失败在 10ms sleep 档位：

```text
median 10530us
TFAIL: pselect() slept for too long
```

其他 1ms、2ms、5ms、25ms、100ms、1s 档位均 TPASS。这更像本地转译 + heap_trace 下的短睡眠抖动，不宜直接判定为内核语义回归。建议在非 heap_trace 或云端环境复验。

### dirtyc0w_shmem

rv64/musl 中 `dirtyc0w_shmem` timeout，但日志先出现：

```text
open(/proc/self/mem) failed: ENOENT
```

这属于 procfs 能力缺口叠加 timeout，不是内核崩溃。

### la64 gettimeofday02

la64/glibc 在 `gettimeofday02` 被 group deadline kill；la64/musl 同 case 后续通过。该问题更像 glibc 轮次前半段耗时导致 deadline 截断，不应单独归因为 gettimeofday 语义错误。

## 与近期 focused 修复的关系

近期 focused 验证已经覆盖并稳定通过的内容，在本轮 full LTP 中大多保持稳定：

- `sighold02`、`pselect01_64` focused 双架构验证通过或 musl TCONF 正确 skip。
- POSIX mqueue 7 个 focused 用例双架构双 libc 通过。
- `getrusage03` focused 子项全部 TPASS。
- `fork07-11`、`pipe11-13` 在 full LTP 中也通过。
- `zpcb/stale/io_buf` 在 full LTP 压力态后回落。

这说明近期修复方向是有效的：核心生命周期、signal/wait、mqueue、timer、pipe/fcntl/epoll 部分子项的稳定性提升没有引入新的内核级回归。

## 当前风险判断

### 可以认为已经收敛的风险

- la64 fork/clone `Bad address`
- getrusage panic
- zombie PCB 长期堆积
- WaitQueue stale 引用堆积
- pipe buffer 长期残留
- full LTP 下 heap OOM/panic

### 仍需关注的风险

- 本地 full LTP deadline 覆盖率低，尤其 la64/glibc。
- `epoll01` 稳定 timeout，影响覆盖率。
- `epoll_wait05` 缺 `EPOLLRDHUP`，会牵涉 socket/epoll 联动。
- fs/path/xattr/mount/inotify/fanotify 失败数量最多，但需要与负责 fs/net 的同学协调。
- page cache / inode / mnode 计数随 full LTP 增长，当前不是确定泄漏，但建议后续在云端或长跑日志里继续观测。

## 后续适配建议

### P0: 云端/近评测环境复验

优先在 x86_64 Linux 或云端环境跑一轮不开 heap_trace 的 full LTP，确认真实 900 秒覆盖率。当前本地 ARM 转译结果不适合直接估分。

### P1: 低冲突非 fs/net 小点

优先处理：

- `close_range01`
- `fcntl07/14/17/22` 及 64 位变体
- `execveat03`
- `ptrace*` 中较小语义点

这些点相对不容易和 fs/net 分支冲突，且可能在 full LTP 中快速增加 pass。

### P2: epoll 分层修复

先做小语义：

- `epoll_create(size <= 0)` 返回 `EINVAL`

再评估大问题：

- `epoll01` timeout 的 `epoll_ctl` 大组合性能/语义
- `epoll_wait05` 的 `EPOLLRDHUP`

其中 `EPOLLRDHUP` 需要 socket half-close/hangup readiness 支持，建议和 net 负责人确认后推进。

### P3: fs/net 大块协调后推进

失败数量最多的是：

- xattr
- path permission/chroot/chown/chmod
- inotify/fanotify
- mount/new mount API

这些是高收益区，但冲突风险也最高。若后续团队允许接手，应单独按子系统开分支推进。

## 结论

当前分支已经从“容易出现 panic/OOM/Bad address 的不稳定状态”推进到“full LTP 可稳定运行到 deadline，核心资源能回收”的状态。下一阶段最重要的是区分环境性能噪声和真实功能缺口：

- 稳定性：当前可接受，未见 P0 回归。
- 性能：本地转译环境下不具备估分价值，需要云端复验。
- 适配收益：非 fs/net 优先看 `close_range/fcntl/execveat/epoll_create`；fs/net 大块需要团队协调。
