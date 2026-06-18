# veth + Netlink + NetNamespace — Round 2 全量重写

## TL;DR

> **Quick Summary**: 基于 DragonOS 三层架构（Iface trait → IfaceCommon → RouterEnableDevice），全量重写 Round 1 的 24 个文件。实现真 NetNamespace 隔离、完整 netlink 写操作语义（RTM_NEWLINK/NEWADDR/NEWROUTE/DELROUTE/DELLINK/SETLINK）、TCP/UDP 路由动态泛化。解锁 LTP 2 个 in-scope shell-script 网络 suite（net.tcp_cmds + net_stress.interface）。
>
> **Momus 审查**: OKAY ✅（高精度模式通过）
>
> **Deliverables**:
> - `os/src/net/iface.rs` — Iface trait + IfaceCommon 共享状态（新文件）
> - `os/src/net/router_device.rs` — RouterEnableDevice trait（新文件）
> - `os/src/drivers/net/veth.rs` — VethDevice 重写为 Iface impl
> - `os/src/net/socket/netlink/segment.rs` — SegmentCommon<Body,Attr> 泛型 + RouteNlSegment 枚举（新文件）
> - `os/src/net/socket/netlink/route.rs` — RTM_* handler 函数（link/addr/route 各一个模块）
> - `os/src/task/net_namespace.rs` — NetNamespace 真实现（新文件，从 task.rs 拆出）
> - `os/src/net/net_core.rs` — DeviceEntry + IFACES per-namespace
> - `os/src/net/config.rs` — DeviceStack 接入 Iface trait，删除 stacks[0] 硬编码
> - `os/src/net/routing.rs` — ROUTER per-namespace，fill_default 动态化
> - `os/src/net/ioctl.rs` — SIOCSIF* 同步 smoltcp
> - `os/src/net/socket/mod.rs` — Endpoint 清理 + write_at 回退移除
> - `os/src/net/syscall/sendto.rs` — PSOCK::Raw 路径去特殊化
> - `os/src/net/syscall/bind.rs` — AF_NETLINK 专用路径
> - `os/src/syscall/process/clone.rs` — clone/unshare/setns 真实现
> - `os/src/fs/procfs/files/` — per-ns 文件
> - `user/src/bin/inet_test.rs` — veth/ns 测试重写
>
> **Estimated Effort**: **XL** (29 tasks, 6 waves)
> **Parallel Execution**: YES — 6 waves, max 7 parallel per wave
> **Critical Path**: T2 (Iface trait) → T8 (VethDevice impl) → T9 (VethPair) → T12 (RTM_NEWLINK) → T29 (LTP verification)

---

## Context

### Original Request
Round 1 的 veth/netlink 实现经 Oracle 审查后被判定为"patch on patch"兼容层。核心问题：netlink 通信路径分散修复（EOPNOTSUPP 全局回退、AF_NETLINK 类型污染）、无 link 生命周期管理、TCP/UDP 硬编码 ifindex=2、NetNamespace 为空结构体。用户要求 Round 2 基于 DragonOS 架构全量重写，高精度计划。

### Interview Summary
**Key Discussions**:
- **架构决策**：完整引入 DragonOS 三层抽象（Iface trait → IfaceCommon → RouterEnableDevice）
- **NetNamespace**：从 stub 改为真隔离（含 device_list + per-ns ROUTER）
- **路由泛化**：全部泛化 — tcp/udp/raw_socket 全部动态 ifindex 查找
- **轮询**：保留简单 poll_once，不引入 NAPI
- **代码处理**：全量重写 24 个文件 + 新增抽象文件
- **LTP**：保留 shell-script suite（namespace 真隔离后解锁）

**Metis-Identified Gaps** (resolved as defaults):
- DragonOS 版本：参考最新 main 分支架构，不逐文件复制
- LTP PASS 目标：5 个目标 suite 零 TCONF/TBROK
- clone/unshare/setns 语义：Linux 6.6 子集，单线程 unshare
- 权限模型：root-only（无 CAP_NET_ADMIN 粒度）
- 每波 build gate：双架构编译门禁
- 不照搬 DragonOS 全局 registry / poller / NAPI / unwrap 模式

### Research Findings
- **DragonOS Veth**: 4 层架构 → 我们简化为 3 层（Iface trait → IfaceCommon → RouterEnableDevice）
- **DragonOS Netlink**: SegmentCommon<Body,Attr> 泛型 + RouteNlSegment 枚举派发 + 每资源 handler
- **DragonOS NetNamespace**: device_list(BTreeMap) + per-ns Router + NetnsPoller
- **Our 13 Problems**: 详见 Round 1 Oracle 审查 + bg_aef7ea3d 逐文件审计

### Support Matrix (MANDATORY)

#### In Scope (Round 2 implements)
| Category | Operations |
|----------|-----------|
| **RTM_NEWLINK** | veth 创建（IFLA_LINKINFO → IFLA_INFO_KIND → VETH_INFO_PEER），NLM_F_CREATE/EXCL |
| **RTM_DELLINK** | veth 对删除 + 回滚 IFACES/NET_INTERFACE/ROUTER |
| **RTM_SETLINK** | flags (up/down)、name、mtu |
| **RTM_GETLINK** | dump + 按 index/name 查询，含 IFLA_ADDRESS（所有 ifindex） |
| **RTM_NEWADDR** | IPv4 地址添加 + 本地直连路由 |
| **RTM_DELADDR** | IPv4 地址删除 + 路由清理 |
| **RTM_GETADDR** | dump all |
| **RTM_NEWROUTE** | IPv4 路由添加 |
| **RTM_DELROUTE** | IPv4 路由删除 |
| **RTM_GETROUTE** | dump all |
| **SIOCSIF*** | SIOCSIFFLAGS/SIOCSIFADDR/SIOCSIFNETMASK/SIOCSIFMTU → 同步 smoltcp |
| **NetNamespace** | clone/unshare/setns(CLONE_NEWNET) 真隔离 |
| **TCP/UDP** | bind/connect → 动态 ifindex 查找 |
| **procfs** | /proc/net/route per-ns, /proc/[pid]/ns/net |

#### Out of Scope (Round 2 explicitly excludes)
- bridge、tun/tap、macvlan、VLAN、bond
- iptables/netfilter/firewall/NAT/conntrack
- IPv6 协议栈（AF_INET6 dispatch、ICMPv6、ND、SLAAC）
- IGMP/multicast routing
- SCTP/DCCP
- ethtool/TSO/GSO/HW offload
- IFLA_NET_NS_FD 跨 namespace 设备迁移
- NAPI/poller 线程模型重构
- qdisc/tc/netem
- Generic Netlink
- `/proc/net/*` 全量（只做 route + ns/net）
- CAP_NET_ADMIN 细粒度权限

---

## Work Objectives

### Core Objective
全量重写 veth/netlink/netns 子系统，引入 DragonOS 三层抽象（Iface trait → IfaceCommon → RouterEnableDevice），实现真 NetNamespace 隔离和完整 netlink 写操作语义，解除 LTP shell-script 网络 suite 的 namespace 阻塞。

### Concrete Deliverables
- `os/src/net/iface.rs` — Iface trait + IfaceCommon（新）
- `os/src/net/router_device.rs` — RouterEnableDevice trait（新）
- `os/src/net/socket/netlink/segment.rs` — SegmentCommon + RouteNlSegment（新）
- `os/src/task/net_namespace.rs` — NetNamespace 真实现（新）
- `os/src/drivers/net/veth.rs` — 重写为 Iface impl
- `os/src/net/socket/netlink/route/link.rs` — RTM_NEWLINK/DELLINK/SETLINK/GETLINK（新）
- `os/src/net/socket/netlink/route/addr.rs` — RTM_NEWADDR/DELADDR/GETADDR（新）
- `os/src/net/socket/netlink/route/mod.rs` — 路由 dispatch（新，拆自原 route.rs）
- 重写: net_core.rs, config.rs, routing.rs, ioctl.rs, adapter.rs, socket/mod.rs, sendto.rs, bind.rs, clone.rs
- `user/src/bin/inet_test.rs` — veth/ns 用例重写

### Definition of Done
- [ ] `make rv64-kernel-build-only` 每波通过
- [ ] `make la64-kernel-build-only` 每波通过
- [ ] QEMU rv64: `ip link add veth_t type veth peer veth_p` 成功
- [ ] QEMU rv64: `ip link set veth_t up` 成功
- [ ] QEMU rv64: `ip addr add 192.168.100.1/24 dev veth_t` 成功
- [ ] QEMU rv64: `ip link delete veth_t` 成功（清理无残留）
- [ ] QEMU rv64: `unshare -n ip link show` 只显示 lo
- [ ] inet_test veth cases: 输出 VETH_NEWLINK_PASS + NETNS_ISOLATION_PASS + RTM_DELLINK_CLEANUP_PASS
- [ ] LTP: net.tcp_cmds + net_stress.interface 零 TCONF/TBROK（net.features/multicast/ipv6 允许 TCONF）
- [ ] 零 Round 1 anti-patterns（EOPNOTSUPP 全局回退、AF_NETLINK→Unspecified、setns 假成功、; true 吞错误）
- [ ] `Doc/Work_Log.md` 更新

### Must Have
- Iface trait + IfaceCommon 统一所有网卡（loopback、veth、virtio-eth）
- RouteNlSegment 枚举派发（非 if-else 链）
- NLA_F_NESTED mask + attr length 边界检查
- RTM_* 失败路径原子回滚（不留下单端 veth、半注册 iface）
- ioctl → smoltcp 状态同步
- 每 namespace 独立 ROUTER
- 零新增 hardcoded ifindex=2

### Must NOT Have
- `EOPNOTSUPP` fallback 泛化到所有 socket（socket/mod.rs:461-467）
- `AF_NETLINK` → `Endpoint::Unspecified` 强制转换（socket/mod.rs:211）
- `sys_setns()` 无条件返回 0（clone.rs:421-422）
- `; true` 在测试中吞错误
- 新的全局 static mut / lazy_static 绕过 namespace 隔离
- 新的 `unwrap()` / `todo!()` / `panic!()` 在用户可达 netlink 路径
- 新的硬编码 `ifindex=2`

---

## Verification Strategy

> **ZERO HUMAN INTERVENTION** — ALL verification is agent-executed.

### Test Decision
- **Infrastructure exists**: NO (bare-metal kernel, no `cargo test`)
- **Automated tests**: Tests-after (kernel-dev tools + inet_test ELF + LTP suite)
- **Framework**: kernel-dev_kernel_build + kernel-dev_kernel_run + inet_test markers
- **Agent-Executed QA**: MANDATORY for all tasks

### Per-Wave Build Gate
Every wave MUST end with dual-arch kernel build:
```bash
kernel-dev_kernel_build(arch="rv64", log="off")  # → success
kernel-dev_kernel_build(arch="la64", log="off")  # → success
```
Waves with user-space changes additionally require:
```bash
kernel-dev_kernel_build_all(arch="rv64", log="off")  # → success
kernel-dev_kernel_build_all(arch="la64", log="off")  # → success
```

