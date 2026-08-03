# B82 真实用户 CoW + 同 VPN remap TLB 证据

- 状态：`partial`（B82 定向门禁 pass；完整 SMP suite 被既有 exec identity 超时阻塞）
- 基线 HEAD：`e5a013f375a79621dc33ef13c79351e481922c3e`
- tracked diff SHA-256：`852fcea09b5caf1b2ae646e0aae21252345bcf6371f2bd711085242a020d0a35`
- Docker container：`mangocore-smp-integration-20260725-os-dev-1`
- image ID：`sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- repo digest：`zhouzhouyi/os-contest@sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- image created：`2026-05-10T08:46:16.065707166Z`
- QEMU：RV64/LA64 均为 `10.0.2`

## 审查与规范

- DeepSeek review job：`smp-s82-unmap-review-r1`，137.699s，exit 0，未修改源码。
- 官方依据：LoongArch Reference Manual V1.10 §7.6.2 规定只有 `TCFG.En=1` 才倒计时，
  §7.6.5 规定 TICLR 只清 timer interrupt flag。
- 人工复核补充：失败清理必须同时保留 old/remap/progress 三个 frame；模型只指出前两页。

## Docker/QEMU 验证

候选修复由 DeepSeek job `smp-s82-timer-fix-validation-r1` 经受限 runner 串行执行：

1. `make ktest ARCH=rv64 PROFILE=normal CORE_NUM=8 KTEST=smp KREPEAT=1`
   - runner child：`agent-8621d1301f91-r01-rv64-ktest`
   - 148.601s，进程退出码 0，`online_mask=0xff`
   - `smp::remote_user_load_observes_cow_and_remap`：PASS
   - suite：33/34；仅 `exec_owner_becomes_group_leader` 5003ms 超时
2. `make ktest ARCH=la64 PROFILE=normal CORE_NUM=8 KTEST=smp KREPEAT=1`
   - runner child：`agent-8621d1301f91-r02-la64-ktest`
   - 141.477s，进程退出码 0，`online_mask=0xff`
   - `smp::remote_user_load_observes_cow_and_remap`：PASS
   - 不再出现 `stale-TLB timer isolation evidence was incomplete`
   - suite：33/34；仅 `exec_owner_becomes_group_leader` 5002ms 超时

两次测试均 `mutation_detected=false`，无 kernel panic 或 fatal trap。runner 因
`KTEST RESULT: FAIL` 正确把完整套件记为 FAIL；本证据只把 B82 的定向结果判为 pass。

## 提交前静态门禁

- `git diff --check`：PASS。
- Docker `make lint`：FAIL，原因是 develop 合并后的首方 warning baseline stale；输出同时
  出现 5 个新增和 5 个已消失的 `unknown` warning，未擅自执行 `--capture-baseline`。
- Docker `cargo fmt --check`：FAIL，显示 root library 与 OS/FS 等大量既有格式漂移；未格式化
  FS/Net/Driver 或其它队友范围。本次三个 Rust 文件均已由双架构 release 构建实际编译。
