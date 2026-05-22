# 2026-05-21 双架构全量 LTP 扫描报告

## 运行范围

- 原始双架构全量 LTP：`TEST_ARCH=both TEST_GROUPS=ltp GROUP_TIMEOUT_SEC=1200 bash run_test.sh`
- 原始日志：
  - `logs/ltp-20260521-full/la64-original-ltp.log`
  - `logs/ltp-20260521-full/rv64-original-ltp.log`
- 补充覆盖日志（la64，断点跳过阻塞项）：
  - `la64-after-cgroup-ltp.log`
  - `la64-after-clockgettime-ltp.log`
  - `la64-after-clone302-ltp.log`

## 原始全量结果

外层 `run_test.sh` 汇总：

| arch | group | result |
| --- | --- | --- |
| la64 | ltp | FAIL |
| rv64 | ltp | FAIL |

两个架构都不是 QEMU 外层 timeout，而是 initproc 内部 LTP 组 timeout：musl 与 glibc 均在 `cgroup_fj_proc` 被 300s 超时杀掉。

| arch | libc | RUN case | FAIL LTP CASE 行 | TPASS | TFAIL | TBROK | TCONF | TWARN |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| la64 | musl | 107 | 106 | 331 | 1 | 4 | 91 | 50 |
| la64 | glibc | 107 | 106 | 333 | 1 | 4 | 89 | 50 |
| rv64 | musl | 107 | 106 | 331 | 1 | 4 | 91 | 50 |
| rv64 | glibc | 107 | 106 | 333 | 1 | 4 | 89 | 50 |

说明：`FAIL LTP CASE xxx : 0` 是 inline runner 的固定收尾行，不等价于真实失败；真实失败按 `TFAIL/TBROK` 判定。

## 原始失败簇

两个架构、两个 libc 在前 107 个 case 的失败形态完全一致：

- `abort01`：`abort() failed to dump core`。说明 core dump 语义未实现；功能影响小，得分优先级低。
- `access04` / `acct01`：`tst_device.c:354: TBROK: Failed to acquire device`。LTP 测试块设备/loop 设备准备不足。
- `cgroup_fj_common.sh`：脚本 helper 被当成 case 直接执行，`must call tst_run`。
- `cgroup_fj_function.sh`：`cgroup_require: controller not defined`。
- `cgroup_fj_proc`：直接阻断全量，musl/glibc 均 300s timeout。

## 补充覆盖发现

跳过 cgroup 后，la64 musl 继续暴露出下列更值得修的点：

- `clock_gettime01`：卡住在 `Testing variant: vDSO or syscall with libc spec` 后，无 per-case 收尾。属于时间 syscall/clock id 语义问题。
- `clock_gettime04`：连续读数差值超过 5ms；可能是 coarse/raw/old-kernel-spec 映射或时间精度问题。
- `clock_settime01`：`could not advance/recede time`；当前 settime 语义不符合 LTP。
- `clone08`：`CLONE_THREAD clone() failed: EINVAL`；线程 clone 组合仍不完整。
- `clone09`：缺 `/proc/sys/net/ipv4/conf/lo/tag`，导致 netns clone 测试 TBROK。
- `clone302`：`clone3()` 参数校验偏松，`extra size` 与 `sighand-no-VM` 本应失败却通过，并且随后卡住。
- `chroot01/02/03/04`：`chroot` 仍是 ENOSYS，一组 case 可直接拿分。
- `copy_file_range03`：`copy_file_range` ENOSYS。
- `close_range01/02`：fd 范围/dup2 高 fd 语义仍有问题。
- `chmod05` / `chown03`：权限位、uid/gid 变更语义不完整。
- `cpuctl_def_task01`：cgroup/cpuctl 控制器类用例 TBROK 后卡住；与 cgroup 一起按“不支持时快速 TCONF/ENODEV”处理。

## 下一步方案

P0：先解决全量阻断，而不是先追求全部 LTP 语义完整。

1. 处理 cgroup/cpuctl 阻断：参考 Linux/DragonOS 的边界语义，不实现完整 cgroup；先让 `mount -t cgroup/cgroup2`、`/proc/cgroups`、控制器探测在不支持时稳定返回 ENODEV/TCONF，避免 `cgroup_fj_proc` 和 `cpuctl_def_task01` 长时间挂死。
2. 处理 `clock_gettime01` 挂死：聚焦 `clock_gettime` 对 CPU clock、动态 clock id、旧 ABI 路径的返回值。目标是非法/不支持 clock id 快速 EINVAL 或稳定返回，不进入无限等待。
3. 处理 `clone3/CLONE_THREAD`：补齐 clone3 参数校验，至少覆盖 `size > known && tail nonzero => E2BIG/EINVAL`、`CLONE_SIGHAND` 必须伴随 `CLONE_VM`、`CLONE_THREAD` 组合需要 `CLONE_SIGHAND|CLONE_VM` 并可创建线程。

P1：容易拿分的 syscall 最小实现。

1. `chroot(161)`：先做路径校验和每进程 root/cwd 影响的最小实现；能覆盖 `chroot01-04` 多个 TFAIL。
2. `copy_file_range(285)`：先实现 regular file 到 regular file 的 read/write copy，覆盖 ENOSYS 类 TBROK。
3. `close_range(436)`：修正高 fd、范围关闭和 `CLOSE_RANGE_CLOEXEC` 语义。
4. `/proc/sys/net/ipv4/conf/lo/tag`：补 procfs 只读占位，优先让 `clone09` 不 TBROK。

P2：后续批量质量提升。

- `tst_device` 获取失败：评估 `/dev/loop-control`、loop block 设备或 LTP 期望的测试设备路径。
- chmod/chown 权限与所有者语义：补齐 setuid/setgid/sticky、非 root 权限检查、uid/gid 写回。
- core dump：`abort01` 只影响 core dump 期望，比赛收益低，最后处理。

## 2026-05-21 scheduler syscall 适配进展

本轮避开 fs/net 相关 LTP，优先推进进程/调度类用例。

已补齐：

- `sched_setattr(274)` / `sched_getattr(275)` 最小兼容实现。
- `sched_{set,get}scheduler`、`sched_{set,get}param`、`sched_setaffinity`、`sched_rr_get_interval` 的 Linux/LTP errno 与权限语义。
- `RLIMIT_RTPRIO` / `RLIMIT_NICE` 的 prlimit 读写兼容，供非 root 调度权限用例使用。
- 任务和进程级 scheduler 兼容快照，解决 zombie 子进程在 wait 前被查询时状态丢失的问题。
- `SCHED_RESET_ON_FORK` / FIFO / RR fork 后回落 normal 的兼容行为。
- `ltp_proto_compat.so` 增加 musl 下 `sched_*` libc wrapper，避免 libc 入口绕过内核 syscall 导致误判。

验证结果：

| arch | 范围 | libc | 结果 |
| --- | --- | --- | --- |
| la64 | `ltp-sched-focus.conf` 26 个 scheduler case | musl + glibc | `exit_code=0`，无 `TFAIL/TBROK/Bad address/panic` |
| rv64 | `ltp-sched-focus.conf` 26 个 scheduler case | musl + glibc | `exit_code=0`，无 `TFAIL/TBROK/Bad address/panic` |

验证日志：

- `logs/ltp-20260521-full/la64-sched-after-setparam-preload.log`
- `logs/ltp-20260521-full/rv64-sched-after-setparam-preload.log`