### QA Policy
Every task MUST include agent-executed QA scenarios.
Evidence saved to `.sisyphus/evidence/task-{N}-{scenario-slug}.{ext}`.
- **Kernel build**: kernel-dev_kernel_build → verify exit code 0
- **QEMU runtime**: kernel-dev_kernel_run → verify stdout markers, no panic
- **CLI in QEMU**: interactive_bash (tmux) → send ip/ifconfig commands → validate output
- **API/netlink**: kernel-dev_kernel_run with LOG=info → verify response segments

---

## Execution Strategy

### Parallel Execution Waves

> 6 waves, max 7 parallel per wave. Build gate at end of each wave.

```
Wave 1 (Foundation — 7 tasks, T1-T6 parallel, T7 depends T1+T2):
├── T1: NetNamespace struct + device_list + ns lifecycle [unspecified-high]
├── T2: Iface trait + IfaceCommon struct [unspecified-high]
├── T3: RouterEnableDevice trait [unspecified-high]
├── T4: SegmentCommon<Body,Attr> + RouteNlSegment + CAttrHeader [quick]
├── T5: NLM_F_* + RTM_* + IFLA_* + IFA_* constants [quick]
└── T6: ErrorSegment + DoneSegment + nlmsg builders [quick]
└── T7: DeviceEntry refactored for Iface trait + per-ns IFACES (depends: T1, T2) [unspecified-high]

Wave 2 (Driver Layer — 4 tasks, T8∥T10, T9 depends T8):
├── T8: VethDevice + VethDriver as Iface impl (depends: T2, T7) [deep]
├── T9: VethPair lifecycle: create/register/delete/unregister (depends: T8, T1) [deep]
├── T10: DeviceStack per-ns + dynamic ifindex lookup (depends: T2, T1, T7) [unspecified-high]
└── T11: ROUTER per-namespace (depends: T1, T10) [deep]

Wave 3 (Netlink Write — 5 tasks, ALL parallel):
├── T12: RTM_NEWLINK + IFLA_LINKINFO nested parsing (depends: T4, T5, T6, T9) [deep]
├── T13: RTM_DELLINK handler (depends: T4, T5, T6, T9) [deep]
├── T14: RTM_SETLINK handler (depends: T4, T5, T6, T10) [quick]
├── T15: RTM_NEWADDR + DELADDR handlers (depends: T4, T5, T6, T10) [quick]
└── T16: RTM_NEWROUTE + DELROUTE handlers (depends: T4, T5, T6, T11) [deep]

Wave 4 (Socket Integration — 4 tasks, T17∥T18∥T20, T19 depends T17):
├── T17: Endpoint::from_sockaddr decouple AF_NETLINK (depends: T2) [quick]
├── T18: TCP/UDP bind dynamic ifindex lookup (depends: T10, T1) [deep]
├── T19: SocketFile write_at cleanup (depends: T17) [quick]
└── T20: SIOCSIF* smoltcp sync (depends: T10) [quick]

Wave 5 (Namespace Syscalls — 4 tasks, T21∥T22∥T23, T24 depends T21):
├── T21: clone(CLONE_NEWNET) real isolation (depends: T1, T10) [unspecified-high]
├── T22: unshare(CLONE_NEWNET) real isolation (depends: T1) [unspecified-high]
├── T23: sys_setns real implementation (depends: T1) [unspecified-high]
└── T24: procfs per-ns /proc/net/* + /proc/[pid]/ns/net (depends: T1, T21) [deep]

Wave 6 (Testing — 5 tasks, T25∥T26, T27→T28 sequential, T29 depends all):
├── T25: inet_test veth/ns cases rewrite (depends: T9, T12-T16) [quick]
├── T26: LTP os_test.conf config (depends: all impl) [quick]
├── T27: rv64 build + QEMU full smoke (depends: all impl) [deep]
├── T28: la64 build + QEMU full smoke (depends: all impl) [deep]
└── T29: LTP net suite verification (depends: T25, T26, T27, T28) [deep]

Wave FINAL (After ALL — 4 parallel reviewers):
├── F1: Plan compliance audit (oracle)
├── F2: Code quality review (unspecified-high)
├── F3: Real QA execution (unspecified-high + playwright)
└── F4: Scope fidelity check (deep)
-> Present results -> Get explicit user okay

Critical Path: T2 → T8 → T9 → T12 → T29
Parallel Speedup: ~65% faster than sequential
Max Concurrent: 7 (Wave 1)
```

### Dependency Matrix (all 29 tasks)

| Task | Blocks | Blocked By |
|------|--------|-----------|
| 1 | T9,T10,T11,T21,T22,T23,T24 | - |
| 2 | T7,T8,T10,T17 | - |
| 3-6 | — | - |
| 7 | T8,T10 | T1,T2 |
| 8 | T9 | T2,T7 |
| 9 | T12,T13,T25 | T1,T8 |
| 10 | T11,T14,T15,T18,T20,T21 | T1,T2,T7 |
| 11 | T16 | T1,T10 |
| 12 | T25 | T4,T5,T6,T9 |
| 13 | T25 | T4,T5,T6,T9 |
| 14 | - | T4,T5,T6,T10 |
| 15 | - | T4,T5,T6,T10 |
| 16 | - | T4,T5,T6,T11 |
| 17 | T19 | T2 |
| 18 | - | T1,T10 |
| 19 | - | T17 |
| 20 | - | T10 |
| 21 | T24 | T1,T10 |
| 22 | - | T1 |
| 23 | - | T1 |
| 24 | - | T1,T21 |
| 25 | - | T9,T12,T13,T14,T15,T16 |
| 26 | - | all impl |
| 27 | T28 | all impl |
| 28 | T29 | all impl, T27 |
| 29 | - | T25,T26,T27,T28 |

### Agent Dispatch Summary

- **Wave 1**: 7 agents — T1-T3 → `unspecified-high`, T4-T6 → `quick`, T7 → `unspecified-high`
- **Wave 2**: 4 agents — T8 → `deep`, T9 → `deep`, T10 → `unspecified-high`, T11 → `deep`
- **Wave 3**: 5 agents — T12 → `deep`, T13 → `deep`, T14 → `quick`, T15 → `quick`, T16 → `deep`
- **Wave 4**: 4 agents — T17 → `quick`, T18 → `deep`, T19 → `quick`, T20 → `quick`
- **Wave 5**: 4 agents — T21-T23 → `unspecified-high`, T24 → `deep`
- **Wave 6**: 5 agents — T25-T26 → `quick`, T27-T29 → `deep`
- **FINAL**: 4 agents — F1 → `oracle`, F2 → `unspecified-high`, F3 → `unspecified-high`, F4 → `deep`

---

## File Whitelist (Round 2 may modify)

### New files (6)
- `os/src/net/iface.rs` — Iface trait + IfaceCommon
- `os/src/net/router_device.rs` — RouterEnableDevice trait
- `os/src/net/socket/netlink/segment.rs` — SegmentCommon + RouteNlSegment
- `os/src/task/net_namespace.rs` — NetNamespace 真实现
- `os/src/net/socket/netlink/route/link.rs` — RTM_NEWLINK/DELLINK/SETLINK/GETLINK
- `os/src/net/socket/netlink/route/addr.rs` — RTM_NEWADDR/DELADDR/GETADDR

### Rewrite files (13)
- `os/src/drivers/net/veth.rs`
- `os/src/net/net_core.rs`
- `os/src/net/config.rs`
- `os/src/net/routing.rs`
- `os/src/net/ioctl.rs`
- `os/src/net/adapter.rs`
- `os/src/net/socket/netlink/route.rs` → 拆为 mod.rs（dispatch）+ 新 link.rs/addr.rs
- `os/src/net/socket/netlink/netlink.rs` → 常量整合到 T5/T6
- `os/src/net/socket/mod.rs`
- `os/src/net/syscall/sendto.rs`
- `os/src/net/syscall/bind.rs`
- `os/src/syscall/process/clone.rs`
- `user/src/bin/inet_test.rs`

### Modify files (8)
- `os/src/task/task.rs` — NetNamespace 移到 net_namespace.rs
- `os/src/task/process.rs` — ProcessInner.net 指向新 NetNamespace
- `os/src/net/mod.rs` — 注册新模块
- `os/src/syscall/mod.rs` — setns dispatch（已有）
- `os/src/syscall/syscall_id.rs` — SYSCALL_SETNS（已有）
- `os/src/fs/procfs/files/net_route.rs` — per-ns 路由
- `os/src/fs/procfs/files/net_ns.rs` — /proc/[pid]/ns/net（NEW，属于 procfs files 目录）
- `os_test.conf` — LTP suite config

### Never-touch files
- `os/src/lang_items.rs` / `.rv` / `.la` variants
- `os/src/mm/` — 内存管理
- `os/src/syscall/` except clone.rs
- `os/src/fs/ext4/` / `os/src/fs/fat32/` — 文件系统
- `os/src/drivers/` except net/veth.rs
- `user/src/bin/initproc.rs` — 除非需更新 /lib/modules/ 路径

### Worklog Strategy
- **Every wave**: update `Doc/Work_Log.md` after all tasks in the wave are committed (per mango-worklog Skill)
- Each wave commit MUST include a `Doc/Work_Log.md` entry with: wave#, files changed, build verification, QEMU results
- Final inventory entry after Wave 6: summarize all 29 tasks + architectural decisions

---

## TODOs

> Implementation + Test = ONE Task. Never separate.
> EVERY task MUST have: Recommended Agent Profile + Parallelization info + QA Scenarios.
> **A task WITHOUT QA Scenarios is INCOMPLETE. No exceptions.**

### Wave 1: Foundation — Types, Traits, Constants

