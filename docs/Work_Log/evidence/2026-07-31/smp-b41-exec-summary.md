# SMP B41 多线程 exec 验证摘要

## 1. 冻结对象

- HEAD：`f1797a85e5b91e5f80ba1d8aa005e183e1936f02`
- 生产源码 diff SHA-256：
  `dff2949af9e355cc1c5382f869e26068d014307e28de1872dc2507a457949d55`
- 目标：多线程 exec 临时关门、sibling owner 自清理、live-count Completion、
  late clone 拒绝、旧 MM 延迟替换，以及等待点生命周期退出。
- B41 在验证时未提交；本文档同步不属于上述生产源码哈希。

验证结束后删除了 `load_elf()` 上方一条与新协议矛盾的旧优化注释，没有改变 Rust
token 或可执行逻辑；最终生产源码 diff SHA-256 为
`7d66fa128f57f723bb4ea4c24dd1a5e80689c11b91ac2d0509e16599daf101f3`。
按注释级 T0 不机械重跑矩阵。

## 2. 环境

- Docker container：`mangocore-smp-integration-20260725-os-dev-1`
- Image：`zhouzhouyi/os-contest:20260510`
- Image ID：
  `sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- Repo digest：
  `zhouzhouyi/os-contest@sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- Image created：`2026-05-10T08:46:16.065707166Z`
- RV64/LA64 QEMU：10.0.2
- CPU：8；focused `KTEST=smp KREPEAT=1`；初赛 `mask=0x003`

## 3. 结果

| 架构 | 项目 | 用时 | 结果 |
|---|---|---:|---|
| RV64 | kernel build | 129.485 s | exit 0 |
| LA64 | kernel build | 133.289 s | exit 0 |
| RV64 | 8 核 SMP focused | 138.659 s | 27/27 PASS，online `0xff` |
| LA64 | 8 核 SMP focused | 133.912 s | 27/27 PASS，online `0xff` |
| RV64 | 8 核初赛 basic+busybox | 343.210 s | 312/314 |
| LA64 | 8 核初赛 basic+busybox | 373.459 s | 308/314 |

focused 中第 25 项 `group_exit_stops_remote_sibling`、第 26 项
`exec_stops_remote_sibling` 和第 27 项 STOP 均通过。B41 用例覆盖：

- CPU0 exec owner；
- CPU1 `Running` sibling；
- 真实未完成 Completion 上的稳定 `Blocked` sibling；
- exec 关门后的 late `New` sibling；
- sibling 只在 CPU1 安全点自行清理；
- owner 安装新映像前 live count 已收缩为 1；
- late publish 返回 `EAGAIN`，任务保持 `New` 且从未运行；
- exec 会话完成后发布门重新开放。

初赛精确失败集合：

- RV64：musl/glibc 两套 `busybox kill 10` 各 0/1；其余满分。
- LA64：musl/glibc 两套 `test_brk` 各 1/3，两套 `busybox kill 10`
  各 0/1；其余满分。
- 双架构 clone/fork/exec/exit/wait/waitpid 均满分，无新增失败。

所有有效 child 均满足：

- process exit code 0；
- no timeout；
- no forbidden marker/panic；
- `mutation_detected=false`；
- HEAD、tracked source diff 和工作树状态 before/after 一致。

## 4. DeepSeek 执行裁决

首次总任务 `smp-b41-validation-r8` 的前五个 child 依次完成双架构 build、
双架构 focused 和 RV64 preliminary，结果全部有效。DeepSeek 在分析 RV64 日志时误将
第六个允许槽再次提交为 `rv64-preliminary`，导致缺少 LA64 preliminary；包装器正确以
`DeepSeek executed fewer tests than required` fail-closed。重复任务被提前终止，该总任务
不标为 PASS。

补充任务 `smp-b41-la-preliminary-r9` 只允许一次 LA64 preliminary，最终
`SUCCEEDED/REVIEWED`，child exit 0、308/314、无源码 mutation。最终门禁结论由 r8
前五个有效 child 与 r9 一个有效 child 合并，不用模型自然语言覆盖 wrapper/manifest 事实。

## 5. 证据边界

本批证明 sibling 不再被远端 CPU 代清资源，owner 只在 authoritative live count
降为 1 后替换旧 MM，并覆盖 Running、稳定 Blocked 和 late publication。它不证明：

- Linux 非 leader exec 的 TID/TGID leader 身份接管；
- 所有 `CLONE_FILES/CLONE_SIGHAND` 等跨 PCB 共享资源的 exec unshare 语义；
- 普通用户任务默认全核调度或 FS/net/driver 的全核并发安全；
- 任意内核指令位置抢占。
