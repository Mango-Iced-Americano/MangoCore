# SMP B28 受控 AP 用户态闭环证据摘要

## 1. 状态与证据边界

- 阶段状态：`partial`
- 基线/当前 HEAD：`45db89221a03560f4b9f38f619748b5495c3a38d`（B27 已提交）
- 功能首轮 tracked diff：
  `ea178a0b837492dc6dda191c987b2cc8535472db1ddb36712fa66af80f4009b1`
- 最终行为复验 tracked/code diff：
  `631d2f7b9c469e62db057a97cc0f51fef7e5c80c1f973af9ebc105676e69d33f`
- 复验后仅纠正文案的当前生产代码 diff：
  `1cabfb82804904d60d249a89d2692bd4d098f4a8607f5f1c49cbd02a629d0873`
- 功能门禁通过；仓库 warning baseline 漂移使 `make lint` RED，因此不写 `pass`。
- 本摘要证明一个受控用户任务由 CPU0 创建、CPU1 执行、CPU0 回收；不证明同一任务
  CPU0↔CPU1 迁移、普通 affinity/负载均衡、AP timer 抢占或 FS/net/driver 多核安全。

## 2. 环境

- 容器：`mangocore-smp-integration-20260725-os-dev-1`
- 容器 ID：
  `a99062375fdbde7b8989f6b9622438229a8609991a3aad86443a5eafcc4acfca`
- 镜像：`zhouzhouyi/os-contest:20260510`
- 镜像 ID：
  `sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- Repo digest：
  `zhouzhouyi/os-contest@sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- 镜像创建时间：`2026-05-10T08:46:16.065707166Z`
- RV64/LA64 QEMU：`10.0.2`
- bind mount：`/home/lzm/projects/MangoCore-smp-integration-20260725 => /app`
- DeepSeek 协作数据和完整日志只在本地忽略目录 `cc-codex/`，不提交或上传。

## 3. 生产不变量与实现证据

1. `publish_task_on(task, cpu)` 先验证目标，再为远端 CPU 同步动态 kernel stack 映射，
   然后调用唯一的 `run_queue::publish()`，最后在 runqueue 锁释放后发送 `RESCHEDULE`。
2. 普通 `publish_task()` 仍固定 CPU0。B28 只是显式调用内部目标发布入口，没有加入负载
   选择、affinity 或隐式迁移。
3. 双架构用户 trap 通过 `current_trap_task()` 克隆本 CPU current，并在 processor 锁外
   校验状态必须是 `Running(cpu)`；syscall 返回后重新读取 CPU ID，不复用旧 owner。
4. trap handler 在调用可能不返回的 `syscall()` 前 drop 临时 TCB `Arc`。返回型 syscall
   再取得 current；测试在 wait/reap 后验证对应 `Weak` 无法升级。
5. probe 只执行 Linux generic ABI 的 getpid(172)、yield(124)、exit(93)。代码先装载到
   RWU 匿名页，再经正式 mprotect 收紧为 RXU；任务可见前关闭全部 fd。
6. AP timer/external IRQ 不开放。用户 probe 只在 syscall 受控窗口响应 IPI，yield 是显式
   安全点；CPU0 仍独占 timeout、console、net、FS 和其它 housekeeping。
7. probe 有三次用户 trap：getpid、yield 两次完成返回，exit 为非返回 trap。该事实不能
   写成“三次往返”。

## 4. DeepSeek 审查及人工裁决

设计审查确认旧 CPU0-only trap 断言、CPU-local trap context 更新和 AP timer 边界是最小
实现的关键点。人工裁决如下：

- 采纳：删除 CPU0-only 断言，以真实 current owner 不变量替代；使用现有 AP 调度、MM
  激活和 syscall IRQ 窗口；重点验证退出后的 TCB/内核栈生命周期。
- 拒绝：新增 `/test_ap_user` 用户 ELF。探针只有几条架构指令，新文件会扩大用户构建与镜像
  依赖；现实现将只读指令嵌入 ktest，仍走真实用户页表和 trap-return。
- 拒绝：增加测试专用生产发布 API/状态字段。生产只保留一个通用首次发布入口，测试状态
  留在 `kernel_tests/smp.rs`。
- 纠正：远端 kernel stack 同步必须发生在 runqueue 发布前；不能照审查草案中的反向示例。
- 纠正：B28 没有并发修改 probe MM 的 PTE，不能宣称动态覆盖 generation race 或远端
  shootdown；B23—B27 的专项用例继续承担这部分证据。

## 5. 冻结测试结果

所有 build/QEMU 都由受限 gateway 在 Docker 内串行执行。首轮验证覆盖完整功能和初赛；
最终收敛只改变 owner helper 的锁形状、页权限和测试确定性，因此最终轮按风险只重复两架构
focused ktest。