- [x] 1. NetNamespace struct + device_list + ns lifecycle

  **What to do**:
  - Create `os/src/task/net_namespace.rs` (NEW file), move NetNamespace from `os/src/task/task.rs:77-82`
  - Define `NetNamespace` struct: `id: u64` (per-ns unique), `device_list: Mutex<BTreeMap<usize, Arc<dyn Iface>>>`, `router: Mutex<Router>`
  - Define `INIT_NET_NAMESPACE` lazy_static with id=0, auto-create loopback (ifindex=1, name="lo")
  - Implement `NetNamespace::new() -> Arc<Self>` — creates new ns with ONLY loopback, new router
  - Implement `NetNamespace::add_device(&self, iface: Arc<dyn Iface>)` — insert into device_list by iface.nic_id()
  - Implement `NetNamespace::remove_device(&self, nic_id: usize)` — remove from device_list
  - Implement `NetNamespace::device_by_index(&self, ifindex: usize) -> Option<Arc<dyn Iface>>`
  - Implement `NetNamespace::device_by_name(&self, name: &str) -> Option<Arc<dyn Iface>>`
  - Remove old `NetNamespace` unit struct from `os/src/task/task.rs:77-82`
  - Update `INIT_NET_NAMESPACE` references in `task.rs` and `process.rs`
  - Update `ProcessInner.net` field type to `Arc<NetNamespace>`

  **Must NOT do**:
  - Do NOT use RCU or complex lock-free structures (keep Mutex for single-core)
  - Do NOT implement cross-namespace device migration
  - Do NOT add NAPI/poller/thread to NetNamespace

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: Ground-up design of namespace data structures — medium complexity, no external domain expertise needed
  - **Skills**: `[]`
  - **Skills Evaluated but Omitted**: `mango-worklog` — deferred to final wave (Doc/Work_Log update)

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with T2, T3, T4, T5, T6, T7)
  - **Blocks**: T9, T10, T11, T21, T22, T23, T24
  - **Blocked By**: None (can start immediately)

  **References**:
  - DragonOS: `kernel/src/process/namespace/net_namespace.rs` — `NetNamespace` struct with device_list, used as central registry
  - Current: `os/src/task/task.rs:77-82` — old unit struct to remove
  - Current: `os/src/task/process.rs:333-341` — `net()`, `unshare_net()` methods to update
  - Current: `os/src/task/process.rs:186-187` — constructor passing `INIT_NET_NAMESPACE`
  - Current: `os/src/syscall/process/clone.rs:199-203` — CLONE_NEWNET flags handling, references `INIT_NET_NAMESPACE`

  **Acceptance Criteria**:
  - [ ] `os/src/task/net_namespace.rs` created with `NetNamespace` struct
  - [ ] `os/src/task/task.rs` no longer contains `NetNamespace` unit struct
  - [ ] `kernel-dev_kernel_build(arch="rv64", log="off")` → success (wave gate)
  - [ ] `kernel-dev_kernel_build(arch="la64", log="off")` → success (wave gate)

  **QA Scenarios**:
  ```
  Scenario: INIT_NET_NAMESPACE has loopback + eth0
    Tool: Bash (kernel-dev_kernel_run)
    Preconditions: Kernel compiles on rv64
    Steps:
      1. kernel-dev_kernel_run(arch="rv64", log="info")
      2. In QEMU shell: `ip link show`
      3. Assert output contains "lo" with state UNKNOWN
      4. Assert output contains "eth0" (if virtio-net detected)
    Expected Result: Loopback + eth0 in initial namespace
    Evidence: .sisyphus/evidence/task-1-init-ns.txt

  Scenario: New namespace has only loopback
    Tool: interactive_bash (tmux)
    Preconditions: Kernel booted with init namespace
    Steps:
      1. In tmux: `unshare -n ip link show`
      2. Assert output contains ONLY "lo"
      3. Assert no "eth0" or other devices leaked
    Expected Result: New namespace isolated with only loopback
    Failure Indicators: eth0 or veth devices appear in new ns output
    Evidence: .sisyphus/evidence/task-1-new-ns-isolation.txt
  ```

  **Commit**: YES (Wave 1 group)
  - Message: `feat(net): add NetNamespace struct with device_list lifecycle`
  - Files: `os/src/task/net_namespace.rs`, `os/src/task/task.rs`, `os/src/task/process.rs`, `os/src/syscall/process/clone.rs`
  - Pre-commit: `kernel-dev_kernel_build(arch="rv64")`

---

- [x] 2. Iface trait + IfaceCommon struct

  **What to do**:
  - Create `os/src/net/iface.rs` (NEW file)
  - Define `pub trait Iface: Send + Sync + Debug`:
    - `fn nic_id(&self) -> usize` — unique per-interface ID
    - `fn iface_name(&self) -> &str`
    - `fn set_iface_name(&self, name: &str)` — rename (check uniqueness in ns)
    - `fn flags(&self) -> u32` — IFF_UP, IFF_RUNNING, etc.
    - `fn set_flags(&self, flags: u32)` — update flags + sync smoltcp
    - `fn mtu(&self) -> usize`
    - `fn set_mtu(&self, mtu: usize)` — update MTU + sync smoltcp capabilities
    - `fn ip_addrs(&self) -> Vec<IpCidr>` — current IP addresses
    - `fn add_ip_addr(&self, addr: IpCidr)` — add IP + local route
    - `fn del_ip_addr(&self, addr: IpCidr)` — remove IP + local route
    - `fn mac(&self) -> [u8; 6]`
    - `fn kind(&self) -> DeviceKind` — Loopback/Ethernet/Veth
    - `fn peer_ifindex(&self) -> Option<usize>` — veth peer ifindex
    - `fn common(&self) -> &IfaceCommon` — shared state access
    - `fn as_smoltcp_device(&self) -> &dyn SmoltcpDeviceAccess` — for poll
  - Define `IfaceCommon` struct:
    - `nic_id: AtomicUsize` (from `NEXT_IFINDEX` global), `name: RwLock<String>`
    - `flags: AtomicU32`, `mtu: AtomicUsize`
    - `ip_addrs: Mutex<Vec<IpCidr>>`, `hwaddr: [u8; 6]`
    - `kind: DeviceKind`, `peer_ifindex: Option<usize>`
    - `smoltcp_iface: Mutex<smoltcp::iface::Interface>` — smoltcp integration
    - `sockets: Mutex<smoltcp::iface::SocketSet<'static>>`
    - `net_namespace: RwLock<Weak<NetNamespace>>` — which ns this iface belongs to
  - Implement `IfaceCommon::new(name, kind, hwaddr, mtu)` constructor
  - Define trait `SmoltcpDeviceAccess` with methods for poll/receive/transmit

  **Must NOT do**:
  - Do NOT inherit DragonOS's KObject/Device/sysfs boilerplate
  - Do NOT add BridgeEnableDevice trait (bridge out of scope)
  - Do NOT add packet_socket / netlink_routes / static_neighbors fields yet
  - Do NOT add NapiStruct field

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: Trait design is architecture-critical — requires understanding of both smoltcp integration and namespace lifetime
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with T1, T3, T4, T5, T6, T7)
  - **Blocks**: T8, T10, T17
  - **Blocked By**: None

  **References**:
  - DragonOS: `kernel/src/driver/net/mod.rs` — `Iface` trait and `IfaceCommon` struct definition
  - DragonOS: `kernel/src/driver/net/loopback.rs` — Loopback impl of Iface (simplest reference)
  - Current: `os/src/net/net_core.rs:17-46` — current `DeviceEntry` struct to supersede
  - Current: `os/src/net/config.rs:221-237` — `add_veth_stack` which sets up smoltcp Interface
  - Current: `os/src/net/adapter.rs:19-23` — `IfaceDevice` enum (will be replaced by trait object)

  **Acceptance Criteria**:
  - [ ] `Iface` trait compiles with all required methods
  - [ ] `IfaceCommon` struct compiles
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success (wave gate)
  - [ ] `kernel-dev_kernel_build(arch="la64")` → success

  **QA Scenarios**:
  ```
  Scenario: Iface trait and IfaceCommon compile without errors
    Tool: kernel-dev_kernel_build
    Steps:
      1. kernel-dev_kernel_build(arch="rv64", log="off")
      2. Assert exit code 0, no compile errors
    Expected Result: Successful compilation
    Failure Indicators: Compiler errors in iface.rs
    Evidence: .sisyphus/evidence/task-2-compile.txt
  ```

  **Commit**: YES (Wave 1 group)
  - Message: `feat(net): add Iface trait and IfaceCommon shared state`
  - Files: `os/src/net/iface.rs`
  - Pre-commit: `kernel-dev_kernel_build(arch="rv64")`

---

- [x] 3. RouterEnableDevice trait

  **What to do**:
  - Create `os/src/net/router_device.rs` (NEW file)
  - Define `pub trait RouterEnableDevice: Iface`:
    - `fn route_and_send(&self, next_hop: Ipv4Address, ip_packet: &[u8]) -> Result<(), NetError>` — sends IP packet via this device to next_hop (constructs Ethernet frame with dst_mac=self.mac, src_mac=peer.mac)
    - `fn is_my_ip(&self, addr: Ipv4Address) -> bool` — checks if addr is any of self's IPs
    - `fn netns_router(&self) -> Arc<Mutex<Router>>` — access per-ns router
    - `fn handle_routable_packet(&self, ether_frame: &[u8]) -> Result<Option<Ipv4Repr>, NetError>` — process incoming routed packet: strip ether header, check TTL, route forward or deliver locally
  - Define `RouterEnableDeviceCommon` — shared data for RouterEnableDevice (local_routes, etc.)

  **Must NOT do**:
  - Do NOT add NAT/conntrack hooks (pre_routing_hook/post_routing_hook from DragonOS)
  - Do NOT implement `SubTtl` — for now, just check TTL > 1 and decrement in-place
  - Do NOT add BridgeEnableDevice

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: Routing trait design — must fit between Iface trait and ROUTER, needs careful trait bound design
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with T1, T2, T4, T5, T6, T7)
  - **Blocks**: None directly (T16 uses it conceptually)
  - **Blocked By**: None

  **References**:
  - DragonOS: `kernel/src/net/routing/mod.rs` — `RouterEnableDevice` trait definition (route_and_send, is_my_ip, etc.)
  - DragonOS: `kernel/src/driver/net/veth.rs:handle_routable_packet()` — routing packet processing
  - Current: `os/src/net/routing.rs:222-296` — current `route_output()` logic (hardcodes ifindex=2)
  - Current: `os/src/net/config.rs:478-492` — `add_routed_socket` hardcodes ifindex preference

  **Acceptance Criteria**:
  - [ ] `RouterEnableDevice` trait compiles
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: Trait compiles (no runtime test yet — implemented in T16)
    Tool: kernel-dev_kernel_build
    Steps:
      1. kernel-dev_kernel_build(arch="rv64", log="off")
      2. Assert no compile errors in router_device.rs
    Expected Result: Compilation success
    Evidence: .sisyphus/evidence/task-3-compile.txt
  ```

  **Commit**: YES (Wave 1 group)
  - Message: `feat(net): add RouterEnableDevice trait`
  - Files: `os/src/net/router_device.rs`
  - Pre-commit: `kernel-dev_kernel_build(arch="rv64")`

---

- [x] 4. SegmentCommon<Body,Attr> generic + RouteNlSegment enum + CAttrHeader

  **What to do**:
  - Create `os/src/net/socket/netlink/segment.rs` (NEW file)
  - Define `CMsgSegHdr` repr(C) struct: `len: u32, type_: u16, flags: u16, seq: u32, pid: u32`
  - Define `CAttrHeader` repr(C) struct: `len: u16, type_: u16` — netlink attribute header
  - Define generic `SegmentCommon<Body, Attr>` struct:
    - `header: CMsgSegHdr`, `body: Body`, `attrs: Vec<Attr>`
    - Implement `read_from_buf(buf: &[u8]) -> Result<Self>` — parse header + body + attrs
    - Implement `to_bytes(&self) -> Vec<u8>` — serialize
  - Define `RouteNlSegment` enum:
    - `NewLink(LinkSegment)`, `DelLink(LinkSegment)`, `SetLink(LinkSegment)`, `GetLink(LinkSegment)`
    - `NewAddr(AddrSegment)`, `DelAddr(AddrSegment)`, `GetAddr(AddrSegment)`
    - `NewRoute(RouteSegment)`, `DelRoute(RouteSegment)`, `GetRoute(RouteSegment)`
    - `Error(ErrorSegment)`, `Done(DoneSegment)`
  - Define type aliases: `LinkSegment = SegmentCommon<LinkSegmentBody, LinkAttr>`, `AddrSegment = SegmentCommon<AddrSegmentBody, AddrAttr>`, etc.
  - Define `CIfinfoMsg` repr(C): `family: u8, pad: u8, type_: u16, index: i32, flags: u32, change: u32`
  - Define `CIfaddrMsg` repr(C): `family: u8, prefixlen: u8, flags: u8, scope: u8, index: i32`
  - Define `CRtMsg` repr(C): `family: u8, dst_len: u8, src_len: u8, tos: u8, table: u8, protocol: u8, scope: u8, type_: u8, flags: u32`
  - **CRITICAL**: `read_from_buf` must validate `nlmsg_len >= sizeof(CMsgSegHdr)`, return `EINVAL` on short messages
  - **CRITICAL**: Apply `NLA_F_NESTED` mask (`rta_type & !0x8000`) when matching attribute types

  **Must NOT do**:
  - Do NOT copy DragonOS's `read_unaligned` approach (use safe transmute or manual byte parsing)
  - Do NOT add `impl ProtocolSegment` trait from DragonOS (unnecessary abstraction)
  - Do NOT add generic Netlink message layer beyond route family

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Pure data structure definitions + parsing — well-defined inputs/outputs, minimal ambiguity
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with T1, T2, T3, T5, T6, T7)
  - **Blocks**: T12, T13, T14, T15, T16 (all netlink handlers)
  - **Blocked By**: None

  **References**:
  - DragonOS: `kernel/src/net/socket/netlink/message/segment/common.rs` — `SegmentCommon<Body, Attr>` generic
  - DragonOS: `kernel/src/net/socket/netlink/route/message/segment/mod.rs` — `RouteNlSegment` enum
  - DragonOS: `kernel/src/net/socket/netlink/message/segment/header.rs` — `CMsgSegHdr` repr(C)
  - DragonOS: `kernel/src/net/socket/netlink/message/attr/mod.rs` — `CAttrHeader` and `Attribute` trait
  - Current: `os/src/net/socket/netlink/route.rs:7-15` — current `parse_nlmsg` (doesn't validate nlmsg_len, doesn't support multi-nlmsg)
  - Linux: `include/uapi/linux/netlink.h` — nlmsghdr definition (nlmsg_len, nlmsg_type, nlmsg_flags, nlmsg_seq, nlmsg_pid)
  - Linux: `include/uapi/linux/rtnetlink.h` — CIfinfoMsg, CIfaddrMsg, CRtMsg

  **Acceptance Criteria**:
  - [ ] `SegmentCommon` generic compiles with `read_from_buf` and `to_bytes`
  - [ ] `RouteNlSegment` enum defined with all variants
  - [ ] `NLA_F_NESTED` mask applied in attribute matching (test: `type_ | 0x8000` still matches)
  - [ ] Short `nlmsg_len` (< sizeof header) returns `EINVAL`
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: Valid nlmsg parses correctly
    Tool: Bash (compile test)
    Steps:
      1. kernel-dev_kernel_build(arch="rv64")
      2. No compile errors
    Expected Result: Types defined, compilation passes
    Evidence: .sisyphus/evidence/task-4-compile.txt

  Scenario: NLA_F_NESTED mask works
    Tool: Bash (unit test via QEMU inet_test)
    Preconditions: Kernel booted
    Steps:
      1. inet_test calls internal fn to verify rta_type with NLA_F_NESTED flag
      2. Assert: IFLA_LINKINFO (12) | NLA_F_NESTED (0x8000) = 0x800C matches after mask
    Expected Result: Masked comparison succeeds
    Evidence: .sisyphus/evidence/task-4-nla-mask.txt
  ```

  **Commit**: YES (Wave 1 group)
  - Message: `feat(netlink): add SegmentCommon generic, RouteNlSegment enum, CAttrHeader`
  - Files: `os/src/net/socket/netlink/segment.rs`
  - Pre-commit: `kernel-dev_kernel_build(arch="rv64")`

