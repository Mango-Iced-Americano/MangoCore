# veth + Netlink Write + LTP网络测试

## TL;DR

> **Quick Summary**: 实现 veth 虚拟网卡对 + netlink RTM_NEWLINK 写操作 + /lib/modules/ 文件，使 LTP 网络 suite（119 个 shell 脚本型测试）从全部 ENV_FAIL → 可实际运行。
> 
> **Deliverables**:
> - `os/src/net/veth.rs` — veth 核心驱动（smoltcp phy::Device）
> - `os/src/net/socket/netlink/route.rs` — RTM_NEWLINK/RTM_NEWADDR 写 handler
> - `os/src/net/socket/netlink/netlink.rs` — 新增 IFLA_INFO_KIND 等常量
> - `os/src/net/net_core.rs` — DeviceEntry 动态化 + 持久 Router
> - `os/src/net/config.rs` — DeviceStack.name 动态化
> - `os/src/task/` — NetNamespace stub + CLONE_NEWNET + setns stub
> - `user/src/bin/initproc.rs` — `/lib/modules/` 目录创建
> - `user/src/bin/inet_test.rs` — veth 专项测试用例
> 
> **Estimated Effort**: Medium
> **Parallel Execution**: YES — 3 waves
> **Critical Path**: T5 (VethDevice) → T8 (VethPair) → T9 (RTM_NEWLINK) → T14 (LTP test)

---

## Context

### Original Request
用户发现 LTP 网络测试（`net.tcp_cmds`、`net.features`、`net.multicast`、`net_stress.interface`、`net.ipv6`）全部显示"库缺失"，日志中 119 个用例全部 TCONF/TBROK。根因是 LTP 测试框架需要 veth 内核模块、`/lib/modules/` 文件、以及 `ip link add ... type veth` 的 netlink RTM_NEWLINK 支持。

### Interview Summary
**Key Discussions**:
- 方案选择：放弃 shell-script skip（不可行，测试不走 syscall 路径）、实现 netlink + veth（可行，DragonOS 有完整参考）
- 参考 DragonOS `kernel/src/driver/net/veth.rs`（~450 行），核心机制为 smoltcp `phy::Device` 通过内存队列互连
- 测试加入现有 `user/src/bin/inet_test.rs`
- 单网络命名空间 stub（不实现多空间隔离）
- 范围：不包含 bridge、tun/tap、netfilter/iptables、IPv6 协议栈、SCTP/DCCP

**Research Findings**:
- DragonOS veth: Veth struct with rx_queue + Weak<peer>, VethDriver implements phy::Device, VethInterface::new_pair() creates connected pair. ~450 lines including KObject/Device boilerplate.
- 我们的 `config.rs`: `stacks: Vec<DeviceStack>` 已支持动态增长；`add_routed_socket_on(ifindex)` 已支持任意栈上的 socket；`poll_once()` 已遍历所有栈。架构已为 veth 做好准备。
- 我们的 netlink: `route.rs` 非 dump 路径返回 ENOPROTOOPT — 这是写操作的分发注入点。
- `ltp_net_plan.md` 已将网络命名空间、VLAN/VXLAN 等列为"长期排除"，veth 属于 J 类 "ENV_FAIL"（环境问题），应先修环境再修内核。

### Metis Review
**Identified Gaps** (addressed):
- smoltcp 动态 Interface 添加：已验证 `stacks: Vec<>` 支持 push，`add_routed_socket_on(ifindex)` 支持任意栈 ✅
- DeviceEntry.name 变更范围：确认 DeviceStack.name 也需同步改，影响 ~5 处 net_core.rs + ~3 处 config.rs + netlink/route.rs ✅
- 已有计划文档：`ltp_net_plan.md` Round-0/1/2 框架已纳入参考 ✅
- 预存内核 bug：5月31日 loopback TCP 路由修复已解决 TCP 关键问题，veth 复用同一 TCP 栈 ✅

---

## Work Objectives

### Core Objective
实现 veth 虚拟网卡对驱动 + netlink RTM_NEWLINK/RTM_NEWADDR 写操作，使 `ip link add name veth0 type veth peer name veth1` 在 QEMU 中成功创建可通信的虚拟网卡对，从而解除 LTP 网络 suite 的 ENV_FAIL 阻塞。

### Concrete Deliverables
- `os/src/net/veth.rs` — veth 核心模块
- `os/src/net/socket/netlink/netlink.rs` — 新增常量
- `os/src/net/socket/netlink/route.rs` — 写操作 handler
- `os/src/net/net_core.rs` — DeviceEntry/IFACES 增强
- `os/src/net/config.rs` — DeviceStack 增强
- `os/src/net/routing.rs` — 持久 Router
- `os/src/task/task.rs` + `os/src/syscall/process/clone.rs` — NetNamespace stub
- `user/src/bin/initproc.rs` — /lib/modules/ 目录
- `user/src/bin/inet_test.rs` — veth 测试

### Definition of Done
- [x] `ip link add name test_veth0 type veth peer name test_veth1` — 用户 QEMU 验证
- [x] `ip addr add 192.168.100.1/24 dev test_veth0` — 用户 QEMU 验证
- [x] veth TCP 通信 — 用户 QEMU 验证
- [x] LTP 网络 suite 不再全部 TCONF — 用户 QEMU 验证
- [x] `make rv64-kernel-build-only` ✅
- [x] `make la64-kernel-build-only` ✅
- [x] Dual-arch compilation passes
- [x] No new `todo!()` or `unwrap()` in user-reachable netlink/veth paths
- [x] `Doc/Work_Log.md` updated per mango-worklog format
- [x] QEMU 测试由用户执行（含 modprobe stub + /lib/modules/ 文件 + veth + netlink）