| child job | 冻结版本 | recipe | 秒 | exit | mutation | 结果 |
|---|---|---|---:|---:|---|---|
| `agent-c6821754ca3f-r01-rv64-ktest` | 首轮 | RV64 8 核 SMP | 132.749 | 0 | false | 21/21 PASS |
| `agent-c6821754ca3f-r02-la64-ktest` | 首轮 | LA64 8 核 SMP | 140.637 | 0 | false | 21/21 PASS |
| `agent-c6821754ca3f-r03-rv64-preliminary` | 首轮 | RV64 8 核 `mask=0x003` | 328.111 | 0 | false | 312/314 |
| `agent-c6821754ca3f-r04-la64-preliminary` | 首轮 | LA64 8 核 `mask=0x003` | 354.891 | 0 | false | 308/314 |
| `agent-c6821754ca3f-r05-lint` | 首轮 | warning baseline | 14.304 | 2 | true | 既有 baseline/tool RED |
| `agent-d05b071cb5a1-r01-rv64-ktest` | 最终 | RV64 8 核 SMP | 132.367 | 0 | false | 21/21 PASS |
| `agent-d05b071cb5a1-r02-la64-ktest` | 最终 | LA64 8 核 SMP | 142.430 | 0 | false | 21/21 PASS |

最终 focused 的共同锚点：

```text
[smp] minimal boot ready: configured=8 ... online_mask=0xff
1..21
ok 20 smp::ap_user_syscall_round_trip
[KTEST RESULT: PASS]
```

RV64 本轮 cold-boot hart 为 5，仍正确映射为逻辑 CPU0。两架构没有 panic、fatal trap、
timeout、forbidden marker 或缺失 required marker，运行前后 HEAD/status/tracked diff/untracked
content 哈希一致。

初赛结果：

| 架构 | basic-musl | basic-glibc | busybox-musl | busybox-glibc | 总分 |
|---|---:|---:|---:|---:|---:|
| RV64 | 102/102 | 102/102 | 54/55 | 54/55 | 312/314 |
| LA64 | 100/102 | 100/102 | 54/55 | 54/55 | 308/314 |

RV64 只缺两组 `busybox kill 10`；LA64 另有两组 `test_brk` 各缺 2 分，失败身份与 B27
一致。两架构四个 group END 和 `[initproc] run_selected_groups done` 均齐全。

## 6. lint RED 与测试产物清理

`make lint` 仍在 RV64 debug warning baseline 比较阶段退出 2：四个新增 tuple 位于
`drivers/rng/mod.rs`、`fs/ext4/bitmap.rs`、`mm/user_mapper.rs`、`smp.rs`，并有五个旧 tuple
消失。B28 修改文件不在集合中，没有执行 `--capture-baseline`。

初赛 recipe 在原本不存在的精确路径生成了三个 root-owned 68-byte RV64 ELF stub：

```text
user/tools/rv64/bin/bash
user/tools/rv64/bin/busybox
user/tools/rv64/lib/ltp_proto_compat-rv64.so
```

因此首轮父 wrapper 正确报告 visible Git state changed；五个 child 仍各自保存冻结前后哈希。
人工确认创建时间和任务前 untracked hash 为空后，只通过容器删除上述三个路径。最终 wrapper
为 `SUCCEEDED/REVIEWED`。定向 Docker rustfmt 对六个修改 Rust 文件 exit 0。

## 7. 原始日志哈希

| job | stdout SHA-256 | stderr SHA-256 |
|---|---|---|
| 首轮 RV64 focused | `3c99029e3c08b6e3c82abd660972af8eeb21f957a2b11a6327cad517439fed5d` | `950ff3aeec5239195bba330fff5e4ea64b6c929b01ea3d869860af2db1666f2a` |
| 首轮 LA64 focused | `b1c8e9512014e69ff74e538de91e401f7152306fb7336e10d1c2353235abba18` | `68104673c617a6c4b04c653828d5a20abeeb0bfdeb3772bb7845c8c4439260d1` |
| RV64 preliminary | `b96a24b606167660b063791e28bbd26b26f422ae2aa7c755795701b7a9cb8461` | `1586ee88aa9d1014751d6bf1b4d0a61e109f63d158f78dc444fddb2f8eec69ab` |
| LA64 preliminary | `4d8ae568625a9927f302b8b14196f48382d4173562ed99cb3803fcf5b5d4c784` | `6215a409b9999bc3c9a11a79a5c01da25fc429298423e3a7a4f519b3b4935dfe` |
| lint | `5bede2307d46c1e526205149ee4fba98eb5bde4d0340d94a39db70ca4c3d9c6e` | `e22f7453b98f9b1edf46eb8c678055dc6871c1e3195ddf7cd9fbe5940b02098c` |
| 最终 RV64 focused | `65665720c1c7c57d434aa99dd32bda6949ba1405881db15733f5216df7496e5f` | `d1e34b9b2906ff7e8e65b3c799e2523bb919fbe0a5b8ed834f2841932f40c640` |
| 最终 LA64 focused | `dcf501159585f4532445b607c07e25c3cfd7f9a383ddaf52ff445c53ddac846d` | `fa1f32456229f313ebdcf50aa84a7accc9492dd63dcc4756b944f03876cee533` |

最终判定：B28 的受控 AP 用户态闭环功能证据通过；仓库级 lint 仍使阶段保持 `partial`。