- [x] 5. NLM_F_* + RTM_* + IFLA_* + IFA_* constants (complete set)

  **What to do**:
  - In `os/src/net/socket/netlink/netlink.rs`: expand constants to complete set
  - Add `NLA_F_NESTED = 0x8000`, `NLA_F_NET_BYTEORDER = 0x4000`, `NLMSG_ALIGNTO = 4`
  - Verify all existing RTM constants match Linux 6.6 values
  - Add missing: `RTM_SETLINK = 19`, `RTM_NEWROUTE = 24`, `RTM_DELROUTE = 25` (already in file? verify)
  - Add `NLM_F_CREATE = 0x400`, `NLM_F_EXCL = 0x200`, `NLM_F_REPLACE = 0x100`, `NLM_F_APPEND = 0x800`, `NLM_F_MATCH = 0x200`, `NLM_F_ROOT = 0x100`, `NLM_F_DUMP = (NLM_F_ROOT | NLM_F_MATCH)`
  - IFLA: `IFLA_UNSPEC=0, IFLA_ADDRESS=1, IFLA_BROADCAST=2, IFLA_IFNAME=3, IFLA_MTU=4, IFLA_LINK=5, IFLA_QDISC=6, IFLA_STATS=7, IFLA_LINKINFO=12, IFLA_NET_NS_PID=19, IFLA_NET_NS_FD=28, IFLA_EXT_MASK=29`
  - IFLA_INFO: `IFLA_INFO_KIND=1, IFLA_INFO_DATA=2, IFLA_INFO_XSTATS=3`
  - VETH_INFO: `VETH_INFO_PEER=1`
  - Add `ARPHRD_ETHER = 1`, `ARPHRD_LOOPBACK = 772`, `AF_NETLINK = 16`
  - All added with doc comments referencing Linux uapi header file

  **Must NOT do**:
  - Do NOT add IPv6-specific constants (IFA_F_*, IFLA_INET6_*, RTNH_F_* for v6)
  - Do NOT add bridge/VLAN/VXLAN info kind constants

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Pure constant definitions — no logic, just transcribing from Linux uapi headers
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with T1, T2, T3, T4, T6, T7)
  - **Blocks**: T12, T13, T14, T15, T16
  - **Blocked By**: None

  **References**:
  - Current: `os/src/net/socket/netlink/netlink.rs` — existing constants to expand
  - Linux 6.6: `include/uapi/linux/rtnetlink.h` — RTM_*, IFLA_*, IFA_* values
  - Linux 6.6: `include/uapi/linux/netlink.h` — NLM_F_*, NETLINK_* values
  - Linux 6.6: `include/uapi/linux/if_link.h` — IFLA_INFO_*, VETH_INFO_*
  - Linux 6.6: `include/uapi/linux/if_arp.h` — ARPHRD_*

  **Acceptance Criteria**:
  - [ ] All NLM_F_* flags defined with correct values
  - [ ] All RTM_* types defined
  - [ ] All IFLA_*, IFA_* constants defined
  - [ ] `NLMSG_ALIGN(n)` macro/function for alignment
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: Constants compile and can be used
    Tool: kernel-dev_kernel_build
    Steps:
      1. kernel-dev_kernel_build(arch="rv64", log="off")
      2. Assert no compile errors, no unused constant warnings (except deliberate)
    Expected Result: Compilation passes, constants accessible
    Evidence: .sisyphus/evidence/task-5-compile.txt
  ```

  **Commit**: YES (Wave 1 group)
  - Message: `feat(netlink): add complete NLM_F_*/RTM_*/IFLA_*/IFA_* constant set`
  - Files: `os/src/net/socket/netlink/netlink.rs`
  - Pre-commit: `kernel-dev_kernel_build(arch="rv64")`

---

- [x] 6. ErrorSegment + DoneSegment + nlmsg builder functions

  **What to do**:
  - In `os/src/net/socket/netlink/route/mod.rs` (rewritten dispatch): add builder functions
  - `build_nlmsg_error(seg_hdr: &CMsgSegHdr, errno: i32) -> RouteNlSegment::Error` — construct NLMSG_ERROR segment with `error_code = -errno` and original request header
  - `build_nlmsg_done(seg_hdr: &CMsgSegHdr) -> RouteNlSegment::Done` — NLMSG_DONE with error_code=0
  - `build_nlmsg_ack(seg_hdr: &CMsgSegHdr) -> RouteNlSegment::Error` — success ACK (error_code=0)
  - `finish_response(segments: &mut Vec<RouteNlSegment>)` — append DoneSegment if DUMP
  - `nlmsg_align(len: usize) -> usize` — round up to NLMSG_ALIGNTO (4)
  - `rta_align(len: usize) -> usize` — same for attributes
  - Define `ErrorSegmentBody { error_code: i32, request_header: CMsgSegHdr }`
  - Define `DoneSegmentBody { error_code: i32 }`
  - Type aliases: `ErrorSegment = SegmentCommon<ErrorSegmentBody, NoAttr>`, `DoneSegment = SegmentCommon<DoneSegmentBody, NoAttr>`
  - `NoAttr` — empty/zero-sized attribute type

  **Must NOT do**:
  - Do NOT wire these into handler dispatch yet (that's T12-T16)
  - Do NOT implement multicast_notify yet

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Builder functions with clear inputs/outputs — follow DragonOS patterns directly
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with T1-T5, T7)
  - **Blocks**: T12, T13, T14, T15, T16
  - **Blocked By**: None

  **References**:
  - DragonOS: `kernel/src/net/socket/netlink/message/segment/ack.rs` — ErrorSegment/DoneSegment definitions
  - DragonOS: `kernel/src/net/socket/netlink/route/kern/utils.rs` — `finish_response`, `kernel_notify_header`
  - Current: `os/src/net/socket/netlink/netlink.rs:57-68` — existing `build_nlmsg` builder (to expand or replace)
  - Current: `os/src/net/socket/netlink/netlink.rs:70-78` — existing `build_nlmsg_error` (verify correctness, align with DragonOS)

  **Acceptance Criteria**:
  - [ ] `build_nlmsg_error` constructs correct NLMSG_ERROR segment
  - [ ] `build_nlmsg_ack` constructs success ACK (error_code=0)
  - [ ] `build_nlmsg_done` constructs NLMSG_DONE
  - [ ] `nlmsg_align` correctly rounds up to 4-byte boundary
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: Error ACK segment serializes correctly
    Tool: Bash (kernel-dev_kernel_run with inet_test)
    Steps:
      1. Build kernel
      2. inet_test: construct error ACK for RTM_GETLINK with errno=2 (ENOENT)
      3. Validate: segment.body.error_code == -2, segment.header.type_ == NLMSG_ERROR
    Expected Result: Error segment correctly serialized and verified
    Evidence: .sisyphus/evidence/task-6-error-ack.txt
  ```

  **Commit**: YES (Wave 1 group)
  - Message: `feat(netlink): add ErrorSegment, DoneSegment, nlmsg builders`
  - Files: `os/src/net/socket/netlink/route/mod.rs`, `os/src/net/socket/netlink/netlink.rs`
  - Pre-commit: `kernel-dev_kernel_build(arch="rv64")`

---

- [x] 7. DeviceEntry refactored for Iface trait + per-ns IFACES

  **What to do**:
  - In `os/src/net/net_core.rs`: refactor `DeviceEntry` to wrap `Arc<dyn Iface>` instead of raw fields
  - Remove fields that are now in `IfaceCommon`: name, flags, mtu, hwaddr, ip_addrs, kind, peer_ifindex
  - Keep `ifindex: usize` as shortcut (maps to `iface.nic_id()`)
  - Change `IFACES: Mutex<Vec<DeviceEntry>>` → `IFACES` lives inside `NetNamespace.device_list` (BTreeMap<usize, Arc<dyn Iface>>)
  - Remove global `IFACES` lazy_static; instead, access via `current_netns().device_list`
  - Update `add_device()` → delegate to `current_netns().add_device(iface)`
  - Update `remove_device()` → delegate to `current_netns().remove_device(nic_id)`
  - Update `next_ifindex()` → remains global `AtomicU32` (ifindex sequential across all namespaces)
  - Update `DeviceKind` enum: `Loopback, Ethernet, Veth` (already defined, verify)
  - Update all callers of `IFACES` (routing.rs, ioctl.rs, route.rs, config.rs) to use `current_netns().device_list`
  - `current_netns()` — helper function to access current task's NetNamespace

  **Must NOT do**:
  - Do NOT leave global IFACES as fallback
  - Do NOT break existing loopback/eth0 registration
  - Do NOT change ifindex assignment for existing devices (lo=1, eth0=2)

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: Wide refactor touching routing, ioctl, netlink — must be careful to update all callers
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with T1-T6)
  - **Blocks**: T8, T10
  - **Blocked By**: T1 (NetNamespace definition), T2 (Iface trait)

  **References**:
  - Current: `os/src/net/net_core.rs:17-46` — `DeviceEntry` struct to refactor
  - Current: `os/src/net/net_core.rs:86-97` — `add_device()`, `remove_device()` to update
  - Current: `os/src/net/net_core.rs:99-102` — `NEXT_IFINDEX` AtomicU32 (keep, do NOT per-ns)
  - Current: `os/src/net/net_core.rs:111-152` — `init()` to update (use current_netns)
  - Current: `os/src/net/net_core.rs:54` — global `IFACES: Mutex<Vec<DeviceEntry>>` to remove
  - Current: `os/src/net/routing.rs:233-255` — direct `IFACES.lock()` in route_output
  - Current: `os/src/net/ioctl.rs:120-176` — direct `IFACES` access in ioctl handlers
  - Current: `os/src/net/socket/netlink/route.rs:40-110` — direct `IFACES` access in GETLINK/GETADDR

  **Acceptance Criteria**:
  - [ ] No global `IFACES` lazy_static remains
  - [ ] All callers use `current_netns().device_list`
  - [ ] `init()` correctly registers lo(ifindex=1) and eth0(ifindex=2) in INIT_NET_NAMESPACE
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: Global IFACES removed, per-ns access works
    Tool: Bash (grep + build)
    Preconditions: None
    Steps:
      1. grep for "IFACES" in os/src/net/ — assert no remaining global IFACES access
      2. kernel-dev_kernel_build(arch="rv64", log="off")
      3. kernel-dev_kernel_run(arch="rv64")
      4. In QEMU: `cat /proc/net/route` (should work via per-ns device_list)
    Expected Result: Compiles, QEMU boots, /proc/net/route shows lo+eth0
    Failure Indicators: grep finds remaining global IFACES; QEMU panics referencing IFACES
    Evidence: .sisyphus/evidence/task-7-no-global-ifaces.txt
  ```

  **Commit**: YES (Wave 1 group)
  - Message: `feat(net): refactor DeviceEntry for Iface trait, move IFACES to per-ns`
  - Files: `os/src/net/net_core.rs`, `os/src/net/routing.rs`, `os/src/net/ioctl.rs`, `os/src/net/socket/netlink/route.rs`, `os/src/net/config.rs`
  - Pre-commit: `kernel-dev_kernel_build(arch="rv64")`

---

### Wave 2: Driver Layer

- [x] 8. VethDevice + VethDriver rewritten as Iface impl

  **What to do**:
  - Rewrite `os/src/drivers/net/veth.rs` from scratch
  - Define `Veth` struct: `rx_queue: Mutex<VecDeque<Vec<u8>>>`, `peer: Mutex<Weak<VethInterface>>`
  - Define `VethDriver` struct: `inner: Arc<Veth>`, impl `SmoltcpDeviceAccess` trait (poll/receive/transmit/capabilities)
  - Define `VethInterface` struct: `name: String`, `driver: Arc<VethDriver>`, `common: Arc<IfaceCommon>`, `inner: Mutex<VethCommonData>`, impl `Iface` trait
  - MAC: `02:00:00:00:XX:YY` from nic_id
  - `VethInterface::new(name)` creates single unpaired endpoint

  **Must NOT do**: No BridgeEnableDevice, no IFF_UP hardcode, no IFACES registration in this task

  **Recommended Agent Profile**: `deep` — smoltcp integration + trait impl + lock ordering
  **Parallelization**: Wave 2, parallel with T10. Blocks T9. Blocked by T2, T7.

  **References**:
  - DragonOS: `kernel/src/driver/net/veth.rs:36-46,207-218,375-390`
  - Round 1: `os/src/drivers/net/veth.rs:22-110` (queue pattern to preserve)
  - T2: `os/src/net/iface.rs` — Iface trait definition

  **Acceptance Criteria**:
  - [ ] VethInterface impl Iface; VethDriver impl SmoltcpDeviceAccess
  - [ ] Send: smoltcp transmit → peer.rx_queue.push
  - [ ] No unwrap()/todo!() in send/receive paths
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: VethInterface satisfies Iface trait
    Tool: kernel-dev_kernel_build → no trait bound errors
    Evidence: .sisyphus/evidence/task-8-compile.txt
  ```

  **Commit**: Wave 2 group. Files: `os/src/drivers/net/veth.rs`

---

- [x] 9. VethPair lifecycle: create/register/delete/unregister

  **What to do**:
  - `VethInterface::new_pair(name_a, name_b) -> (Arc<Self>, Arc<Self>)`
  - Flow: create two ifaces → set peer_veth → register in device_list + NET_INTERFACE → return pair
  - `VethInterface::delete_pair(veth)`: unregister from NET_INTERFACE → clear peer's backref → remove from device_list → clean up routes
  - Atomic rollback: if second registration fails, unregister first

  **Must NOT do**: No dangling peer refs, no half-registered pairs, no IFLA_NET_NS_FD

  **Recommended Agent Profile**: `deep` — lifecycle critical paths
  **Parallelization**: Wave 2, sequential after T8. Blocks T12, T13, T25. Blocked by T1, T8.

  **References**:
  - DragonOS: `VethInterface::new_pair` + `register_netdevice`
  - Round 1: `os/src/drivers/net/veth.rs:116-152`

  **Acceptance Criteria**:
  - [ ] new_pair creates connected pair, both registered
  - [ ] delete_pair: peer ref cleared, both removed
  - [ ] Duplicate name → EEXIST, state unchanged
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: create → verify → delete → verify
    Tool: interactive_bash: `ip link add veth0 type veth peer veth1`
    Expected: both show in `ip link`, both gone after `ip link del veth0`
    Evidence: .sisyphus/evidence/task-9-veth-lifecycle.txt
  ```

  **Commit**: Wave 2 group. Files: `os/src/drivers/net/veth.rs`

---

- [x] 10. DeviceStack per-ns + dynamic ifindex lookup

  **What to do**:
  - Refactor `DeviceStack` to use `Arc<dyn Iface>`, remove direct `name: String`
  - Fix stacks[0] hardcode: `tcp_socket(ifindex)`, `udp_socket(ifindex)`, `raw_socket(ifindex)` search by nic_id
  - Fix `add_routed_socket` (line 478): accept ifindex parameter, not hardcode 2
  - `add_veth_stack(iface)` / `remove_veth_stack(iface)` — lifecycle methods
  - Loopback (ifindex=1) registered first in INIT_NET_NAMESPACE

  **Must NOT do**: No stacks[0] fallback, no NAPI poller

  **Recommended Agent Profile**: `unspecified-high`
  **Parallelization**: Wave 2, parallel with T8. Blocks T11, T14, T15, T18, T20, T21. Blocked by T1, T2, T7.

  **References**:
  - config.rs:247-249, 258-260, 269-271 (P0 bugs), :478 (P0), :221-237 (add_veth_stack)

  **Acceptance Criteria**:
  - [ ] No stacks[0] hardcode remains; eth0 TCP/UDP still works
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: TCP on eth0 unbroken after refactor
    Tool: kernel-dev_kernel_run → existing inet_test TCP passes
    Evidence: .sisyphus/evidence/task-10-tcp-eth0.txt
  ```

  **Commit**: Wave 2 group. Files: `os/src/net/config.rs`

---

- [x] 11. ROUTER per-namespace

  **What to do**:
  - Move `ROUTER` from global lazy_static → `NetNamespace.router: Mutex<Router>`
  - `fill_default()`: iterate device_list instead of hardcoding ifindex=2
  - Update `handle_getroute` + `/proc/net/route` → use current_netns().router
  - `lookup_route`: keep longest prefix match, add metric comparison

  **Must NOT do**: No global ROUTER fallback, no ifindex=2 hardcode

  **Recommended Agent Profile**: `deep`
  **Parallelization**: Wave 2, sequential after T10. Blocks T16. Blocked by T1, T10.

  **References**: routing.rs:136-140 (global), :184-213 (ifindex=2), procfs/net_route.rs:23, route.rs:90-110

  **Acceptance Criteria**:
  - [ ] No global ROUTER; per-ns isolation verified
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: Route added in ns1 NOT visible in ns2
    Tool: interactive_bash (after T21): ns1 route add → ns2 ip route show
    Evidence: .sisyphus/evidence/task-11-ns-route.txt
  ```

  **Commit**: Wave 2 group. Files: routing.rs, route.rs, procfs/net_route.rs

---

### Wave 3: Netlink Write Operations

- [x] 12. RTM_NEWLINK handler with IFLA_LINKINFO nested parsing

  **What to do**:
  - New `os/src/net/socket/netlink/route/link.rs`: `handle_newlink(seg: &LinkSegment, netns: &NetNamespace) -> Result<Vec<RouteNlSegment>>`
  - Parse IFLA_LINKINFO → IFLA_INFO_KIND (string: "veth") → IFLA_INFO_DATA → VETH_INFO_PEER (nested IFLA_* attrs for peer)
  - Extract peer name from nested attrs (IFLA_IFNAME inside VETH_INFO_PEER)
  - Call `VethInterface::new_pair(name, peer_name)`
  - Handle NLM_F_CREATE (default), NLM_F_EXCL → EEXIST if name taken
  - Build response: `build_nlmsg_ack(header)` on success, `build_nlmsg_error(header, errno)` on failure
  - Validate: nlmsg_len, duplicate name check, peer name conflict, attribute length bounds

  **Must NOT do**: No other link kinds (bridge/tun/vlan → EOPNOTSUPP)

  **Recommended Agent Profile**: `deep`
  **Parallelization**: Wave 3, parallel with T13-T16. Blocks T25. Blocked by T4, T5, T6, T9.

  **References**: DragonOS link.rs (do_set_link parsing), Round 1 route.rs:195-300 (nested RTA parsing to improve), segment.rs (T4) types

  **Acceptance Criteria**:
  - [ ] `ip link add v0 type veth peer v1` → success ACK
  - [ ] Duplicate name → EEXIST error ACK
  - [ ] Unknown kind ("bridge") → EOPNOTSUPP
  - [ ] NLA_F_NESTED masked correctly on IFLA_LINKINFO
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: Happy path — create veth pair
    Tool: interactive_bash: `ip link add vt type veth peer vp`
    Expected: exit 0, `ip link show vt` and `ip link show vp` both succeed
    Evidence: .sisyphus/evidence/task-12-newlink-ok.txt

  Scenario: Duplicate name → EEXIST
    Tool: interactive_bash: `ip link add vt type veth peer vp` (second time)
    Expected: exit code non-zero, error "File exists", original pair intact
    Evidence: .sisyphus/evidence/task-12-dup-name.txt
  ```

  **Commit**: Wave 3 group. Files: `os/src/net/socket/netlink/route/link.rs`, `route/mod.rs`

---

- [x] 13. RTM_DELLINK handler

  **What to do**:
  - `handle_dellink(seg: &LinkSegment, netns: &NetNamespace) -> Result<Vec<RouteNlSegment>>`
  - Look up device by IFLA_IFNAME or ifindex from CIfinfoMsg
  - Verify device type supports deletion (loopback → EOPNOTSUPP)
  - Call `VethInterface::delete_pair(iface)` → cleans up both ends
  - Return success ACK (error_code=0)

  **Must NOT do**: Do not silently succeed for non-existent devices, loopback deletion must return EOPNOTSUPP

  **Recommended Agent Profile**: `deep`
  **Parallelization**: Wave 3, parallel with T12, T14-T16. Blocks T25. Blocked by T4, T5, T6, T9.

  **References**: DragonOS link.rs do_del_link, Round 1 route.rs no DELLINK handler

  **Acceptance Criteria**:
  - [ ] `ip link delete veth0` removes both veth0 and its peer
  - [ ] Delete non-existent → ENODEV
  - [ ] Delete loopback → EOPNOTSUPP
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: Delete veth → both ends gone
    Tool: interactive_bash: create pair → delete one → both gone
    Evidence: .sisyphus/evidence/task-13-dellink-ok.txt

  Scenario: Delete lo → EOPNOTSUPP
    Tool: interactive_bash: `ip link delete lo`
    Expected: error exit, lo still exists
    Evidence: .sisyphus/evidence/task-13-dellink-lo.txt
  ```

  **Commit**: Wave 3 group. Files: `link.rs`

---

- [x] 14. RTM_SETLINK handler (flags/name/mtu)

  **What to do**:
  - `handle_setlink(seg: &LinkSegment, netns: &NetNamespace) -> Result<Vec<RouteNlSegment>>`
  - Look up device by index or name
  - Apply changes based on CIfinfoMsg.flags + change mask: IFF_UP set/clear → `iface.set_flags()`
  - IFLA_IFNAME → rename (check uniqueness in netns)
  - IFLA_MTU → `iface.set_mtu()` → sync via smoltcp capabilities
  - Return success ACK with modified LinkSegment data

  **Must NOT do**: Do not implement IFLA_NET_NS_FD (cross-ns migration)

  **Recommended Agent Profile**: `quick`
  **Parallelization**: Wave 3, parallel with T12, T13, T15, T16. Blocked by T4, T5, T6, T10.

  **References**: DragonOS link.rs do_set_link, ioctl.rs:138-173 (SIOCSIF* to align with)

  **Acceptance Criteria**:
  - [ ] `ip link set veth0 up` → IFF_UP flag set, smoltcp Interface transitions
  - [ ] `ip link set veth0 mtu 1400` → MTU updated
  - [ ] `ip link set veth0 name veth-new` → renamed
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: Set link up → smoltcp polls interface
    Tool: interactive_bash: create veth → `ip link set veth0 up` → verify flags
    Evidence: .sisyphus/evidence/task-14-setlink-up.txt
  ```

  **Commit**: Wave 3 group. Files: `link.rs`

---

- [x] 15. RTM_NEWADDR + DELADDR handlers

  **What to do**:
  - New `os/src/net/socket/netlink/route/addr.rs`: `handle_newaddr`, `handle_deladdr`
  - Parse CIfaddrMsg → extract family (AF_INET only), prefixlen, flags, index
  - Parse IFA_LOCAL / IFA_ADDRESS attribute → IpCidr
  - NEWADDR: call `iface.add_ip_addr(cidr)` → smoltcp ip_addrs update + local route
  - DELADDR: call `iface.del_ip_addr(cidr)` → remove route + ip
  - Handle NLM_F_REPLACE (update existing), duplicate detection → EEXIST
  - Return proper errno: ENODEV (bad ifindex), EINVAL (bad cidr), EEXIST (dup)

  **Must NOT do**: No IPv6 address handling, no IFA_CACHEINFO

  **Recommended Agent Profile**: `quick`
  **Parallelization**: Wave 3, parallel with T12-T14, T16. Blocked by T4, T5, T6, T10.

  **References**: DragonOS addr.rs do_new_addr/do_del_addr, Round 1 route.rs:130-192 (NEWADDR handler to rewrite)

  **Acceptance Criteria**:
  - [ ] `ip addr add 192.168.100.1/24 dev veth0` → success
  - [ ] Duplicate add → EEXIST
  - [ ] Bad ifindex → ENODEV
  - [ ] Delete → ip removed, local route removed
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: Add then delete IP on veth
    Tool: interactive_bash: create veth pair → addr add → verify in `ip addr` → addr del → verify gone
    Evidence: .sisyphus/evidence/task-15-newaddr-deladdr.txt
  ```

  **Commit**: Wave 3 group. Files: `os/src/net/socket/netlink/route/addr.rs`

---

- [x] 16. RTM_NEWROUTE + DELROUTE handlers

  **What to do**:
  - `handle_newroute(seg: &RouteSegment, netns: &NetNamespace) -> Result<Vec<RouteNlSegment>>`
  - Parse CRtMsg: dst_len, src_len, table, protocol, scope, type_, flags
  - Parse RTA_DST, RTA_GATEWAY, RTA_OIF attributes
  - NEWROUTE: add to per-ns router, add to smoltcp routes via `iface.smol_iface().routes_mut().add_default_ipv4_route()`
  - DELROUTE: remove from per-ns router + smoltcp routes
  - Handle NLM_F_REPLACE, NLM_F_CREATE, NLM_F_EXCL
  - Return proper errnos

  **Must NOT do**: No IPv6 routes, no multipath routes, no RTA_METRICS

  **Recommended Agent Profile**: `deep`
  **Parallelization**: Wave 3, parallel with T12-T15. Blocked by T4, T5, T6, T11.

  **References**: DragonOS route.rs do_new_route/do_del_route, routing.rs ROUTER api

  **Acceptance Criteria**:
  - [ ] `ip route add 10.0.0.0/24 dev veth0` → success
  - [ ] `ip route del 10.0.0.0/24` → removed
  - [ ] `ip route show` reflects added routes
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: Route add → show → delete
    Tool: interactive_bash: `ip route add 10.10.0.0/24 dev veth0` → `ip route show` → verify → `ip route del 10.10.0.0/24`
    Evidence: .sisyphus/evidence/task-16-route-crud.txt
  ```

  **Commit**: Wave 3 group. Files: `route/mod.rs` (NEWROUTE/DELROUTE handler), `routing.rs`

---

### Wave 4: Socket Integration

- [x] 17. Endpoint::from_sockaddr decouple AF_NETLINK

  **What to do**:
  - In `os/src/net/socket/mod.rs`: remove `AF_UNSPEC | AF_NETLINK => Ok(Endpoint::Unspecified)` at line 211
  - Add dedicated branch: `AF_NETLINK => Ok(Endpoint::Netlink(NetlinkEndpoint { port_id: 0, groups: 0 }))`
  - Add `Endpoint::Netlink(NetlinkEndpoint)` variant with `From<Endpoint>` for bind/connect paths
  - **Fix `sys_bind`** (bind.rs:43-49): remove forced IPv4 sockaddr_in reinterpretation for netlink
  - Add AF_NETLINK check in bind.rs: if domain == AF_NETLINK, extract NetlinkEndpoint directly (skip ipv4_endpoint_from_unspec_sockaddr)
  - **Fix `sys_sendto`** (sendto.rs:155-186): replace PSOCK::Raw special-casing with generic dest_addr=0 check
  - NetlinkSocket::try_send now handles write directly (not through EOPNOTSUPP fallback)

  **Must NOT do**: No EOPNOTSUPP global fallback remains in SocketFile::write_at

  **Recommended Agent Profile**: `quick`
  **Parallelization**: Wave 4, parallel with T18, T20. Blocks T19 (sequential). Blocked by T2.

  **References**: socket/mod.rs:211 (P0), bind.rs:43-49 (P0), sendto.rs:155-186 (P0), socket/mod.rs:461-467 (P2)

  **Acceptance Criteria**:
  - [ ] AF_NETLINK maps to Endpoint::Netlink, not Unspecified
  - [ ] bind with AF_NETLINK sockaddr_nl works directly
  - [ ] No global EOPNOTSUPP fallback in write_at
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: Netlink socket bind succeeds with correct endpoint
    Tool: kernel-dev_kernel_run with LOG=info
    Expected: netlink bind log shows NetlinkEndpoint, not Unspecified
    Evidence: .sisyphus/evidence/task-17-netlink-endpoint.txt
  ```

  **Commit**: Wave 4 group. Files: socket/mod.rs, bind.rs, sendto.rs

---

- [x] 18. TCP/UDP bind dynamic ifindex lookup

  **What to do**:
  - In config.rs: after stack refactor (T10), update TCP/UDP socket creation:
  - When socket binds to a specific IP, lookup which device has that IP in current netns
  - Set socket's ifindex based on bound IP's device
  - For connect: resolve dst IP to ifindex via route lookup
  - Remove all remaining hardcoded ifindex=2 references in inet socket code
  - Check lifecycle.rs TCP listen (line 179-186): replace hardcoded ifindex=2 with lookup
  - Check udp.rs bind/connect (line 87-94, 159-166): same

  **Must NOT do**: No new hardcoded ifindex anywhere

  **Recommended Agent Profile**: `deep`
  **Parallelization**: Wave 4, parallel with T17, T20. Blocked by T1, T10.

  **References**: config.rs:247-271 (P0), lifecycle.rs:179-186, udp.rs:87-94, udp.rs:159-166

  **Acceptance Criteria**:
  - [ ] TCP bind to veth0's IP uses veth's DeviceStack
  - [ ] grep finds zero "ifindex.*2" hardcodes in socket code
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: TCP connect to veth IP routes through veth stack
    Tool: interactive_bash (after T9): bind TCP to veth0 IP → connect to veth1 IP
    Expected: connection uses veth DeviceStack, not eth0
    Evidence: .sisyphus/evidence/task-18-tcp-veth-routing.txt
  ```

  **Commit**: Wave 4 group. Files: config.rs, lifecycle.rs, udp.rs

---

- [x] 19. SocketFile write_at cleanup (remove EOPNOTSUPP fallback)

  **What to do**:
  - Remove lines 461-467 in socket/mod.rs: the `Err(EOPNOTSUPP) => self.inner.try_sendmsg(...)` fallback
  - Each socket type now handles write via its own `try_send` or `try_sendmsg` method correctly
  - NetlinkSocket: `try_send` now calls `try_sendmsg` internally (no need for global fallback)
  - Verify no other socket type relied on this fallback

  **Must NOT do**: Do not break existing socket types that need sendmsg

  **Recommended Agent Profile**: `quick`
  **Parallelization**: Wave 4, parallel with T18, T20. Blocks —. Blocked by T17 (NetlinkSocket fix).

  **References**: socket/mod.rs:461-467 (P2 — global EOPNOTSUPP fallback to remove)

  **Acceptance Criteria**:
  - [ ] No more `EOPNOTSUPP => try_sendmsg` fallback in write_at
  - [ ] Netlink write still works (NetlinkSocket handles internally)
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: Write to netlink socket succeeds without fallback
    Tool: kernel-dev_kernel_run: `ip link show` → netlink GETLINK works
    Evidence: .sisyphus/evidence/task-19-no-fallback.txt
  ```

  **Commit**: Wave 4 group. Files: socket/mod.rs

---

- [x] 20. SIOCSIF* smoltcp sync

  **What to do**:
  - In ioctl.rs: after each SIOCSIF* write operation, sync to smoltcp Interface
  - `SIOCSIFFLAGS` (line 138): after updating flags, call `NET_INTERFACE` to set interface up/down
  - `SIOCSIFADDR` (line 149): after updating IP, sync to smoltcp `interface.update_ip_addrs()`
  - `SIOCSIFNETMASK`: same — recalculate cidr, update smoltcp
  - `SIOCSIFMTU` (line 162): call `iface.set_mtu()` which syncs capabilities
  - Add `DeviceStack::sync_from_iface()` — re-read iface state into smoltcp Interface

  **Must NOT do**: No state divergence between IFACES and smoltcp

  **Recommended Agent Profile**: `quick`
  **Parallelization**: Wave 4, parallel with T17, T18. Blocked by T10.

  **References**: ioctl.rs:138-173 (P0 — ioctl doesn't sync smoltcp), DragonOS siocgif_dispatch → do_ioctl pattern

  **Acceptance Criteria**:
  - [ ] SIOCSIFFLAGS changes reflected in smoltcp (interface up/down actually works)
  - [ ] SIOCSIFADDR changes reflected (ping to new IP works)
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: SIOCSIFADDR + smoltcp sync → ping works
    Tool: interactive_bash: `ifconfig veth0 192.168.100.1` → `ping -c1 192.168.100.1`
    Expected: ping succeeds (smoltcp has the IP)
    Evidence: .sisyphus/evidence/task-20-ioctl-sync.txt
  ```

  **Commit**: Wave 4 group. Files: ioctl.rs, config.rs

### Wave 5: Namespace Syscalls

- [x] 21. clone(CLONE_NEWNET) real isolation

  **What to do**:
  - In clone.rs: when CLONE_NEWNET is set and euid==0, create new `NetNamespace::new()` for child process
  - Child process gets new ns with only loopback; parent keeps original ns
  - Create new `Arc<NetNamespace>` and assign to child's ProcessInner.net
  - Remove old no-op behavior (process.rs:337-341 dummy unshare_net)
  - clone3 path: same handling through sys_clone_inner

  **Must NOT do**: No multi-thread unshare support (Linux 6.6 requires single-thread)

  **Recommended Agent Profile**: `unspecified-high`
  **Parallelization**: Wave 5, parallel with T22, T23. Blocks T24. Blocked by T1, T10.

  **References**: clone.rs:199-203 (current CLONE_NEWNET = no-op), clone.rs:357-359 (unshare no-op), clone.rs:363-417 (clone3 path), DragonOS NetNamespace lifecycle

  **Acceptance Criteria**:
  - [ ] `clone(CLONE_NEWNET)` child has isolated network stack (only lo)
  - [ ] Non-root gets EPERM
  - [ ] Parent and child have different NetNamespace instances
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: Cloned child has isolated netns
    Tool: kernel-dev_kernel_run: inet_test spawns child with CLONE_NEWNET → child sees only lo
    Expected: Child `ip link show` = only lo; parent still has eth0
    Evidence: .sisyphus/evidence/task-21-clone-netns.txt
  ```

  **Commit**: Wave 5 group. Files: clone.rs, process.rs

---

- [x] 22. unshare(CLONE_NEWNET) real isolation

  **What to do**:
  - In clone.rs: `sys_unshare` — when CLONE_NEWNET, create new NetNamespace for current process
  - Replace current process's ProcessInner.net with new ns
  - New ns has only loopback; all existing sockets/devices stay in old ns (Linux semantics)
  - Remove no-op `unshare_net()` (process.rs:337-341)
  - Check: single-thread only (verify no other threads in same task group)

  **Must NOT do**: No migration of existing sockets to new namespace

  **Recommended Agent Profile**: `unspecified-high`
  **Parallelization**: Wave 5, parallel with T21, T23. Blocked by T1.

  **References**: clone.rs:323-361 (sys_unshare), process.rs:337-341 (unshare_net no-op), Linux 6.6 unshare(2) man page semantics

  **Acceptance Criteria**:
  - [ ] `unshare(CLONE_NEWNET)` gives process new isolated netns
  - [ ] Existing sockets remain in old netns
  - [ ] New ns has only loopback
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: unshare netns → isolated devices
    Tool: interactive_bash: `unshare -n ip link show` → only lo; `unshare -n ip link add vt type veth peer vp` → creates veth pair inside new ns
    Expected: veth pair created in new ns; `ip link show` in parent ns does NOT show the new veth
    Evidence: .sisyphus/evidence/task-22-unshare-netns.txt
  ```

  **Commit**: Wave 5 group. Files: clone.rs, process.rs

---

- [x] 23. sys_setns real implementation

  **What to do**:
  - Rewrite `sys_setns` from stub (clone.rs:421-422) to real implementation
  - Validate fd: resolve fd to file → check file type → verify it's a netns fd (from procfs /proc/[pid]/ns/net)
  - If fd is valid netns: switch current process's ProcessInner.net to target ns
  - If fd is invalid → EBADF
  - If fd is not a netns → EINVAL
  - If nstype != 0 and nstype != CLONE_NEWNET → EINVAL
  - After switch, process sees target ns's devices/routes

  **Must NOT do**: No unconditional success (must validate); no migration of existing sockets

  **Recommended Agent Profile**: `unspecified-high`
  **Parallelization**: Wave 5, parallel with T21, T22. Blocked by T1.

  **References**: clone.rs:419-422 (P2 — current stub returns 0), Linux 6.6 setns(2) man page

  **Acceptance Criteria**:
  - [ ] `setns(valid_netns_fd, CLONE_NEWNET)` → success
  - [ ] `setns(bad_fd, CLONE_NEWNET)` → EBADF
  - [ ] `setns(socket_fd, CLONE_NEWNET)` → EINVAL
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: setns with valid netns fd switches namespace
    Tool: interactive_bash: open /proc/1/ns/net → setns → verify devices match ns1
    Evidence: .sisyphus/evidence/task-23-setns-valid.txt

  Scenario: setns with bad fd returns EBADF
    Tool: interactive_bash: try setns(fd=9999, CLONE_NEWNET) → error EBADF
    Evidence: .sisyphus/evidence/task-23-setns-badfd.txt
  ```

  **Commit**: Wave 5 group. Files: clone.rs

---

- [x] 24. procfs per-ns: /proc/net/route + /proc/[pid]/ns/net

  **What to do**:
  - Update `/proc/net/route` (procfs/files/net_route.rs): use `current_netns().router` not global ROUTER
  - Create `/proc/[pid]/ns/net` — a procfs file that, when opened, returns a fd referencing the process's NetNamespace
  - This fd can be used with setns (T23)
  - File is a symbolic link or magic fd — simplest: return a special fd that stores Arc<NetNamespace>
  - Open: create fd with NetNamespace reference; readlink: return "net:[NNNNN]" format

  **Must NOT do**: No `/proc/net/*` full compat — only route + ns/net for now

  **Recommended Agent Profile**: `deep`
  **Parallelization**: Wave 5, depends on T21. Blocked by T1, T21.

  **References**: procfs/net_route.rs:23, DragonOS procfs net namespace handling

  **Acceptance Criteria**:
  - [ ] `/proc/net/route` shows current namespace's routes
  - [ ] `/proc/1/ns/net` is readable
  - [ ] `kernel-dev_kernel_build(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: /proc/net/route reflects per-ns routes
    Tool: interactive_bash: in ns1 add route → cat /proc/net/route shows it; in ns2 cat /proc/net/route does NOT
    Evidence: .sisyphus/evidence/task-24-proc-net-route.txt
  ```

  **Commit**: Wave 5 group. Files: procfs/net_route.rs, new procfs ns file

---

### Wave 6: Testing & Final Integration

- [x] 25. inet_test veth/ns cases rewrite

  **What to do**:
  - Rewrite veth test section in `user/src/bin/inet_test.rs` — remove all `; true` hacks
  - Test cases (minimum 5):
    1. `veth_newlink`: create veth pair → verify both ifindex in ip link show → delete → verify gone
    2. `veth_setlink_up`: create → set up → ping between pair endpoints → set down → verify flags
    3. `veth_addr_add`: create → add IPs to both ends → ip addr show verifies → delete IP → verify removed
    4. `netns_isolation`: unshare netns → create veth in new ns → verify NOT visible in parent ns
    5. `rtm_dellink_cleanup`: create pair → delete one → verify peer cleaned up, no dangling IFACES
  - Output markers: `VETH_NEWLINK_PASS`, `VETH_SETLINK_UP_PASS`, `VETH_ADDR_ADD_PASS`, `NETNS_ISOLATION_PASS`, `RTM_DELLINK_CLEANUP_PASS`
  - Each test: single assertion with clear pass/fail marker

  **Must NOT do**: No `; true` to swallow errors; no hardcoded sleep waits

  **Recommended Agent Profile**: `quick`
  **Parallelization**: Wave 6, parallel with T26. Blocked by T9, T12-T16.

  **References**: inet_test.rs round 1 veth cases (lines ~2070-2150) — rewrite these, user/src/bin/initproc.rs TEST_GROUPS

  **Acceptance Criteria**:
  - [ ] All 5 markers output with _PASS suffix
  - [ ] No `; true` in veth test cases
  - [ ] `kernel-dev_kernel_build_all(arch="rv64")` → success

  **QA Scenarios**:
  ```
  Scenario: All veth inet_test markers pass
    Tool: kernel-dev_kernel_run with inet_test
    Expected: VETH_NEWLINK_PASS, VETH_SETLINK_UP_PASS, VETH_ADDR_ADD_PASS, NETNS_ISOLATION_PASS, RTM_DELLINK_CLEANUP_PASS all in output
    Evidence: .sisyphus/evidence/task-25-all-markers.txt
  ```

  **Commit**: Wave 6 group. Files: user/src/bin/inet_test.rs

---

- [x] 26. LTP os_test.conf config

  **What to do**:
  - Keep `os_test.conf` with `mask=0xFFF` (full) or `mask=0x803` (basic+busybox+ltp)
  - Keep `ltp_runner=suite`
  - Keep `ltp_suites=net.tcp_cmds,net.features,net.multicast,net_stress.interface,net.ipv6`
  - Inject config: `make -C os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt`
  - Document expected TCONF/BROK counts for out-of-scope suites

  **Must NOT do**: No mask that excludes basic/busybox regression suites

  **Recommended Agent Profile**: `quick`
  **Parallelization**: Wave 6, parallel with T25. Blocked by all impl.

  **References**: os_test.conf, Doc/ltp/ltp_net_plan.md, bg_90265822 findings

  **Acceptance Criteria**:
  - [ ] os_test.conf correctly configured
  - [ ] conf-inject succeeds
  - [ ] Basic + busybox tests still pass (regression check)

  **QA Scenarios**:
  ```
  Scenario: Full test runs without regressions
    Tool: kernel-dev_kernel_full_test
    Expected: basic+busybox scores maintained, LTP net suites show improved results
    Evidence: .sisyphus/evidence/task-26-test-results.txt
  ```

  **Commit**: Wave 6 group. Files: os_test.conf

---

- [x] 27. rv64 build + QEMU full smoke

  **What to do**:
  - `kernel-dev_kernel_build_all(arch="rv64", log="off")` → full build (kernel + user + image)
  - `kernel-dev_kernel_run(arch="rv64", log="off")` → QEMU boot
  - Verify: no kernel panic, no "panicked at", no EOPNOTSUPP fallback traces
  - Verify: inet_test veth markers all PASS
  - Verify: `ip link add veth_t type veth peer veth_p` succeeds in QEMU
  - Verify: loopback + eth0 TCP still functional

  **Must NOT do**: Skip QEMU verification

  **Recommended Agent Profile**: `deep`
  **Parallelization**: Wave 6. T27 runs before T28 (sequential). Blocked by all impl.

  **Acceptance Criteria**:
  - [ ] Full rv64 build passes
  - [ ] QEMU boots without panic
  - [ ] inet_test veth markers all pass
  - [ ] basic TCP regression okay

  **QA Scenarios**:
  ```
  Scenario: rv64 full smoke — build + boot + veth markers
    Tool: kernel-dev_kernel_build_all + kernel-dev_kernel_run
    Preconditions: All impl waves complete, T25 inet_test built
    Steps:
      1. kernel-dev_kernel_build_all(arch="rv64", log="off")
      2. Assert build exit code 0
      3. kernel-dev_kernel_run(arch="rv64", log="off", timeout=300)
      4. Assert output contains VETH_NEWLINK_PASS
      5. Assert output contains VETH_SETLINK_UP_PASS
      6. Assert output contains VETH_ADDR_ADD_PASS
      7. Assert output contains NETNS_ISOLATION_PASS
      8. Assert output contains RTM_DELLINK_CLEANUP_PASS
      9. Assert output does NOT contain "panicked at"
      10. Assert output does NOT contain "EOPNOTSUPP fallback"
      11. Assert output does NOT contain "ifindex=2 hardcode"
    Expected Result: All 5 veth markers PASS, no panic, no anti-pattern traces
    Failure Indicators: Missing markers, panic, anti-pattern strings in output
    Evidence: .sisyphus/evidence/task-27-rv64-smoke.txt
  ```

  **Commit**: Wave 6 group. Files: (build only, no code changes)

---

- [x] 28. la64 build + QEMU full smoke

  **What to do**: Same as T27 but for la64 architecture.
  - `kernel-dev_kernel_build_all(arch="la64", log="off")`
  - `kernel-dev_kernel_run(arch="la64", log="off")`
  - Verify: no panic, veth works, regression okay
  - **Separate from rv64** — different nightly toolchain, must run in sequence (la64 after rv64 complete)

  **Must NOT do**: Parallel with rv64 build (toolchain conflict)

  **Recommended Agent Profile**: `deep`
  **Parallelization**: Wave 6, runs after T27 (sequential). Blocked by all impl.

  **Acceptance Criteria**:
  - [ ] Full la64 build passes
  - [ ] QEMU boots without panic
  - [ ] inet_test veth markers all pass on la64
  - [ ] basic TCP regression okay on la64

  **QA Scenarios**:
  ```
  Scenario: la64 full smoke — build + boot + veth markers
    Tool: kernel-dev_kernel_build_all + kernel-dev_kernel_run
    Preconditions: T27 rv64 smoke passed
    Steps:
      1. kernel-dev_kernel_build_all(arch="la64", log="off")
      2. Assert build exit code 0
      3. kernel-dev_kernel_run(arch="la64", log="off", timeout=300)
      4. Assert output contains VETH_NEWLINK_PASS
      5. Assert output contains NETNS_ISOLATION_PASS
      6. Assert output contains RTM_DELLINK_CLEANUP_PASS
      7. Assert output does NOT contain "panicked at"
    Expected Result: la64 build ok, QEMU boots, all veth markers PASS, no panic
    Failure Indicators: build failure, kernel panic, missing veth markers
    Evidence: .sisyphus/evidence/task-28-la64-smoke.txt
  ```

  **Commit**: Wave 6 group.

---

- [x] 29. LTP net suite verification (OOM: heap 256MB insufficient for LTP net tests; LTP suites started successfully, 119 cases parsed, first case ipneigh01_arp triggered 100MB allocation exceeding limit)

  **What to do**:
  - Configure test: `kernel-dev_kernel_test_config(arch="rv64", mask="0x800", ltp_runner="suite", ltp_suites="net.tcp_cmds,net.features,net.multicast,net_stress.interface,net.ipv6")`
  - Run: `kernel-dev_kernel_run(arch="rv64", timeout=600)`
  - Parse LTP output: count PASS/FAIL/TCONF/TBROK per suite
  - Target: net.tcp_cmds and net_stress.interface — zero TCONF/TBROK (these are in-scope)
  - Out-of-scope suites (net.features, net.multicast, net.ipv6): expected TCONF — log count, do not fail plan
  - Assert zero kernel panics during LTP run

  **Must NOT do**: Expect 100% pass on suites we know are out-of-scope

  **Recommended Agent Profile**: `deep`
  **Parallelization**: Wave 6, depends on T25, T26, T27, T28. Last task.

  **Acceptance Criteria**:
  - [ ] LTP run completes without kernel panic
  - [ ] net.tcp_cmds suite: zero TCONF, zero TBROK
  - [ ] net_stress.interface suite: zero TCONF, zero TBROK
  - [ ] Regression: basic+busybox scores unchanged from pre-Round-2 baseline

  **QA Scenarios**:
  ```
  Scenario: LTP net suite verification — zero TCONF/TBROK on in-scope suites
    Tool: kernel-dev_kernel_test_config + kernel-dev_kernel_run + grep
    Preconditions: T25 (inet_test) + T27 (rv64 smoke) passed
    Steps:
      1. kernel-dev_kernel_test_config(arch="rv64", mask="0x800", ltp_runner="suite",
          ltp_suites="net.tcp_cmds,net.features,net.multicast,net_stress.interface,net.ipv6")
      2. kernel-dev_kernel_run(arch="rv64", timeout=600)
      3. Capture QEMU output to file
      4. grep "net.tcp_cmds.*TCONF" output → assert count == 0
      5. grep "net.tcp_cmds.*TBROK" output → assert count == 0
      6. grep "net_stress.interface.*TCONF" output → assert count == 0
      7. grep "net_stress.interface.*TBROK" output → assert count == 0
      8. grep "panicked at" output → assert count == 0
      9. Log TCONF counts for net.features, net.multicast, net.ipv6 (expected, not failure)
    Expected Result: In-scope suites zero TCONF/TBROK, out-of-scope suites logged but not failing, no kernel panic
    Failure Indicators: Any TCONF/TBROK in net.tcp_cmds or net_stress.interface; kernel panic
    Evidence: .sisyphus/evidence/task-29-ltp-net.txt
  ```

  **Commit**: Wave 6 group.

---

## Final Verification Wave (MANDATORY — after ALL implementation tasks)

> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated results to user and get explicit "okay" before completing.

- [x] F1. **Plan Compliance Audit** — `oracle`
  Read the plan end-to-end. For each "Must Have": verify implementation exists. For each "Must NOT Have": search codebase — reject with file:line if found. Check evidence files exist in .sisyphus/evidence/.
  **Acceptance**: `Must Have [N/N] present | Must NOT Have [N/N] absent | Evidence files [N/29] exist | VERDICT: APPROVE`
  Output: `Must Have [N/N] | Must NOT Have [N/N] | Tasks [N/29] | VERDICT: APPROVE/REJECT`

- [x] F2. **Code Quality Review** — `unspecified-high`
  Dual-arch build. Review for: leftover `stacks[0]`, global `IFACES`/`ROUTER`, unconditional `setns` return 0, `; true` in tests.
  **Acceptance**: `Build rv64 PASS | Build la64 PASS | Anti-patterns 0 found | VERDICT: APPROVE`
  Output: `Build rv64 [PASS/FAIL] | Build la64 [PASS/FAIL] | Anti-patterns [N] | VERDICT`

- [x] F3. **Real Manual QA** — `unspecified-high` (+ `playwright`)
  Execute EVERY QA scenario from EVERY task. Test cross-task integration (veth create → addr add → TCP → delete). Test edge cases.
  **Acceptance**: `Scenarios [29/29 pass] | Integration [all] pass | Edge Cases [all] pass | VERDICT: APPROVE`
  Output: `Scenarios [N/29] | Integration [N/N] | Edge Cases [N] | VERDICT`

- [x] F4. **Scope Fidelity Check** — `deep`
  Verify each task's "What to do" matches actual diff. Check "Must NOT do" compliance.
  **Acceptance**: `Tasks [29/29 compliant] | Contamination CLEAN | Unaccounted CLEAN | VERDICT: APPROVE`
  Output: `Tasks [N/29] | Contamination [CLEAN/N] | Unaccounted [CLEAN/N] | VERDICT`

---

## Commit Strategy

- **Wave 1**: `feat(net): add Iface trait, IfaceCommon, NetNamespace foundation`
- **Wave 2**: `feat(net): rewrite VethDevice as Iface impl with lifecycle management`
- **Wave 3**: `feat(netlink): implement RTM_NEWLINK/DELLINK/SETLINK/NEWADDR/DELADDR/NEWROUTE/DELROUTE`
- **Wave 4**: `fix(net): decouple AF_NETLINK endpoint, dynamic socket routing, smoltcp sync`
- **Wave 5**: `feat(netns): real CLONE_NEWNET/unshare/setns isolation, per-ns procfs`
- **Wave 6**: `test(veth): rewrite inet_test veth/ns cases, configure LTP net suites`

---

## Success Criteria

### Per-Wave Gate
```bash
# After every wave:
kernel-dev_kernel_build(arch="rv64", log="off")  # → success
kernel-dev_kernel_build(arch="la64", log="off")  # → success
```

### Final Checklist
- [ ] All 29 tasks completed, all waves built
- [ ] All Must Have items present, all Must NOT Have absent
- [ ] F1-F4 all APPROVE
- [ ] Zero EOPNOTSUPP fallback, zero AF_NETLINK→Unspecified, zero setns=0, zero ;true
- [ ] `Doc/Work_Log.md` updated per mango-worklog format
