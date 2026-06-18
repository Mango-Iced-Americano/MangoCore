# Net Subsystem Architecture Upgrade — 6-Stage Incremental Migration

## TL;DR

> **Quick Summary**: Reference DragonOS design patterns to incrementally add a Linux-compatible network control/observation plane (device list, routing, port manager, /proc/net, SIOCGIF* ioctl, NETLINK_ROUTE) around the existing smoltcp data path, preserving all current inet_test functionality throughout.
>
> **Deliverables**:
> - Single-netns core objects: Device list, Router/RouteTable, PortManager (with TCP/UDP tables)
> - RoutingDevice migration: replace hardcoded IPs with Router::lookup_route
> - /proc/net entries: dev, route, tcp, udp, /proc/sys/net/ipv4/ip_forward
> - SIOCGIF* ioctl: read-only queries for lo and eth0
> - Minimum read-only NETLINK_ROUTE: RTM_GETLINK/GETADDR/GETROUTE dump
> - Advanced socket semantics tests: UDP/TCP edge cases, stress tests — all in single inet_test.rs
>
> **Estimated Effort**: XL (6 stages, ~49 tasks)
> **Parallel Execution**: YES — 6 waves (one per stage), each wave with 5-10 parallel tasks
> **Critical Path**: Stage 1 (core objects) → Stage 2 (route migration) → Stage 3 (procfs) → Stage 4 (ioctl) → Stage 5 (netlink) → Stage 6 (advanced tests)

---

## Context

### Original Request
The user wants to upgrade the oskernel2026-mango kernel's network subsystem from a basic smoltcp-based data plane to a Linux/DragonOS-style architecture. The data plane (UDP/TCP send/recv, DNS, HTTP) already works. The goal is to add the control and observation planes needed by LTP net suite and standard Linux tooling, without regression.

### Interview Summary

**Key Discussions**:
- Six incremental stages, each independently verifiable
- Reference DragonOS design patterns (device_list, PortManager, Router, BoundInner)
- All tests in single inet_test.rs, LTP-style (TPASS/TFAIL/TBROK/TCONF)
- No panics on user-triggerable paths — all unsupported features return errno
- Keep existing smoltcp data path intact; add control plane around it
- No full net namespace, no data plane refactor, no per-test hacks

**Research Findings**:
- **16 hardcoded IP locations** across 7 files (adapter.rs, config.rs, socket/mod.rs, raw/raw.rs, and 3 others)
- **PortManager exists** but is stateless — scans fd_table, no TCP/UDP port tables
- **procfs** has excellent infrastructure (LockedProcInode with add_dir/add_file pattern), with `/proc/sys/net/ipv4/conf/` already started
- **ioctl dispatch**: sys_ioctl → file.inode.ioctl(); SocketFile returns ENOTTY — no SIOCGIF* handling
- **Socket::alloc()**: AF_INET and AF_UNIX only, no AF_NETLINK
- **DragonOS** has dual routing tables (kernel Router + netlink-visible routes), NetNamespace with device_list using RwSem
- **RawSocket** has hardcoded IPs and todo!() methods for bind/listen/connect/accept — these must not panic
- **inet_test** currently NOT in preload_app.S (only on sdcard image)

### Metis Review

**Identified Gaps** (addressed):
- RawSocket hardcoded IPs must be included in Stage 2 migration → added as explicit task
- TCP TIME_WAIT state must be checked by PortManager → added as acceptance criterion
- inet_test embedding strategy needed → no embedding required; tests run from sdcard image
- DragonOS dual routing table pattern → Stage 5 netlink uses separate netlink-visible route table
- RawSocket todo!() methods → Stage 2 only replaces IP constants, does NOT implement these methods
- 16 hardcoded locations (not 3) → Stage 2 task expanded to cover all 7 files

---

## Work Objectives

### Core Objective
Incrementally add a Linux-compatible network control/observation plane (device list, routing, port manager, /proc/net, SIOCGIF* ioctl, NETLINK_ROUTE) around the existing smoltcp data path, preserving all current inet_test and syscall functionality.

### Concrete Deliverables
- `os/src/net/net_core.rs` — single-netns core objects (DeviceEntry, devices list, default_iface, loopback_iface)
- `os/src/net/routing.rs` — RouteEntry, RouteTable, Router with longest prefix match
- `os/src/net/socket/inet/common/port.rs` — enhanced PortManager with TCP/UDP port tables
- `os/src/net/socket/inet/common/bound.rs` — BoundInner with logical iface tracking
- `os/src/fs/procfs/files/net_dev.rs` — /proc/net/dev
- `os/src/fs/procfs/files/net_route.rs` — /proc/net/route
- `os/src/fs/procfs/files/net_tcp.rs` — /proc/net/tcp
- `os/src/fs/procfs/files/net_udp.rs` — /proc/net/udp
- `os/src/net/ioctl.rs` — SIOCGIF* handler for SocketFile
- `os/src/net/socket/netlink/` — AF_NETLINK + NETLINK_ROUTE module
- `user/src/bin/inet_test.rs` — expanded with 6 new test groups

### Definition of Done
- [ ] `make rv64-kernel-build-only` ✅
- [ ] `make la64-kernel-build-only` ✅
- [ ] QEMU boots without panic, net initialized
- [ ] All existing 11 inet_test cases pass
- [ ] All new test groups pass (NET_CORE, NET_ROUTE, PROC_NET, NET_IOCTL, RTNETLINK, UDP_SEMANTICS, TCP_POLL_TIMEOUT, SOCKET_STRESS_SMALL)
- [ ] No user-triggerable path panics/uses todo!/unwrap
- [ ] RoutingDevice no longer contains hardcoded IP addresses
- [ ] `/proc/net/dev` readable with lo and eth0
- [ ] SIOCGIF* ioctls return correct values

### Must Have
- Single-netns device list with lo (ifindex=1) and eth0 (ifindex=2)
- Router with longest prefix match lookup for 127.0.0.0/8, 10.0.2.0/24, default via 10.0.2.2
- PortManager with TCP and UDP port tables, ephemeral allocation, REUSEADDR support
- /proc/net/dev, /proc/net/route, /proc/net/tcp (header), /proc/net/udp (header)
- SIOCGIFCONF, SIOCGIFFLAGS, SIOCGIFADDR, SIOCGIFNETMASK, SIOCGIFBRDADDR, SIOCGIFMTU, SIOCGIFHWADDR, SIOCGIFINDEX
- AF_NETLINK + NETLINK_ROUTE with RTM_GETLINK/GETADDR/GETROUTE dump + NLMSG_DONE
- All unsupported operations return errno or NLMSG_ERROR, never panic
- LTP-style test framework in single inet_test.rs

### Must NOT Have (Guardrails)
- Full net namespace implementation
- Data plane refactoring (smoltcp path remains)
- Per-test hacks or hardcoded shortcuts for individual test cases
- New scattered test binaries (all tests in inet_test.rs)
- Panics/todo!/unwrap on any user-triggerable code path
- RawSocket bind/listen/connect/accept implementation (keep returning EOPNOTSUPP)
- NAT/firewall hooks or netfilter infrastructure
- iproute2/ip/ss dependencies in tests
- veth, bridge, multi-machine, complex LTP net suite environment requirements
- netfilter, network namespace, policy routing

---

## Verification Strategy

> **ZERO HUMAN INTERVENTION** — ALL verification is agent-executed via QEMU boot + console output parsing.

### Test Decision
- **Infrastructure exists**: YES — QEMU integration tests
- **Automated tests**: Tests-after (no bun test/vitest — bare metal kernel)
- **Framework**: User-space test binary compiled + run in QEMU, console output parsed
- **Agent QA**: Each task verified by `make rv64-run` (or la64-run) + parse console for PASS/FAIL

### QA Policy
Every task includes agent-executed QA scenarios. Evidence captured from QEMU console output.
- **Build verification**: `make rv64-kernel-build-only` + `make la64-kernel-build-only` — zero warnings/errors
- **Runtime verification**: `make rv64-run` — kernel boots, inet_test runs, all cases PASS
- **Evidence**: Console output saved for each verification run

---

## Execution Strategy

### Parallel Execution Waves

```
Wave 1a — S1: Core Types + Tables (Start Immediately — 5 parallel tasks)
├── T1: net_core.rs: DeviceEntry + device_list
├── T2: net_core.rs: Default + loopback iface accessors
├── T3: routing.rs: RouteEntry + RouteTable
├── T4: routing.rs: Router struct + lookup_route
└── T5: port.rs: TCP/UDP port tables + ephemeral with range

Wave 1b — S1: Integration + Wiring (After 1a — 5 parallel tasks)
├── T6: port.rs: bind/unbind conflict detection
├── T7: bound.rs: BoundInner with logical iface tracking
├── T8: config.rs: Unified address source from net_core
├── T9: initializer: Wire net_core + routing + port init
└── T10: inet_test: [NET_CORE] test group

Wave 2 — S2: RoutingDevice Migration (After Wave 1b — 7 parallel tasks)
├── T11: adapter.rs: Remove hardcoded local_ip, use Router
├── T12: config.rs: Remove hardcoded IPs, use net_core
├── T13: raw/raw.rs: Remove hardcoded IPs (2 locations)
├── T14: socket/mod.rs: Remove GATEWAY/LOCAL_IP statics
├── T15: All files: Audit and remove remaining hardcoded IPs
├── T16: Debug logging: Route lookup decision logging
└── T17: inet_test: [NET_ROUTE] test group

Wave 3 — S3: /proc/net Observation (After Wave 2 — 5-7 parallel tasks)
├── T18: procfs/net_dev.rs: /proc/net/dev
├── T19: procfs/net_route.rs: /proc/net/route
├── T20: procfs/net_tcp.rs: /proc/net/tcp
├── T21: procfs/net_udp.rs: /proc/net/udp
├── T22: procfs/sys.rs: /proc/sys/net/ipv4/ip_forward
├── T23: procfs/files/mod.rs: Register /proc/net dir + entries
└── T24: inet_test: [PROC_NET] test group

Wave 4a — S4: SIOCGIF* Implementation (After Wave 3 — 7 parallel tasks)
├── T25: ioctl.rs: ifreq/ifconf compat structures
├── T26: ioctl.rs: SIOCGIFCONF
├── T27: ioctl.rs: SIOCGIFINDEX
├── T28: ioctl.rs: SIOCGIFFLAGS
├── T29: ioctl.rs: SIOCGIFADDR + SIOCGIFNETMASK
├── T30: ioctl.rs: SIOCGIFMTU + SIOCGIFHWADDR + SIOCGIFBRDADDR
├── T31: ioctl.rs: Set ioctl fallback (EPERM/EOPNOTSUPP)
└── T32: main dispatch + SocketFile::ioctl() wiring

Wave 4b — S4: Test (After 4a — sequential)
└── T33: inet_test: [NET_IOCTL] test group

Wave 5a — S5: NETLINK Implementation (After Wave 4b — 8 parallel tasks)
├── T34: netlink/mod.rs: AF_NETLINK socket registration
├── T35: netlink/netlink.rs: nlmsghdr/rtattr message helpers
├── T36: netlink/route.rs: RTM_GETLINK dump
├── T37: netlink/route.rs: RTM_GETADDR dump
├── T38: netlink/route.rs: RTM_GETROUTE dump
├── T39: netlink: Multipart response + NLMSG_DONE
├── T40: netlink: NLMSG_ERROR for unsupported
├── T41: sendmsg/recvmsg + Socket::alloc() wiring

Wave 5b — S5: Test (After 5a — sequential)
└── T42: inet_test: [RTNETLINK] test group

Wave 6 — S6: Advanced Semantics Tests (After Wave 5b — 6-8 parallel tasks)
├── T42: inet_test: [UDP_SEMANTICS] group (8 cases)
├── T43: inet_test: [TCP_POLL_TIMEOUT] group (8 cases)
├── T44: inet_test: [SOCKET_STRESS_SMALL] group (6 cases)
├── T45: inet_test: Test framework refactoring (LTP-style macros)
├── T46: inet_test: Group runner + summary statistics
├── T47: Whole-system regression: run all inet_test groups
├── T48: Doc/Work_Log.md: Update with migration summary
└── T49: AGENTS.md: Update net section with new architecture

Wave FINAL (After ALL tasks — 4 parallel reviews):
├── F1: Plan Compliance Audit (oracle)
├── F2: Code Quality Review (unspecified-high)
├── F3: Real Manual QA — QEMU full run (unspecified-high)
└── F4: Scope Fidelity Check (deep)
```

### Critical Path
```
T1 (net_core) → T3 (routing types) → T4 (Router::lookup_route)
  → T9 (init wiring) → T11 (adapter migration) → T12 (config migration)
  → T18-T23 (procfs) → T25-T32 (ioctl) → T34-T41 (netlink)
  → T43-T46 (advanced tests) → T47 (regression) → F1-F4 (verification)
```

---

## TODOs

- [x] 1. **net_core.rs: DeviceEntry struct + global device_list**

  **What to do**:
  - Create `os/src/net/net_core.rs` with a `DeviceEntry` struct containing: `ifindex: u32`, `name: &'static str`, `flags: u32`, `mtu: u32`, `hwaddr: [u8; 6]`, `ip_addrs: Vec<IpCidr>`, `operstate: u8`
  - Define Linux-compatible flag constants: `IFF_UP=0x1`, `IFF_BROADCAST=0x2`, `IFF_LOOPBACK=0x8`, `IFF_RUNNING=0x40`, `IFF_NOARP=0x80`, `IFF_MULTICAST=0x1000`
  - Create `IFACES: Mutex<Vec<DeviceEntry>>` global static
  - Implement `register_device(name, flags, mtu, hwaddr, ip_addrs) -> u32` returning ifindex
  - Implement `find_by_name(name: &str) -> Option<DeviceEntry>` and `find_by_index(idx: u32) -> Option<DeviceEntry>`
  - Initial registration: lo (ifindex=1, flags=IFF_UP|IFF_LOOPBACK|IFF_RUNNING, mtu=65536, addr=127.0.0.1/8) and eth0 (ifindex=2, flags=IFF_UP|IFF_BROADCAST|IFF_RUNNING|IFF_MULTICAST, mtu=1500, addr=10.0.2.15/24, hwaddr from NET_DEVICE)
  - If NET_DEVICE is None (no physical NIC), still register lo but skip eth0, log warning

  **Must NOT do**:
  - Do NOT implement full NetNamespace struct — single global is sufficient
  - Do NOT add netlink/kobject integration — pure data structure only

  **Recommended Agent Profile**:
  - **Category**: `quick` — straightforward struct + static initialization
  - **Skills**: `[]`
  - **Skills Evaluated but Omitted**: `mango-worklog` (not yet, after Wave completion)

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T3, T5)
  - **Parallel Group**: Wave 1
  - **Blocks**: T2, T8, T9
  - **Blocked By**: None

  **References**:
  - `os/src/net/config.rs:30-40` — NetInterfaceInner pattern for wrapping Mutex<Option<...>>
  - `os/src/drivers/net/mod.rs` — NetDevice trait for getting hwaddr
  - `os/src/net/adapter.rs:16-20` — existing RoutingDevice struct (hw_addr extraction pattern)
  - DragonOS kernel/src/net/net_core.rs — DeviceEntry pattern (device_list, ifindex assignment)

  **Acceptance Criteria**:
  - [ ] `os/src/net/net_core.rs` created with DeviceEntry struct and IFACES global
  - [ ] `make rv64-kernel-build-only` passes
  - [ ] `find_by_name("lo")` returns Some with correct ifindex/flags/addr
  - [ ] `find_by_name("eth0")` returns Some when NET_DEVICE is available
  - [ ] `find_by_name("nonexistent")` returns None without panicking

  **QA Scenarios**:
  ```
  Scenario: lo interface registered with correct attributes
    Tool: QEMU boot + console trace
    Preconditions: Kernel boots, net::init() called
    Steps:
      1. Check boot log for "[net_core] registered lo (ifindex=1)"
      2. Verify lo flags include IFF_LOOPBACK (0x8)
      3. Verify lo ip_addrs contains 127.0.0.1/8
    Expected Result: lo appears in IFACES with ifindex=1, IFF_LOOPBACK=0x8, addr=127.0.0.1/8
    Failure Indicators: Missing lo entry, wrong ifindex, wrong flags
    Evidence: .sisyphus/evidence/task-1-boot-log.txt

  Scenario: eth0 interface registered with correct attributes
    Tool: QEMU boot + console trace
    Preconditions: NET_DEVICE available
    Steps:
      1. Check boot log for "[net_core] registered eth0 (ifindex=2)"
      2. Verify eth0 flags include IFF_UP|IFF_BROADCAST|IFF_RUNNING
      3. Verify eth0 ip_addrs contains 10.0.2.15/24
    Expected Result: eth0 appears with ifindex=2, correct flags, addr=10.0.2.15/24
    Evidence: .sisyphus/evidence/task-1-boot-log.txt
  ```

  **Commit**: YES (groups with Wave 1)
  - Message: `feat(net): add net_core DeviceEntry with device list for lo and eth0`
  - Files: `os/src/net/net_core.rs`, `os/src/net/mod.rs`

- [x] 2. **net_core.rs: default_iface + loopback_iface accessors**

  **What to do**:
  - Add `default_iface() -> Option<DeviceEntry>` function that returns eth0 (or lo if no eth0)
  - Add `loopback_iface() -> Option<DeviceEntry>` function that returns lo
  - Add `default_gateway() -> Option<Ipv4Address>` returning 10.0.2.2 (only when eth0 exists)
  - Add `local_port_range() -> (u16, u16)` returning (32768, 60999) — configurable via static
  - Add `iface_ip(ifindex: u32) -> Option<IpAddress>` helper

  **Must NOT do**:
  - Do NOT make these mutable at runtime (read-only for now)
  - Do NOT add config file parsing

  **Recommended Agent Profile**:
  - **Category**: `quick` — simple accessor functions
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T3, T5, T6)
  - **Parallel Group**: Wave 1
  - **Blocks**: T8, T9
  - **Blocked By**: T1

  **References**:
  - `os/src/net/socket/mod.rs:145-146` — existing GATEWAY/LOCAL_IP statics (to be replaced)
  - `os/src/net/config.rs:345-351` — lookup_source_ip pattern

  **Acceptance Criteria**:
  - [ ] `loopback_iface()` returns lo with ifindex=1
  - [ ] `default_iface()` returns eth0 when eth0 exists
  - [ ] `default_gateway()` returns 10.0.2.2
  - [ ] `local_port_range()` returns (32768, 60999)
  - [ ] `iface_ip(1)` returns 127.0.0.1

  **QA Scenarios**:
  ```
  Scenario: default_iface returns eth0 when NIC available
    Tool: QEMU boot
    Preconditions: NET_DEVICE available
    Steps: Boot kernel, wait for net init
    Expected Result: default_iface().name == "eth0"
    Evidence: .sisyphus/evidence/task-2-boot-log.txt

  Scenario: loopback_iface always returns lo
    Tool: QEMU boot
    Preconditions: Any kernel boot
    Steps: Boot kernel
    Expected Result: loopback_iface().name == "lo", ifindex == 1
    Evidence: .sisyphus/evidence/task-2-boot-log.txt
  ```

  **Commit**: SQUASH with T1

- [x] 3. **routing.rs: RouteEntry + RouteTable types**

  **What to do**:
  - Create `os/src/net/routing.rs`
  - Define `RouteType` enum: `Connected`, `Static`, `Default`
  - Define `RouteEntry` struct: `destination: IpCidr`, `next_hop: Option<IpAddress>`, `ifindex: u32`, `metric: u32`, `route_type: RouteType`
  - Define `RouteTable` struct: `entries: Vec<RouteEntry>`
  - Implement `RouteTable::new()` creating empty table
  - Implement `RouteTable::add(entry: RouteEntry)` adding to entries vec
  - Implement `RouteTable::remove(destination: IpCidr)` removing matching entry
  - Derive Clone, Debug for all types

  **Must NOT do**:
  - Do NOT implement lookup_route yet (T4)
  - Do NOT add netlink serialization
  - Do NOT add route type other than Connected/Static/Default

  **Recommended Agent Profile**:
  - **Category**: `quick` — data type definitions
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T1, T2, T5)
  - **Parallel Group**: Wave 1
  - **Blocks**: T4, T9
  - **Blocked By**: None

  **References**:
  - `smoltcp::wire::IpCidr` — used throughout codebase for CIDR representation
  - `os/src/net/config.rs:59-72` — existing smoltcp route registration pattern
  - DragonOS kernel/src/net/routing/ — RouteEntry fields

  **Acceptance Criteria**:
  - [ ] `os/src/net/routing.rs` created with RouteEntry, RouteTable
  - [ ] `make rv64-kernel-build-only` passes
  - [ ] RouteEntry::new(IpCidr::new(IpAddress::v4(127,0,0,1), 8), None, 1, 0, RouteType::Connected) compiles

  **QA Scenarios**: (unit-test only, verified by T10 inet_test)
  ```
  Scenario: RouteTable add and iteration works
    Tool: QEMU boot (verified in T10 inet_test)
    Preconditions: Router initialized
    Steps: T10 will verify routes via inet_test
    Expected Result: RouteTable contains expected entries
    Evidence: .sisyphus/evidence/task-10-routing-log.txt
  ```

  **Commit**: SQUASH with T4

- [x] 4. **routing.rs: Router struct + lookup_route (longest prefix match)**

  **What to do**:
  - Define `Router` struct wrapping `RouteTable` with `add_route`/`remove_route` methods
  - Implement `Router::lookup_route(dest: Ipv4Address) -> Option<&RouteEntry>` using longest prefix match
  - Algorithm: scan all entries, filter those where `dest` is in `entry.destination` network, pick entry with max prefix_len
  - Implement `Router::init_default()`: creates Router with:
    - `127.0.0.0/8 dev lo Connected metric=0`
    - `10.0.2.0/24 dev eth0 Connected metric=0`
    - `0.0.0.0/0 via 10.0.2.2 dev eth0 Default metric=100`
  - If NET_DEVICE is None, only add lo route
  - Add `debug!()` logging in lookup_route: `"route lookup: dest={} -> ifindex={} ifname={} next_hop={:?}"`

  **Must NOT do**:
  - Do NOT implement policy routing, ECMP, or route caching
  - Do NOT implement RTM_NEWROUTE/RTM_DELROUTE

  **Recommended Agent Profile**:
  - **Category**: `quick` — straightforward scan algorithm + initialization
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (depends on T3 types)
  - **Parallel Group**: Wave 1 (sequential within wave)
  - **Blocks**: T9, T11, T12
  - **Blocked By**: T3

  **References**:
  - `smoltcp::wire::Ipv4Address` and `IpAddress` — used in routing
  - `os/src/net/config.rs:69-72` — existing `add_default_ipv4_route` pattern
  - DragonOS kernel/src/net/routing/ — `Router::lookup_route` implementation (Vec scan + max_by_key)

  **Acceptance Criteria**:
  - [ ] `lookup_route(Ipv4Address::new(127,0,0,1))` returns lo route (ifindex=1)
  - [ ] `lookup_route(Ipv4Address::new(127,1,2,3))` returns lo route (127.0.0.0/8 match)
  - [ ] `lookup_route(Ipv4Address::new(10,0,2,15))` returns eth0 route (ifindex=2)
  - [ ] `lookup_route(Ipv4Address::new(10,0,2,3))` returns eth0 route (10.0.2.0/24 match)
  - [ ] `lookup_route(Ipv4Address::new(8,8,8,8))` returns default route (ifindex=2, next_hop=10.0.2.2)
  - [ ] `lookup_route(Ipv4Address::new(192,168,1,1))` returns default route

  **QA Scenarios**:
  ```
  Scenario: Longest prefix match for 127.0.0.1
    Tool: Console debug log
    Preconditions: Router initialized
    Steps: Trigger lookup_route(127.0.0.1) during network operation
    Expected Result: Returns lo entry, prefix_len=8
    Evidence: .sisyphus/evidence/task-4-route-log.txt

  Scenario: Default route for external addresses
    Tool: Console debug log
    Preconditions: Router initialized
    Steps: Trigger lookup_route(8.8.8.8)
    Expected Result: Returns default route entry with next_hop=10.0.2.2, ifindex=2
    Evidence: .sisyphus/evidence/task-4-route-log.txt
  ```

  **Commit**: `feat(net): add routing module with Router and longest prefix match lookup`
  - Files: `os/src/net/routing.rs`

- [x] 5. **port.rs: TCP/UDP port tables + ephemeral alloc with configurable range**

  **What to do**:
  - Enhance `os/src/net/socket/inet/common/port.rs` `PortManager`:
    - Add `TCP_PORTS: Mutex<HashMap<u16, PortBinding>>` — one binding per TCP port
    - Add `UDP_PORTS: Mutex<HashMap<u16, Vec<UdpPortBinding>>>` — multiple bindings per UDP port (REUSEADDR)
  - Define `PortBinding` struct: `port: u16, addr: Option<Ipv4Address>, socket_weak: Weak<dyn Socket>`
  - Define `UdpPortBinding` struct: `port: u16, addr: Option<Ipv4Address>, reuseaddr: bool, reuseport: bool, socket_weak: Weak<dyn Socket>`
  - Change ephemeral range from hardcoded 49152-65534 to use `local_port_range` from net_core (32768-60999)
  - Add `register_tcp_bind()`, `unregister_tcp_bind()`, `register_udp_bind()`, `unregister_udp_bind()` methods
  - Add `check_tcp_conflict(port, addr) -> bool` — checks TCP table
  - Add `check_udp_conflict(port, addr, reuseaddr) -> Result<(), SyscallErr>` — checks UDP table with REUSEADDR logic
  - Update `alloc_ephemeral_port()` to skip ports already in TCP_PORTS and UDP_PORTS
  - Clean up dead Weak references in register operations

  **Must NOT do**:
  - Do NOT remove the existing `check_bind_conflict` that scans fd_table — keep as fallback
  - Do NOT implement SO_REUSEPORT full semantics (just track the flag)
  - Do NOT change the public API of PortManager (bind_port signature stays same)

  **Recommended Agent Profile**:
  - **Category**: `deep` — careful port management with correct concurrent semantics
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T1, T2, T3)
  - **Parallel Group**: Wave 1
  - **Blocks**: T6, T9
  - **Blocked By**: T2 (for local_port_range)

  **References**:
  - `os/src/net/socket/inet/common/port.rs:1-132` — current PortManager implementation
  - `os/src/net/socket/inet/common/port.rs:9-11` — current ephemeral range constants to replace
  - DragonOS kernel/src/net/socket/inet/common/port.rs — TCP/UDP table structures

  **Acceptance Criteria**:
  - [ ] TCP_PORTS and UDP_PORTS tables exist and are populated on bind
  - [ ] ephemeral port alloc uses local_port_range (32768-60999)
  - [ ] `alloc_ephemeral_port()` skips ports already in TCP/UDP tables
  - [ ] `unregister_tcp_bind()` removes port from table
  - [ ] Dead Weak refs are cleaned up (no memory leak)
  - [ ] `check_tcp_conflict` returns true for duplicate TCP port

  **QA Scenarios**:
  ```
  Scenario: Ephemeral port allocation respects range
    Tool: QEMU boot + inet_test
    Preconditions: PortManager initialized
    Steps: alloc_ephemeral_port() called, verify port in [32768, 60999]
    Expected Result: Port in range, does not collide with existing bindings
    Evidence: .sisyphus/evidence/task-5-port-log.txt

  Scenario: TCP port register then unregister makes port available
    Tool: QEMU boot + inet_test
    Steps: register_tcp_bind(8080), check_tcp_conflict(8080, addr)=true, unregister_tcp_bind(8080), check_tcp_conflict(8080, addr)=false
    Expected Result: Port released after unregister
    Evidence: .sisyphus/evidence/task-5-port-log.txt
  ```

  **Commit**: SQUASH with T6

- [x] 6. **port.rs: bind/unbind integration with PortManager tables**

  **What to do**:
  - Update `PortManager::bind_port()` to register in TCP_PORTS or UDP_PORTS table after successful bind
  - Update `PortManager::check_bind_conflict()` to check port tables FIRST, then fall back to fd_table scan
  - For TCP: check TCP_PORTS table, conflict if same port and address matches (or wildcard on either side)
  - For UDP: check UDP_PORTS table, allow bind if REUSEADDR is set on BOTH existing and new socket
  - For TCP: handle TIME_WAIT by checking smoltcp TCP state — `with_tcp_mut(handle, |s| s.state())` — treat TIME_WAIT as occupied
  - Add log messages for bind failures: `"bind conflict: port={} existing_addr={:?} new_addr={:?} type={:?}"`

  **Must NOT do**:
  - Do NOT modify the Socket trait or individual socket implementations
  - Do NOT add new public API beyond what's needed

  **Recommended Agent Profile**:
  - **Category**: `deep` — careful integration with existing bind path
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (depends on T5 port tables)
  - **Parallel Group**: Wave 1
  - **Blocks**: T9
  - **Blocked By**: T5

  **References**:
  - `os/src/net/syscall/bind.rs` — sys_bind calls PortManager::bind_port
  - `os/src/net/socket/inet/stream/inner.rs` — TCP state enum, `with_tcp_mut` helper
  - `smoltcp::socket::tcp::State::TimeWait` — TIME_WAIT state check

  **Acceptance Criteria**:
  - [ ] `bind_port()` for TCP registers in TCP_PORTS
  - [ ] `bind_port()` for UDP registers in UDP_PORTS
  - [ ] Duplicate TCP bind returns EADDRINUSE
  - [ ] UDP REUSEADDR allows same port bind (when both have REUSEADDR)
  - [ ] TCP TIME_WAIT port treated as occupied
  - [ ] Close/cleanup removes from port tables
  - [ ] Existing inet_test pass (no regression)

  **QA Scenarios**:
  ```
  Scenario: TCP duplicate bind returns EADDRINUSE
    Tool: QEMU + inet_test (net_core05)
    Steps: bind(sock1, 0.0.0.0:8080) OK, bind(sock2, 0.0.0.0:8080) → EADDRINUSE
    Expected Result: Second bind returns -EADDRINUSE
    Evidence: .sisyphus/evidence/task-6-bind-conflict-log.txt

  Scenario: Close releases port for reuse
    Tool: QEMU + inet_test (net_core06)
    Steps: bind(TCP, port=9000), close socket, bind(TCP, port=9000) → OK
    Expected Result: Port reusable after close
    Evidence: .sisyphus/evidence/task-6-port-reuse-log.txt
  ```

  **Commit**: `feat(net): enhance PortManager with TCP/UDP port tables and proper conflict detection`
  - Files: `os/src/net/socket/inet/common/port.rs`

- [x] 7. **bound.rs: BoundInner with logical iface tracking**

  **What to do**:
  - Create `os/src/net/socket/inet/common/bound.rs`
  - Define `BoundInner` struct: `socket_handle: SocketHandle`, `ifindex: u32`, `bound_addr: Option<IpAddress>`, `bound_port: u16`
  - Implement `BoundInner::new(handle, ifindex)` constructor
  - Add `bound_iface() -> Option<DeviceEntry>` helper that looks up ifindex in IFACES
  - Update UDP and TCP socket bind/connect paths to create BoundInner and store it
  - For now, store BoundInner as a field on UdpSocket and TcpSocket structs (not in a per-iface SocketSet)
  - This is a lightweight struct that does NOT change how smoltcp SocketSet works

  **Must NOT do**:
  - Do NOT change the smoltcp SocketSet architecture
  - Do NOT move sockets between interfaces
  - Do NOT implement per-iface polling

  **Recommended Agent Profile**:
  - **Category**: `deep` — touches core socket structs, needs careful integration
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T1-T4 implementations, after those complete)
  - **Parallel Group**: Wave 1
  - **Blocks**: T11, T12 (route migration uses ifindex)
  - **Blocked By**: T1 (DeviceEntry), T4 (Router for ifindex lookup)

  **References**:
  - `os/src/net/socket/inet/datagram/udp.rs` — UdpSocket struct
  - `os/src/net/socket/inet/stream/inner.rs` — TcpSocket struct / inner state
  - `os/src/net/socket/inet/stream/lifecycle.rs` — TCP bind/connect paths
  - DragonOS kernel/src/net/socket/inet/common/mod.rs — BoundInner pattern

  **Acceptance Criteria**:
  - [ ] BoundInner created on successful UDP bind with correct ifindex
  - [ ] BoundInner created on successful TCP bind with correct ifindex
  - [ ] `bound_iface()` returns the DeviceEntry for the bound interface
  - [ ] Existing inet_test pass (no regression)
  - [ ] No change to smoltcp SocketSet management

  **QA Scenarios**:
  ```
  Scenario: UDP bind to 127.0.0.1 creates BoundInner with ifindex=1
    Tool: QEMU + inet_test (net_core01)
    Steps: Create UDP socket, bind to 127.0.0.1:12345
    Expected Result: BoundInner.ifindex == 1, bound_iface().name == "lo"
    Evidence: .sisyphus/evidence/task-7-boundinner-log.txt
  ```

  **Commit**: `feat(net): add BoundInner for logical interface tracking on sockets`
  - Files: `os/src/net/socket/inet/common/bound.rs`, `os/src/net/socket/inet/datagram/udp.rs`, `os/src/net/socket/inet/stream/inner.rs`

- [x] 8. **config.rs: Unified address source from net_core objects**

  **What to do**:
  - In `os/src/net/config.rs`, refactor `NetInterfaceInner::new()` to source IP addresses and routes from net_core:
    - IP addresses: iterate IFACES entries, push each device's ip_addrs to `iface.update_ip_addrs()`
    - Default route: use `default_gateway()` from net_core instead of hardcoded `Ipv4Address::new(10, 0, 2, 2)`
  - In `lookup_source_ip()`, use `Router::lookup_route()` result to determine source IP instead of hardcoded logic
  - Keep the smoltcp `iface.routes_mut().add_default_ipv4_route()` call but source gateway from net_core
  - Add log: `"[net::config] sourced addresses from net_core: lo={:?}, eth0={:?}"`

  **Must NOT do**:
  - Do NOT remove the smoltcp Interface/SocketSet — just change how they're configured
  - Do NOT change the poll mechanism

  **Recommended Agent Profile**:
  - **Category**: `quick` — refactoring existing init to use new data sources
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (depends on T1, T2, T4)
  - **Parallel Group**: Wave 1
  - **Blocks**: T9
  - **Blocked By**: T1, T2, T4

  **References**:
  - `os/src/net/config.rs:56-72` — current hardcoded iface IP and route setup
  - `os/src/net/config.rs:345-351` — current lookup_source_ip with hardcoded logic

  **Acceptance Criteria**:
  - [ ] IP addresses sourced from IFACES, not hardcoded
  - [ ] Default gateway sourced from net_core::default_gateway()
  - [ ] `lookup_source_ip()` uses Router instead of hardcoded pattern match
  - [ ] Existing inet_test pass (identical behavior, different data source)
  - [ ] `make rv64-kernel-build-only` passes

  **QA Scenarios**:
  ```
  Scenario: Kernel boots with unified address source
    Tool: QEMU boot
    Steps: Boot kernel, check net init logs
    Expected Result: "sourced addresses from net_core: lo=[127.0.0.1/8], eth0=[10.0.2.15/24]"
    Evidence: .sisyphus/evidence/task-8-unified-source-log.txt
  ```

  **Commit**: `refactor(net): source IP addresses and gateway from net_core instead of hardcoded`
  - Files: `os/src/net/config.rs`

- [x] 9. **Initializer: Wire net_core, routing, port initialization**

  **What to do**:
  - Create `os/src/net/net_core.rs` init function that:
    1. Registers lo with full attributes
    2. If NET_DEVICE available: reads hwaddr, registers eth0 with full attributes
    3. Initializes Router with default routes
    4. Prints init summary: `"[net_core] initialized: {} devices, {} routes, port_range={:?}"`
  - Call this from `os/src/net/config.rs::init()` BEFORE `NET_INTERFACE.init()`
  - Ensure init is idempotent (check if already initialized via atomic flag)

  **Must NOT do**:
  - Do NOT add config file parsing
  - Do NOT add dynamic device hotplug

  **Recommended Agent Profile**:
  - **Category**: `quick` — simple init function wiring
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (depends on T1-T8)
  - **Parallel Group**: Wave 1 (final task)
  - **Blocks**: T10 (test can now run)
  - **Blocked By**: T1, T2, T4, T6, T8

  **References**:
  - `os/src/net/config.rs:21-27` — current init() function
  - `os/src/drivers/net/mod.rs` — NET_DEVICE global for hwaddr access

  **Acceptance Criteria**:
  - [ ] Kernel boots, net_core init log appears
  - [ ] lo registered with ifindex=1 before NET_INTERFACE.init()
  - [ ] eth0 registered with ifindex=2 when NIC available
  - [ ] Router initialized with 3 default routes
  - [ ] PortManager ready after init
  - [ ] Existing inet_test pass (no regression)

  **QA Scenarios**:
  ```
  Scenario: Full init sequence completes without errors
    Tool: QEMU boot
    Steps: Boot kernel
    Expected Result: Log shows "[net_core] initialized: 2 devices, 3 routes, port_range=(32768, 60999)"
    Evidence: .sisyphus/evidence/task-9-init-log.txt
  ```

  **Commit**: `feat(net): wire net_core device/routing/port initialization into boot sequence`
  - Files: `os/src/net/net_core.rs`, `os/src/net/config.rs`

- [x] 10. **inet_test: [NET_CORE] test group (6 LTP-style cases)**

  **What to do**:
  - In `user/src/bin/inet_test.rs`, add LTP-style test framework macros:
    - `TPASS(group, name, msg)` — prints `"[group] TPASS: name: msg"`
    - `TFAIL(group, name, msg)` — prints `"[group] TFAIL: name: msg"`, increments fail counter
    - `TBROK(group, name, msg)` — prints `"[group] TBROK: name: msg"`, increments broken counter
    - `TCONF(group, name, msg)` — prints `"[group] TCONF: name: msg"`, increments conf counter
  - Add test result counters: `total, passed, failed, broken, conf`
  - Add 6 cases for [NET_CORE]:
    1. `net_core01_interface_basic`: verify lo and eth0 exist via proc/net/dev or logical check
    2. `net_core02_loopback_and_default_iface`: verify loopback=lo, default=eth0
    3. `net_core03_route_lookup`: verify route lookups for 127.0.0.1, 10.0.2.15, 8.8.8.8
    4. `net_core04_ephemeral_port_range`: bind ephemeral, check port in [32768, 60999]
    5. `net_core05_port_bind_conflict`: TCP bind same port → EADDRINUSE
    6. `net_core06_port_reuse_after_close`: close TCP, rebind same port → success
  - Route lookup tests use indirect methods (e.g., socket behavior, or new test syscall if available)
  - Each case cleans up resources, returns TPASS/TFAIL/TBROK/TCONF

  **Must NOT do**:
  - Do NOT add new syscalls for testing route lookup unless absolutely necessary — use existing syscalls
  - Do NOT depend on /proc/net being implemented (Stage 3) — use indirect verification
  - Do NOT create new test binaries — all in inet_test.rs

  **Recommended Agent Profile**:
  - **Category**: `visual-engineering` — user-space test code with careful errno checking
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (needs kernel changes first)
  - **Parallel Group**: Wave 1 (final task)
  - **Blocks**: None (end of Wave 1)
  - **Blocked By**: T9

  **References**:
  - `user/src/bin/inet_test.rs:1342-1387` — current main() and test registration pattern
  - `user/src/bin/inet_test.rs:12-55` — sockaddr_in struct and syscall wrappers
  - AGENTS.md §inet_test — test must use LTP-style output

  **Acceptance Criteria**:
  - [ ] LTP-style macros output TPASS/TFAIL/TBROK/TCONF format
  - [ ] All 6 net_core cases pass
  - [ ] net_core05: duplicate TCP bind returns -EADDRINUSE
  - [ ] net_core06: port reusable after close
  - [ ] net_core03: route lookups return expected results (indirect verification)
  - [ ] Each case cleans up resources (close fds)
  - [ ] Total/passed/failed/broken/conf summary printed

  **QA Scenarios**:
  ```
  Scenario: net_core05 port bind conflict
    Tool: QEMU + inet_test
    Steps: Build + run, check inet_test console output
    Expected Result: "[NET_CORE] TPASS: net_core05_port_bind_conflict: duplicate bind returned EADDRINUSE as expected"
    Evidence: .sisyphus/evidence/task-10-net-core-test-log.txt

  Scenario: net_core06 port reuse after close
    Tool: QEMU + inet_test
    Steps: Build + run
    Expected Result: "[NET_CORE] TPASS: net_core06_port_reuse_after_close: port reusable after close"
    Evidence: .sisyphus/evidence/task-10-net-core-test-log.txt
  ```

  **Commit**: `test: add NET_CORE test group to inet_test with LTP-style output framework`
  - Files: `user/src/bin/inet_test.rs`

- [x] 11. **adapter.rs: Remove hardcoded local_ip, use Router::lookup_route**

  **What to do**:
  - In `os/src/net/adapter.rs` `RoutingTxToken::consume()` (line ~106):
    - Remove hardcoded `local_ip = &[10, 0, 2, 15]`
    - Remove the EthernetProtocol::Ipv4/IPv6/Arp branch that checks `dst_ip == local_ip` (lines ~135-157)
    - Instead, for each outbound packet, extract destination IP, call `Router::lookup_route()`
    - Route to lo if lookup_route returns ifindex=1, route to eth if ifindex=2
    - If lookup_route returns None: drop packet, log `"[Routing] no route for dst={}, dropping"`
    - Keep MAC-based routing for loopback (dst_mac == hw_addr → send_to_lo, broadcast → both)
    - Log routing decision: `"[RoutingTxToken] dst={} -> ifindex={} ifname={}"`

  **Must NOT do**:
  - Do NOT change the TxToken trait implementation
  - Do NOT change the receive path (RxToken)
  - Do NOT remove the MAC-based routing logic (needed for ARP/broadcast)

  **Recommended Agent Profile**:
  - **Category**: `deep` — critical data path change, must not break connectivity
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (depends on Router from Wave 1)
  - **Parallel Group**: Wave 2
  - **Blocks**: T12, T13, T17
  - **Blocked By**: T4 (Router::lookup_route), T9 (initialization)

  **References**:
  - `os/src/net/adapter.rs:83-186` — full RoutingTxToken::consume (routing logic to refactor)
  - `os/src/net/adapter.rs:106` — the hardcoded `local_ip = &[10, 0, 2, 15]` to remove
  - `os/src/net/routing.rs` — Router::lookup_route to call

  **Acceptance Criteria**:
  - [ ] No hardcoded `10.0.2.15` in adapter.rs
  - [ ] Outbound to 127.x.x.x routed to lo (via Router)
  - [ ] Outbound to 10.0.2.x routed to eth (via Router)
  - [ ] Outbound to external routed to eth (default route)
  - [ ] No route = packet dropped, log printed, no panic
  - [ ] UDP/TCP loopback inet_test still passes
  - [ ] DNS/HTTP inet_test still passes

  **QA Scenarios**:
  ```
  Scenario: UDP loopback still works after migration
    Tool: QEMU + inet_test (existing udp_loopback)
    Steps: Run inet_test udp_loopback
    Expected Result: [PASS] udp_loopback
    Evidence: .sisyphus/evidence/task-11-loopback-log.txt

  Scenario: DNS query still works after migration
    Tool: QEMU + inet_test (existing udp_external_dns)
    Steps: Run inet_test udp_external_dns
    Expected Result: [PASS] udp_external_dns
    Evidence: .sisyphus/evidence/task-11-dns-log.txt
  ```

  **Commit**: `refactor(net): replace hardcoded IP routing in adapter.rs with Router::lookup_route`
  - Files: `os/src/net/adapter.rs`

- [x] 12. **config.rs: Remove hardcoded IPs, use net_core (remaining refs)**

  **What to do**:
  - In `os/src/net/config.rs`:
    - Remove `lookup_source_ip()` hardcoded logic (127.x → 127.0.0.1, else → 10.0.2.15)
    - Replace with Router-based lookup: find route for dest_ip, get source IP from the route's iface
    - Remove the hardcoded IpCidr pushes (already done in T8, verify complete)
  - In `os/src/net/socket/mod.rs`:
    - Remove `pub static GATEWAY: IpAddress = ...` (line 145)
    - Remove `pub static LOCAL_IP: IpAddress = ...` (line 146)
    - Update all references to GATEWAY/LOCAL_IP to use net_core functions
    - Check mod.rs re-exports — remove GATEWAY/LOCAL_IP from the pub use block

  **Must NOT do**:
  - Do NOT change NET_INTERFACE initialization order
  - Do NOT change the poll mechanism

  **Recommended Agent Profile**:
  - **Category**: `quick` — straightforward refactoring
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T13, T14, T15 — all remove hardcoded IPs)
  - **Parallel Group**: Wave 2
  - **Blocks**: T17
  - **Blocked By**: T11, T4

  **References**:
  - `os/src/net/config.rs:345-351` — lookup_source_ip to replace
  - `os/src/net/socket/mod.rs:145-146` — GATEWAY/LOCAL_IP to remove
  - `os/src/net/mod.rs:19-24` — re-exports that include GATEWAY/LOCAL_IP

  **Acceptance Criteria**:
  - [ ] No hardcoded IP in config.rs lookup_source_ip
  - [ ] No GATEWAY static in socket/mod.rs
  - [ ] No LOCAL_IP static in socket/mod.rs
  - [ ] All references to GATEWAY/LOCAL_IP resolved to net_core functions
  - [ ] `make rv64-kernel-build-only` and `make la64-kernel-build-only` pass
  - [ ] Existing inet_test pass

  **QA Scenarios**: (covered by T17 NET_ROUTE tests)
  ```
  Scenario: Build passes without GATEWAY/LOCAL_IP statics
    Tool: make rv64-kernel-build-only
    Steps: Compile
    Expected Result: Zero errors related to missing GATEWAY/LOCAL_IP
    Evidence: .sisyphus/evidence/task-12-build-log.txt
  ```

  **Commit**: SQUASH with T11

- [x] 13. **raw/raw.rs: Remove hardcoded IPs + replace todo!() with EOPNOTSUPP**

  **What to do**:
  - In `os/src/net/socket/inet/raw/raw.rs`:
    - Remove hardcoded `[127, 0, 0, 1]` at approximately line 113
    - Remove hardcoded `[10, 0, 2, 15]` at approximately line 115
    - Replace with calls to net_core functions: source IP selection via Router
    - **CRITICAL**: Replace `todo!()` in `bind()`, `listen()`, `connect()`, `accept()` with `Err(SyscallErr::EOPNOTSUPP)` — they MUST NOT panic
    - Ensure ALL user-triggerable paths through RawSocket return errno, never panic

  **Must NOT do**:
  - Do NOT implement bind/listen/connect/accept — just replace todo!() with Err(EOPNOTSUPP)
  - Do NOT change RawSocket data path logic

  **Recommended Agent Profile**:
  - **Category**: `quick` — targeted IP replacement + todo→errno, no behavioral change
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T12, T14, T15)
  - **Parallel Group**: Wave 2
  - **Blocks**: T17
  - **Blocked By**: T4, T9

  **References**:
  - `os/src/net/socket/inet/raw/raw.rs` — RawSocket implementation with todo!() locations
  - Metis report: lines ~113, ~115 have hardcoded IPs; bind/listen/connect/accept are todo!()

  **Acceptance Criteria**:
  - [ ] No hardcoded IPs in raw.rs (sourced from net_core/Router)
  - [ ] `bind()`, `listen()`, `connect()`, `accept()` return `Err(SyscallErr::EOPNOTSUPP)`, NOT panic
  - [ ] `make rv64-kernel-build-only` passes
  - [ ] Existing raw socket tests pass (no regression)

  **QA Scenarios**:
  ```
  Scenario: RawSocket bind returns EOPNOTSUPP
    Tool: QEMU + inet_test
    Preconditions: Kernel booted, net initialized
    Steps:
      1. Create raw socket: socket(AF_INET, SOCK_RAW, IPPROTO_RAW)
      2. Bind to localhost: bind(fd, 127.0.0.1:0)
      3. Check return value
    Expected Result: bind returns -EOPNOTSUPP (or -EAFNOSUPPORT), kernel does NOT crash
    Failure Indicators: Kernel panic, OOM, or unexpected errno
    Evidence: .sisyphus/evidence/task-13-raw-nopanic.txt

  Scenario: RawSocket listen returns EOPNOTSUPP
    Tool: QEMU + inet_test
    Steps:
      1. Create raw socket
      2. listen(fd, 5)
      3. Check return value
    Expected Result: listen returns -EOPNOTSUPP, kernel does NOT crash
    Evidence: .sisyphus/evidence/task-13-raw-nopanic.txt
  ```

  **Commit**: SQUASH with T11

- [x] 14. **All files: Audit and remove remaining hardcoded IPs (16 locations)**

  **What to do**:
  - Search ALL Rust files under `os/src/net/` for hardcoded IPv4 address patterns:
    - `[127, 0, 0, 1]`
    - `[10, 0, 2, 15]`
    - `Ipv4Address::new(127, 0, 0, 1)`
    - `Ipv4Address::new(10, 0, 2, 15)`
    - `Ipv4Address::new(10, 0, 2, 2)`
  - Replace each with call to net_core functions (or appropriate Router lookup)
  - **Exclude from grep**: `os/src/net/net_core.rs` and `os/src/net/routing.rs` — these are the central default-source files that INTENTIONALLY contain the canonical IP/gateway defaults
  - Metis found 16 locations across 7 files — systematically verify and fix ALL non-central locations
  - For locations that legitimately need constants (like test mock data), document why with a comment: `// SAFETY: test mock data, not production routing`
  - After all replacements, run grep again (excluding net_core.rs + routing.rs) to verify zero remaining hardcoded IPs

  **Must NOT do**:
  - Do NOT remove IP defaults from net_core.rs or routing.rs — these are the single source of truth
  - Do NOT touch hardcoded IPs in user-space test code (inet_test, user tests)
  - Do NOT change smoltcp internal constants

  **Recommended Agent Profile**:
  - **Category**: `quick` — systematic grep-and-replace with validation
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T12, T13)
  - **Parallel Group**: Wave 2
  - **Blocks**: T17
  - **Blocked By**: T4, T9

  **References**:
  - Metis report: 16 hardcoded IP locations across 7 files
  - Central files (EXCLUDE from grep): `os/src/net/net_core.rs`, `os/src/net/routing.rs`
  - Command: `grep -rn "10, 0, 2, 15\|127, 0, 0, 1\|10, 0, 2, 2" os/src/net/ --exclude="net_core.rs" --exclude="routing.rs"`

  **Acceptance Criteria**:
  - [ ] Zero hardcoded IPv4 addresses in `os/src/net/` EXCEPT net_core.rs and routing.rs (central defaults)
  - [ ] All non-central references use net_core/routing functions
  - [ ] `make rv64-kernel-build-only` passes
  - [ ] Existing inet_test pass (no regression)

  **QA Scenarios**:
  ```
  Scenario: Zero remaining hardcoded IPs in non-central net files
    Tool: grep
    Steps: grep -rn "10, 0, 2, 15\|127, 0, 0, 1" os/src/net/ --exclude="net_core.rs" --exclude="routing.rs"
    Expected Result: No matches (or comments only)
    Evidence: .sisyphus/evidence/task-14-no-hardcoded-ips.txt
  ```

  **Commit**: SQUASH with T11

- [x] 15. **Debug logging: Route lookup decision logging**

  **What to do**:
  - In `Router::lookup_route()`, add structured logging: `"route_lookup: src={} dst={} -> ifindex={} ifname={} next_hop={:?} route_type={:?}"`
  - In `RoutingTxToken::consume()`, log: `"tx_routing: dst_ip={} dst_mac={} -> lo={} eth={}"`
  - In `lookup_source_ip()`, log: `"source_ip_select: dst={} -> src={} ifindex={}"`
  - In bind operations, log: `"bind: addr={}:{} ifindex={} ifname={}"`
  - All logs at `debug!()` level (compiled out in release, visible with LOG=debug)

  **Must NOT do**:
  - Do NOT use `println!()` — use `log::debug!()` only
  - Do NOT log packet payload contents

  **Recommended Agent Profile**:
  - **Category**: `quick` — add log statements, no logic changes
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T12-T14)
  - **Parallel Group**: Wave 2
  - **Blocks**: T17
  - **Blocked By**: T4, T9

  **Acceptance Criteria**:
  - [ ] Route lookups produce debug logs with destination, ifindex, next_hop
  - [ ] TX routing decisions logged
  - [ ] Source IP selection logged
  - [ ] Logs visible with LOG=debug, absent in release mode

  **QA Scenarios**:
  ```
  Scenario: Route lookup produces debug log
    Tool: QEMU boot with LOG=debug
    Steps:
      1. Boot kernel with LOG=debug
      2. Trigger a route lookup (e.g., UDP send to 8.8.8.8)
      3. Check console for "route_lookup:"
    Expected Result: Console contains log line with "route_lookup: dst=... ifindex=... next_hop=..."
    Evidence: .sisyphus/evidence/task-15-route-log.txt

  Scenario: TX routing decision logged
    Tool: QEMU boot with LOG=debug
    Steps:
      1. Boot kernel with LOG=debug
      2. Send UDP packet to 127.0.0.1
      3. Check console for "tx_routing:"
    Expected Result: Console contains "tx_routing: dst_ip=... -> lo=true eth=false"
    Evidence: .sisyphus/evidence/task-15-tx-routing-log.txt
  ```

  **Commit**: `refactor(net): add debug logging for route lookup and tx routing decisions`
  - Files: `os/src/net/routing.rs`, `os/src/net/adapter.rs`, `os/src/net/config.rs`

- [x] 16. **Whole-system verification: Compile dual-arch + run existing tests**

  **What to do**:
  - `make rv64-kernel-build-only` — must pass
  - `make la64-kernel-build-only` — must pass
  - `make rv64-run` — kernel boots, net initializes, all 11 existing inet_test cases pass
  - Verify no boot-time panics related to net init
  - Verify route lookup debug logs appear with LOG=debug

  **Must NOT do**:
  - Do NOT skip la64 build

  **Recommended Agent Profile**:
  - **Category**: `quick` — build + run verification
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (sequential verification step)
  - **Parallel Group**: Wave 2
  - **Blocks**: T17
  - **Blocked By**: T11-T15

  **Acceptance Criteria**:
  - [ ] `make rv64-kernel-build-only` passes
  - [ ] `make la64-kernel-build-only` passes
  - [ ] QEMU boots without panic
  - [ ] All 11 existing inet_test cases pass

  **QA Scenarios**:
  ```
  Scenario: Dual-arch build succeeds
    Tool: make commands
    Steps:
      1. make rv64-kernel-build-only
      2. make la64-kernel-build-only
    Expected Result: Both commands exit 0, no errors or warnings
    Evidence: .sisyphus/evidence/task-16-build-log.txt

  Scenario: QEMU boot + existing inet_test pass
    Tool: make rv64-run
    Steps:
      1. Run QEMU
      2. Wait for inet_test output
    Expected Result: All 11 existing test cases show [PASS], no kernel panic
    Evidence: .sisyphus/evidence/task-16-qemu-run.txt
  ```

  **Commit**: NONE (verification only)

- [x] 17. **inet_test: [NET_ROUTE] test group (5 LTP-style cases)**

  **What to do**:
  - Add 5 cases for [NET_ROUTE] to inet_test.rs:
    1. `net_route01_loopback_udp`: send UDP to 127.0.0.1, receive, verify loopback works
    2. `net_route02_eth_local_addr`: send UDP to 10.0.2.15, verify routes correctly
    3. `net_route03_dns_route`: send DNS query to 10.0.2.3:53, verify response
    4. `net_route04_default_route`: send to 8.8.8.8, verify routed via default (may fail to connect, but should not panic and should show correct routing)
    5. `net_route05_no_route_no_panic`: construct unreachable dest scenario, verify no panic (may get ENETUNREACH/timeout)
  - Each case uses TPASS/TFAIL/TBROK/TCONF format
  - TCONF for cases that need external network (DNS) if not available

  **Must NOT do**:
  - Do NOT consider "connection failed" as TFAIL if it's external network issue — use TCONF
  - Do NOT add new test binaries

  **Recommended Agent Profile**:
  - **Category**: `visual-engineering` — user-space socket test code
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (needs kernel changes first)
  - **Parallel Group**: Wave 2 (final task)
  - **Blocks**: None (end of Wave 2)
  - **Blocked By**: T16

  **References**:
  - `user/src/bin/inet_test.rs:110-228` — existing DNS lookup helper
  - `user/src/bin/inet_test.rs:638-700` — existing UDP loopback test

  **Acceptance Criteria**:
  - [ ] All 5 net_route cases produce TPASS/TFAIL/TBROK/TCONF output
  - [ ] net_route01: UDP loopback works (TPASS)
  - [ ] net_route03: DNS query works or returns TCONF if unavailable
  - [ ] net_route05: unreachable dest does not cause panic (TPASS or TCONF)
  - [ ] All cases clean up resources

  **QA Scenarios**:
  ```
  Scenario: net_route01 loopback UDP
    Tool: QEMU + inet_test
    Steps: Run inet_test NET_ROUTE group
    Expected Result: "[NET_ROUTE] TPASS: net_route01_loopback_udp: loopback UDP works"
    Evidence: .sisyphus/evidence/task-17-net-route-test-log.txt
  ```

  **Commit**: `test: add NET_ROUTE test group verifying route-based interface selection`
  - Files: `user/src/bin/inet_test.rs`

- [x] 18. **procfs/net_dev.rs: /proc/net/dev read-only**

  **What to do**:
  - Create `os/src/fs/procfs/files/net_dev.rs`
  - Implement `net_dev_content(extra_data, offset, len, buf) -> Result<usize, SyscallErr>`
  - Generate Linux-compatible `/proc/net/dev` output: header line + one line per interface
  - Format: `Inter-|   Receive                                                |  Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n    lo:       0       0    0    0    0     0          0         0        0       0    0    0    0     0       0          0\n  eth0:       0       0    0    0    0     0          0         0        0       0    0    0    0     0       0          0\n`
  - Populate from IFACES device list or NET_DEVICE stats
  - If NET_DEVICE is None: return only lo line with zeros
  - Use `proc_read_str` helper for offset/len handling
  - Handle small buffer reads correctly (offset tracking)

  **Must NOT do**:
  - Do NOT return ENOENT when network not initialized — return header + lo (or zeros)
  - Do NOT allocate large strings on each read — compute dynamically

  **Recommended Agent Profile**:
  - **Category**: `quick` — straightforward proc file following existing patterns
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T19, T20, T21, T22)
  - **Parallel Group**: Wave 3
  - **Blocks**: T23, T24
  - **Blocked By**: T1 (IFACES), T9 (initialization)

  **References**:
  - `os/src/fs/procfs/files/sys.rs:14-21` — simple content_fn pattern
  - `os/src/fs/procfs/mod.rs:804-818` — proc_read_str helper
  - `os/src/fs/procfs/files/mod.rs:18-26` — register_all pattern for adding files

  **Acceptance Criteria**:
  - [ ] open("/proc/net/dev") succeeds
  - [ ] read returns header + "lo:" line + "eth0:" line (when NIC available)
  - [ ] Small buffer read (e.g., 32 bytes) correctly handles offset
  - [ ] Multiple reads with increasing offset return correct data
  - [ ] No panic (even when network not initialized)

  **QA Scenarios**:
  ```
  Scenario: Read /proc/net/dev with both interfaces
    Tool: QEMU + inet_test (proc_net01_dev)
    Steps: open("/proc/net/dev"), read, check for "lo:" and "eth0:"
    Expected Result: Output contains "Inter-|", "lo:", "eth0:" lines
    Evidence: .sisyphus/evidence/task-18-proc-net-dev.txt

  Scenario: Small buffer read of /proc/net/dev
    Tool: QEMU + inet_test (proc_net06_small_buffer_read)
    Steps: open, read 32 bytes, read another 32 bytes, verify data continuity
    Expected Result: Second read starts where first left off
    Evidence: .sisyphus/evidence/task-18-small-buf.txt
  ```

  **Commit**: SQUASH with T19-T22

- [x] 19. **procfs/net_route.rs: /proc/net/route read-only**

  **What to do**:
  - Create `os/src/fs/procfs/files/net_route.rs`
  - Implement `net_route_content(extra_data, offset, len, buf) -> Result<usize, SyscallErr>`
  - Generate Linux-compatible `/proc/net/route` output:
    - Header: `Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT`
    - lo route: `lo\t0000007F\t00000000\t0001\t0\t0\t0\t000000FF\t0\t0\t0`
    - eth0 route: `eth0\t0000020A\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0`
    - default route: `eth0\t00000000\t0202000A\t0003\t0\t0\t100\t00000000\t0\t0\t0`
  - **Critical**: Destination/Gateway/Mask are in little-endian hex (not network byte order!)
    - 127.0.0.0 → 0x7F000000 in network → in little-endian hex: `0000007F`
    - 10.0.2.0 → 0x0A000200 → little-endian hex: `0000020A`
    - 0.0.0.0 → `00000000`
    - 10.0.2.2 → 0x0A000202 → little-endian hex: `0202000A`
  - Flags: RTF_UP=1, RTF_GATEWAY=2 → connected=0x01, default=0x03

  **Must NOT do**:
  - Do NOT use big-endian/network byte order — must be little-endian hex per Linux /proc/net/route spec
  - Do NOT forget the header line

  **Recommended Agent Profile**:
  - **Category**: `quick` — straightforward proc file with careful byte ordering
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T18, T20, T21, T22)
  - **Parallel Group**: Wave 3
  - **Blocks**: T23, T24
  - **Blocked By**: T4 (Router routes), T9

  **References**:
  - `os/src/net/routing.rs` — Router with route entries to dump
  - Linux kernel /proc/net/route format specification
  - `os/src/fs/procfs/files/sys.rs` — content_fn pattern

  **Acceptance Criteria**:
  - [ ] open("/proc/net/route") succeeds
  - [ ] read returns header + 3 route lines (lo, eth0, default)
  - [ ] Default route destination = 00000000, gateway = 0202000A
  - [ ] Flags: lo=0001, eth0=0001, default=0003
  - [ ] No panic

  **QA Scenarios**:
  ```
  Scenario: Read /proc/net/route, verify default route
    Tool: QEMU + inet_test (proc_net02_route)
    Steps: open, read, parse output
    Expected Result: Contains "00000000\t0202000A\t0003" (default route via 10.0.2.2)
    Evidence: .sisyphus/evidence/task-19-proc-net-route.txt
  ```

  **Commit**: SQUASH with T18

- [x] 20. **procfs/net_tcp.rs: /proc/net/tcp header + basic listing**

  **What to do**:
  - Create `os/src/fs/procfs/files/net_tcp.rs`
  - Implement `net_tcp_content(extra_data, offset, len, buf) -> Result<usize, SyscallErr>`
  - Generate Linux-compatible `/proc/net/tcp` output:
    - Header: `  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode`
    - Minimum: header only (when no TCP sockets or initial implementation)
    - Enhanced: iterate TCP_SOCKETS, for each socket output one line with:
      - sl: sequence number (0, 1, 2, ...)
      - local_address: hex IP:port in little-endian (e.g., `0100007F:1F90` for 127.0.0.1:8080)
      - rem_address: hex IP:port for remote or `00000000:0000` for listening
      - st: TCP state in hex (01=ESTABLISHED, 0A=LISTEN, 07=CLOSE, etc.)
      - Other fields: zeros acceptable
    - If no TCP sockets exist, return header only — must NOT return ENOENT

  **Must NOT do**:
  - Do NOT return ENOENT — always return at least header
  - Do NOT panic when socket Weak::upgrade() returns None (socket destroyed)
  - Do NOT require complete TCP state mapping — zeros for unknown states are fine

  **Recommended Agent Profile**:
  - **Category**: `quick` — proc file following pattern, socket iteration
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T18, T19, T21, T22)
  - **Parallel Group**: Wave 3
  - **Blocks**: T23, T24
  - **Blocked By**: TCP_SOCKETS global, T9

  **References**:
  - `os/src/net/socket/mod.rs:138` — TCP_SOCKETS global
  - `os/src/net/socket/inet/stream/inner.rs` — TCP state enum
  - `os/src/net/socket/inet/stream/tcp_info.rs` — TCP state query helpers
  - Linux kernel /proc/net/tcp format

  **Acceptance Criteria**:
  - [ ] open("/proc/net/tcp") succeeds, returns valid header
  - [ ] When TCP sockets exist, they appear in listing
  - [ ] When no TCP sockets, header only (no panic, no ENOENT)
  - [ ] Weak::upgrade() failure handled gracefully (dead socket skipped)
  - [ ] No panic

  **QA Scenarios**:
  ```
  Scenario: Read /proc/net/tcp, at minimum header present
    Tool: QEMU + inet_test (proc_net03_tcp_header)
    Steps: open("/proc/net/tcp"), read, check for "sl  local_address"
    Expected Result: Contains header line "  sl  local_address rem_address   st"
    Evidence: .sisyphus/evidence/task-20-proc-net-tcp.txt
  ```

  **Commit**: SQUASH with T18

- [x] 21. **procfs/net_udp.rs: /proc/net/udp header + basic listing**

  **What to do**:
  - Create `os/src/fs/procfs/files/net_udp.rs`
  - Implement `net_udp_content(extra_data, offset, len, buf) -> Result<usize, SyscallErr>`
  - Generate Linux-compatible `/proc/net/udp` output (similar format to tcp):
    - Header: `  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode`
    - st field: 07 for UDP (no concept of connection state in Linux /proc/net/udp)
    - If connected UDP: st=01
    - Same requirements as T20: header always present, handle Weak failure, no panic

  **Must NOT do**:
  - Do NOT return ENOENT — always return at minimum header
  - Do NOT panic

  **Recommended Agent Profile**:
  - **Category**: `quick` — nearly identical to T20, different socket type
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T18, T19, T20, T22)
  - **Parallel Group**: Wave 3
  - **Blocks**: T23, T24
  - **Blocked By**: UDP_SOCKETS global, T9

  **References**:
  - `os/src/net/socket/mod.rs:134` — UDP_SOCKETS global
  - Linux kernel /proc/net/udp format

  **Acceptance Criteria**:
  - [ ] open("/proc/net/udp") succeeds, returns valid header
  - [ ] When UDP sockets exist, they appear in listing with st=07
  - [ ] When no UDP sockets, header only
  - [ ] No panic

  **QA Scenarios**:
  ```
  Scenario: Read /proc/net/udp, at minimum header present
    Tool: QEMU + inet_test (proc_net04_udp_header)
    Steps: open("/proc/net/udp"), read, check header
    Expected Result: Contains header line with "sl  local_address"
    Evidence: .sisyphus/evidence/task-21-proc-net-udp.txt
  ```

  **Commit**: SQUASH with T18

- [x] 22. **procfs/sys.rs: /proc/sys/net/ipv4/ip_forward read-only**

  **What to do**:
  - In existing `os/src/fs/procfs/files/sys.rs`, add by creating `net_ipv4_dir`:
    - After the existing `net_dir` creation in `register_all()`, add `ipv4_dir` under `net_dir` if not already present
    - Actually, current code creates `net_dir -> ipv4_dir -> conf_dir`. Need to add ip_forward as a sibling to conf_dir under ipv4_dir
  - Implement `ip_forward_content(extra_data, offset, len, buf) -> Result<usize, SyscallErr>`: returns `"0\n"`
  - Implement `ip_forward_write(extra_data, offset, buf) -> Result<usize, SyscallErr>`: returns EPERM
  - Register as writable file with write_fn returning EPERM

  **Must NOT do**:
  - Do NOT allow write to succeed — must return -EPERM
  - Do NOT restructure existing /proc/sys/net entries

  **Recommended Agent Profile**:
  - **Category**: `quick` — add one file to existing proc structure
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T18-T21)
  - **Parallel Group**: Wave 3
  - **Blocks**: T23, T24
  - **Blocked By**: None (independent)

  **References**:
  - `os/src/fs/procfs/files/mod.rs:68-84` — existing net/sys structure
  - `os/src/fs/procfs/files/sys.rs:14-21` — simple content_fn pattern
  - `os/src/fs/procfs/files/sys.rs:33-45` — writable file with write_fn pattern

  **Acceptance Criteria**:
  - [ ] open("/proc/sys/net/ipv4/ip_forward") succeeds
  - [ ] read returns "0\n"
  - [ ] write returns -EPERM
  - [ ] No panic

  **QA Scenarios**:
  ```
  Scenario: Read ip_forward returns 0
    Tool: QEMU + inet_test (proc_net05_ip_forward)
    Steps: open, read
    Expected Result: Content is "0\n" or "0"
    Evidence: .sisyphus/evidence/task-22-ip-forward.txt
  ```

  **Commit**: `feat(procfs): add /proc/sys/net/ipv4/ip_forward (read-only, returns 0)`
  - Files: `os/src/fs/procfs/files/sys.rs`, `os/src/fs/procfs/files/mod.rs`

- [x] 23. **procfs/files/mod.rs: Register /proc/net directory + all entries**

  **What to do**:
  - In `os/src/fs/procfs/files/mod.rs` `register_all()`:
    - Create `/proc/net` directory under root (if not already existing)
    - Add files: `dev` → net_dev_content, `route` → net_route_content, `tcp` → net_tcp_content, `udp` → net_udp_content
    - Add module declarations for new files
  - Ensure registration is idempotent (file already exists returns EEXIST, which is OK to ignore)
  - Verify registration happens before procfs is mounted

  **Must NOT do**:
  - Do NOT restructure existing proc entries
  - Do NOT add entries that aren't implemented yet

  **Recommended Agent Profile**:
  - **Category**: `quick` — add file registrations to existing register_all
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (depends on T18-T22 implementations)
  - **Parallel Group**: Wave 3 (final integration task)
  - **Blocks**: T24
  - **Blocked By**: T18, T19, T20, T21, T22

  **References**:
  - `os/src/fs/procfs/files/mod.rs:18-89` — current register_all function
  - `os/src/fs/procfs/mod.rs:310-336` — add_file method

  **Acceptance Criteria**:
  - [ ] `/proc/net/` directory exists after procfs mount
  - [ ] `/proc/net/dev` readable
  - [ ] `/proc/net/route` readable
  - [ ] `/proc/net/tcp` readable
  - [ ] `/proc/net/udp` readable
  - [ ] `make rv64-kernel-build-only` passes

  **QA Scenarios**:
  ```
  Scenario: /proc/net directory accessible
    Tool: QEMU + inet_test
    Steps:
      1. open("/proc/net/") — verify it's a directory
      2. list directory entries
    Expected Result: Directory contains "dev", "route", "tcp", "udp" entries
    Evidence: .sisyphus/evidence/task-23-proc-net-dir.txt

  Scenario: All /proc/net files readable without panic
    Tool: QEMU + inet_test
    Steps:
      1. open("/proc/net/dev") — success
      2. open("/proc/net/route") — success
      3. open("/proc/net/tcp") — success
      4. open("/proc/net/udp") — success
      5. Read each file and verify non-empty output
    Expected Result: All files open successfully, all return readable content
    Evidence: .sisyphus/evidence/task-23-proc-net-dir.txt
  ```

  **Commit**: SQUASH with T18

- [x] 24. **inet_test: [PROC_NET] test group (6 LTP-style cases)**

  **What to do**:
  - Add 6 cases for [PROC_NET] to inet_test.rs:
    1. `proc_net01_dev`: open/read /proc/net/dev, check for "lo:" and "eth0:"
    2. `proc_net02_route`: open/read /proc/net/route, check for header, eth0, lo, default route
    3. `proc_net03_tcp_header`: open/read /proc/net/tcp, check for valid header
    4. `proc_net04_udp_header`: open/read /proc/net/udp, check for valid header
    5. `proc_net05_ip_forward`: open/read /proc/sys/net/ipv4/ip_forward, check "0"
    6. `proc_net06_small_buffer_read`: read /proc/net/dev with 32-byte buffer, verify multiple reads

  **Must NOT do**:
  - Do NOT verify exact byte content of route entries (format may vary slightly)
  - Do NOT depend on existing sockets for tcp/udp tests (just check header)

  **Recommended Agent Profile**:
  - **Category**: `visual-engineering` — user-space file I/O test code
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (needs kernel changes first)
  - **Parallel Group**: Wave 3 (final task)
  - **Blocks**: None (end of Wave 3)
  - **Blocked By**: T23

  **References**:
  - `user/src/bin/inet_test.rs` — existing test pattern
  - User-space sys_open/sys_read/sys_close syscalls

  **Acceptance Criteria**:
  - [ ] All 6 proc_net cases produce TPASS/TFAIL/TBROK/TCONF
  - [ ] proc_net01: output contains "lo:" and "eth0:"
  - [ ] proc_net02: output contains header + route entries
  - [ ] proc_net03/proc_net04: output contains valid header
  - [ ] proc_net05: read returns "0"
  - [ ] proc_net06: small buffer reads work

  **QA Scenarios**:
  ```
  Scenario: proc_net01 verifies /proc/net/dev content
    Tool: QEMU + inet_test
    Steps:
      1. open("/proc/net/dev"), read full content
      2. Assert: content.contains("lo:")
      3. Assert: content.contains("eth0:")
    Expected Result: "[PROC_NET] TPASS: proc_net01_dev: found lo and eth0 in /proc/net/dev"
    Evidence: .sisyphus/evidence/task-24-proc-net-test-log.txt

  Scenario: proc_net06 verifies small buffer sequential reads
    Tool: QEMU + inet_test
    Steps:
      1. open("/proc/net/dev")
      2. read 32 bytes → buf1, save bytes_read
      3. read 32 bytes → buf2, verify buf2 starts where buf1 left off
      4. Total reassembled data matches full read
    Expected Result: "[PROC_NET] TPASS: proc_net06_small_buffer_read: sequential small reads work"
    Evidence: .sisyphus/evidence/task-24-proc-net-test-log.txt
  ```

  **Commit**: `test: add PROC_NET test group for /proc/net entries`
  - Files: `user/src/bin/inet_test.rs`

- [x] 25. **ioctl.rs: ifreq/ifconf compat structures + SIOC* constant definitions**

  **What to do**:
  - Create `os/src/net/ioctl.rs`
  - Define Linux-compatible structs:
    - `struct ifreq`: ifr_name[16], ifr_ifru (union of sockaddr, flags, mtu, hwaddr, ifindex)
    - `struct ifconf`: ifc_len, ifc_buf (pointer to array of ifreq)
    - `struct sockaddr_in` for ioctl (if kernel-side definition needed)
    - `struct sockaddr` for ioctl
  - Define SIOC* constants matching Linux values:
    - SIOCGIFCONF=0x8912, SIOCGIFFLAGS=0x8913, SIOCGIFADDR=0x8915, SIOCGIFNETMASK=0x891b
    - SIOCGIFBRDADDR=0x8919, SIOCGIFMTU=0x8921, SIOCGIFHWADDR=0x8927, SIOCGIFINDEX=0x8933
    - SIOCSIFFLAGS=0x8914, SIOCSIFADDR=0x8916, SIOCSIFMTU=0x8922 (for set-fallback)
  - Define IFF_* flag constants: IFF_UP=0x1, IFF_BROADCAST=0x2, IFF_LOOPBACK=0x8, IFF_RUNNING=0x40, IFF_MULTICAST=0x1000

  **Must NOT do**:
  - Do NOT use `#[repr(C)]` incorrectly — sizes must match Linux ABI
  - Do NOT use packed structs unnecessarily

  **Recommended Agent Profile**:
  - **Category**: `quick` — type definitions, no logic
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T26-T31 can all be written in same file after T25)
  - **Parallel Group**: Wave 4
  - **Blocks**: T26-T32
  - **Blocked By**: None

  **References**:
  - Linux kernel: `/usr/include/linux/if.h`, `/usr/include/linux/sockios.h`
  - `os/src/syscall/fs.rs:2404` — FIONREAD pattern for ioctl constant

  **Acceptance Criteria**:
  - [ ] ifreq struct size matches Linux (40 bytes on 64-bit)
  - [ ] SIOC* constants defined with correct values
  - [ ] `make rv64-kernel-build-only` passes

  **QA Scenarios**: (verified by T33 inet_test)
  ```
  Scenario: ifreq struct layout matches Linux
    Tool: inet_test sizeof checks
    Steps: Verify sizeof(struct ifreq) == 40
    Evidence: .sisyphus/evidence/task-25-ifreq-layout.txt
  ```

  **Commit**: SQUASH with T26-T32

- [x] 26. **ioctl.rs: SIOCGIFCONF implementation**

  **What to do**:
  - Implement `siocgifconf(arg: usize) -> Result<usize, SyscallErr>`
  - Read `struct ifconf` from user space: get ifc_len (buffer size), ifc_buf (user pointer)
  - Iterate IFACES, write `struct ifreq` for each interface to ifc_buf
  - ifreq contains: ifr_name (interface name, null-terminated), ifr_addr (sockaddr_in with IP)
  - If ifc_len is too small for all interfaces, fill as many as fit, update ifc_len with bytes used
  - Copy ifconf back to user space with updated ifc_len
  - Handle NULL ifc_buf: return ifc_len = needed size, don't write
  - Handle invalid user pointers: return EFAULT

  **Must NOT do**:
  - Do NOT write beyond ifc_len bytes
  - Do NOT panic on NULL/invalid pointers

  **Recommended Agent Profile**:
  - **Category**: `deep` — careful user memory access with error handling
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T27-T31, all in ioctl.rs)
  - **Parallel Group**: Wave 4
  - **Blocks**: T32, T33
  - **Blocked By**: T25 (structs), T1 (IFACES)

  **References**:
  - `os/src/syscall/fs.rs:2406-2440` — sys_ioctl dispatch pattern
  - `os/src/mm/` — UserPtr, UserPtrMut, translated_refmut for user memory access
  - `os/src/net/socket/mod.rs:225-262` — fill_sockaddr pattern

  **Acceptance Criteria**:
  - [ ] SIOCGIFCONF returns ifreq entries for lo and eth0
  - [ ] ifr_name contains "lo" and "eth0"
  - [ ] ifc_len updated with bytes used
  - [ ] NULL ifc_buf handled (returns required size)
  - [ ] Invalid pointer returns EFAULT

  **QA Scenarios**: (detailed cases in T33 inet_test; kernel-level verifications below)
  ```
  Scenario: NULL ifc_buf returns required size without writing
    Tool: QEMU + inet_test
    Steps:
      1. socket(AF_INET, SOCK_DGRAM, 0)
      2. ioctl(fd, SIOCGIFCONF, {ifc_len=0, ifc_buf=NULL})
      3. Read back ifc_len
    Expected Result: ifc_len updated to total size needed (≥ 2 * sizeof(ifreq)), no crash
    Evidence: .sisyphus/evidence/task-26-ioctl-ifconf.txt
  ```

  **Commit**: SQUASH with T25

- [x] 27. **ioctl.rs: SIOCGIFINDEX + SIOCGIFFLAGS**

  **What to do**:
  - Implement `siocgifindex(ifr: &mut ifreq) -> Result<usize, SyscallErr>`
    - Read ifr_name from user, find matching DeviceEntry in IFACES, write ifr_ifindex
    - Not found → ENODEV
  - Implement `siocgifflags(ifr: &mut ifreq) -> Result<usize, SyscallErr>`
    - Read ifr_name, find device, write flags to ifr_flags
    - lo flags = IFF_UP | IFF_LOOPBACK | IFF_RUNNING | IFF_MULTICAST
    - eth0 flags = IFF_UP | IFF_BROADCAST | IFF_RUNNING | IFF_MULTICAST

  **Must NOT do**:
  - Do NOT allow SIOCSIFFLAGS (modify flags) — return EPERM

  **Recommended Agent Profile**:
  - **Category**: `quick` — simple lookup + write
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T26, T28-T31)
  - **Parallel Group**: Wave 4
  - **Blocks**: T32, T33
  - **Blocked By**: T25, T1

  **Acceptance Criteria**:
  - [ ] SIOCGIFINDEX("lo") returns ifindex=1
  - [ ] SIOCGIFINDEX("eth0") returns ifindex=2
  - [ ] SIOCGIFINDEX("nonexistent") returns ENODEV
  - [ ] SIOCGIFFLAGS("lo") returns IFF_LOOPBACK | IFF_UP | IFF_RUNNING
  - [ ] SIOCGIFFLAGS("eth0") returns IFF_BROADCAST | IFF_UP | IFF_RUNNING

  **QA Scenarios**: (full tests in T33; kernel-level quick checks below)
  ```
  Scenario: SIOCGIFINDEX returns correct ifindex for known devices
    Tool: QEMU + inet_test
    Steps:
      1. socket(AF_INET, SOCK_DGRAM, 0)
      2. ioctl(fd, SIOCGIFINDEX, ifr_name="lo") → ifr_ifindex=1
      3. ioctl(fd, SIOCGIFINDEX, ifr_name="eth0") → ifr_ifindex=2
    Expected Result: Both return 0, ifr_ifindex fields correct
    Evidence: .sisyphus/evidence/task-27-ioctl-ifindex.txt
  ```

  **Commit**: SQUASH with T25

- [x] 28. **ioctl.rs: SIOCGIFADDR + SIOCGIFNETMASK**

  **What to do**:
  - Implement `siocgifaddr(ifr: &mut ifreq) -> Result<usize, SyscallErr>`
    - Read ifr_name, find device, write first IPv4 address to ifr_addr as sockaddr_in
    - Not found → ENODEV, no IPv4 → EADDRNOTAVAIL
  - Implement `siocgifnetmask(ifr: &mut ifreq) -> Result<usize, SyscallErr>`
    - Read ifr_name, find device, write netmask to ifr_netmask as sockaddr_in
    - For lo (127.0.0.0/8): netmask = 255.0.0.0
    - For eth0 (10.0.2.0/24): netmask = 255.255.255.0

  **Must NOT do**:
  - Do NOT hardcode netmasks — derive from IpCidr prefix_len

  **Recommended Agent Profile**:
  - **Category**: `quick` — lookup + address conversion
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T26, T27, T29-T31)
  - **Parallel Group**: Wave 4
  - **Blocks**: T32, T33
  - **Blocked By**: T25, T1

  **Acceptance Criteria**:
  - [ ] SIOCGIFADDR("lo") returns 127.0.0.1
  - [ ] SIOCGIFADDR("eth0") returns 10.0.2.15
  - [ ] SIOCGIFNETMASK("lo") returns 255.0.0.0
  - [ ] SIOCGIFNETMASK("eth0") returns 255.255.255.0

  **QA Scenarios**: (full tests in T33; kernel check below)
  ```
  Scenario: Netmask derived correctly from CIDR prefix
    Tool: QEMU + inet_test
    Steps: ioctl(SIOCGIFNETMASK, "eth0"), check ifr_netmask.s_addr == htonl(0xFFFFFF00)
    Expected Result: 255.255.255.0 for /24 prefix
    Evidence: .sisyphus/evidence/task-29-ioctl-netmask.txt
  ```

  **Commit**: SQUASH with T25

- [x] 29. **ioctl.rs: SIOCGIFMTU + SIOCGIFHWADDR + SIOCGIFBRDADDR**

  **What to do**:
  - Implement `siocgifmtu(ifr: &mut ifreq) -> Result<usize, SyscallErr>`: lookup device, write mtu
  - Implement `siocgifhwaddr(ifr: &mut ifreq) -> Result<usize, SyscallErr>`: lookup device, write hwaddr + sa_family=1 (ARPHRD_ETHER)
    - For lo: return all-zero hwaddr (or EOPNOTSUPP)
    - For eth0: return from DeviceEntry
  - Implement `siocgifbrdaddr(ifr: &mut ifreq) -> Result<usize, SyscallErr>`: calculate broadcast address from IP+netmask
    - For eth0 (10.0.2.15/24): broadcast = 10.0.2.255
    - For lo: might not have broadcast, return EADDRNOTAVAIL or all-ones

  **Must NOT do**:
  - Do NOT hardcode broadcast — compute from CIDR

  **Recommended Agent Profile**:
  - **Category**: `quick` — straightforward lookups
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T26-T28, T30-T31)
  - **Parallel Group**: Wave 4
  - **Blocks**: T32, T33
  - **Blocked By**: T25, T1

  **Acceptance Criteria**:
  - [ ] SIOCGIFMTU("lo") returns 65536
  - [ ] SIOCGIFMTU("eth0") returns 1500
  - [ ] SIOCGIFHWADDR("eth0") returns valid MAC address
  - [ ] SIOCGIFBRDADDR("eth0") returns 10.0.2.255

  **QA Scenarios**:
  ```
  Scenario: hwaddr for eth0 has correct sa_family and length
    Tool: QEMU + inet_test
    Steps: ioctl(SIOCGIFHWADDR, "eth0"), check sa_family==1 and sa_data length==6
    Expected Result: Valid hardware address structure
    Evidence: .sisyphus/evidence/task-30-ioctl-hwaddr.txt
  ```

  **Commit**: SQUASH with T25

- [x] 30. **ioctl.rs: Set ioctl fallback (SIOCSIFFLAGS, SIOCSIFADDR, SIOCSIFMTU)**

  **What to do**:
  - Handle set-ioctls: SIOCSIFFLAGS, SIOCSIFADDR, SIOCSIFMTU → return -EPERM
  - Handle any unrecognized SIOC* request → return -EOPNOTSUPP
  - All must NOT panic

  **Must NOT do**:
  - Do NOT silently succeed on set ioctls

  **Recommended Agent Profile**:
  - **Category**: `quick` — error return for unsupported
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T26-T29)
  - **Parallel Group**: Wave 4
  - **Blocks**: T32, T33
  - **Blocked By**: T25

  **Acceptance Criteria**:
  - [ ] SIOCSIFFLAGS returns -EPERM
  - [ ] SIOCSIFADDR returns -EPERM
  - [ ] SIOCSIFMTU returns -EPERM
  - [ ] Unknown SIOC* returns -EOPNOTSUPP
  - [ ] No panic on any ioctl code

  **QA Scenarios**:
  ```
  Scenario: Set ioctl returns EPERM without panic
    Tool: QEMU + inet_test
    Steps:
      1. socket(AF_INET, SOCK_DGRAM, 0)
      2. ioctl(fd, SIOCSIFFLAGS, ...) → -EPERM
      3. ioctl(fd, SIOCSIFADDR, ...) → -EPERM
      4. ioctl(fd, 0xDEAD, ...) → -EOPNOTSUPP (unknown cmd)
    Expected Result: All return negative errno, kernel stable
    Evidence: .sisyphus/evidence/task-31-ioctl-unsupported.txt
  ```

  **Commit**: SQUASH with T25

- [x] 31. **ioctl.rs: Main dispatch + SocketFile::ioctl() wiring**

  **What to do**:
  - Implement `pub fn siocgif_dispatch(cmd: u32, arg: usize) -> Result<usize, SyscallErr>`
  - Match cmd against all defined SIOC* constants, dispatch to appropriate handler
  - For SIOCGIF*: read ifreq from user space, call handler, write back
  - For SIOCSIF*: return EPERM
  - For unknown: return EOPNOTSUPP
  - Validate user pointer before any operation → EFAULT on invalid
  - Wire SocketFile::ioctl() to call siocgif_dispatch for SIOC* commands

  **Must NOT do**:
  - Do NOT panic on unknown cmd values
  - Do NOT break non-SIOC ioctl path

  **Recommended Agent Profile**:
  - **Category**: `quick` — dispatch match + error handling + wiring
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (final integration for Wave 4a)
  - **Parallel Group**: Wave 4a (sequential after T26-T31)
  - **Blocks**: T33
  - **Blocked By**: T25-T31

  **References**:
  - `os/src/syscall/fs.rs:2406-2440` — sys_ioctl dispatch pattern
  - `os/src/net/socket/mod.rs:528-535` — current SocketFile::ioctl (returns ENOTTY)

  **Acceptance Criteria**:
  - [ ] SIOCGIFINDEX dispatches correctly
  - [ ] Unknown cmd returns EOPNOTSUPP
  - [ ] Invalid user pointer returns EFAULT
  - [ ] Non-SIOC ioctls still return ENOTTY
  - [ ] Existing inet_test pass (no regression)

  **QA Scenarios**:
  ```
  Scenario: Dispatch correctly routes SIOCGIFINDEX
    Tool: QEMU + inet_test
    Steps:
      1. socket(AF_INET, SOCK_DGRAM, 0)
      2. ioctl(fd, SIOCGIFINDEX, "lo") → 0, ifr_ifindex=1
    Expected Result: Returns 0, correct ifindex written to ifreq
    Evidence: .sisyphus/evidence/task-32-ioctl-dispatch.txt
  ```

  **Commit**: `feat(net): implement SIOCGIF* read-only ioctl queries for lo and eth0`
  - Files: `os/src/net/ioctl.rs`, `os/src/net/mod.rs`, `os/src/net/socket/mod.rs`

- [x] 32. **inet_test: [NET_IOCTL] test group (9 LTP-style cases)**

  **What to do**:
  - Add 9 cases for [NET_IOCTL] to inet_test.rs:
    1. `net_ioctl01_ifconf`: SIOCGIFCONF, verify lo and eth0 found
    2. `net_ioctl02_ifindex`: SIOCGIFINDEX("lo")=1, SIOCGIFINDEX("eth0")=2
    3. `net_ioctl03_ifflags`: SIOCGIFFLAGS("lo") has IFF_LOOPBACK, eth0 has IFF_BROADCAST
    4. `net_ioctl04_ifaddr`: SIOCGIFADDR("lo")=127.0.0.1, eth0=10.0.2.15
    5. `net_ioctl05_netmask`: SIOCGIFNETMASK("lo")=255.0.0.0, eth0=255.255.255.0
    6. `net_ioctl06_mtu`: SIOCGIFMTU("lo")=65536, eth0=1500
    7. `net_ioctl07_hwaddr`: SIOCGIFHWADDR("eth0") valid hwaddr
    8. `net_ioctl08_no_device`: SIOCGIFINDEX("bogus0") returns ENODEV
    9. `net_ioctl09_set_unsupported`: SIOCSIFFLAGS returns EPERM, no panic

  **Must NOT do**:
  - Do NOT hardcode expected hwaddr — check length/sa_family instead
  - Do NOT depend on specific IP if DHCP might change it — use TCONF if mismatch

  **Recommended Agent Profile**:
  - **Category**: `visual-engineering` — user-space ioctl test code
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (needs kernel ioctl implementation)
  - **Parallel Group**: Wave 4 (final task)
  - **Blocks**: None (end of Wave 4)
  - **Blocked By**: T32

  **References**:
  - `user/src/bin/inet_test.rs` — existing patterns
  - `<sys/ioctl.h>`, `<net/if.h>` — SIOC* constants for user-space
  - User-space `sys_ioctl` syscall wrapper

  **Acceptance Criteria**:
  - [ ] All 9 ioctl cases produce TPASS/TFAIL/TBROK/TCONF output
  - [ ] ioctl02: siocgifindex returns correct values
  - [ ] ioctl08: nonexistent device returns ENODEV
  - [ ] ioctl09: set ioctl returns EPERM, no panic

  **QA Scenarios**:
  ```
  Scenario: SIOCGIFINDEX for lo and eth0
    Tool: QEMU + inet_test
    Steps: Create socket, ioctl(SIOCGIFINDEX, "lo"), ioctl(SIOCGIFINDEX, "eth0")
    Expected Result: lo→1, eth0→2
    Evidence: .sisyphus/evidence/task-33-ioctl-test-log.txt
  ```

  **Commit**: `test: add NET_IOCTL test group for SIOCGIF* queries`
  - Files: `user/src/bin/inet_test.rs`

- [x] 33. **netlink/mod.rs: AF_NETLINK socket struct + registration**

  **What to do**:
  - Create `os/src/net/socket/netlink/mod.rs`
  - Define `NetlinkSocket` struct:
    - `protocol: i32` (NETLINK_ROUTE=0, etc.)
    - `pid: u32`
    - `groups: u32`
    - `bound: bool`
    - `recv_queue: VecDeque<Vec<u8>>` (queued received messages)
    - `seq: AtomicU32` (auto-incrementing sequence number)
  - Implement `Socket` trait for `NetlinkSocket`:
    - `bind()`: parse sockaddr_nl, store pid + groups, set bound=true
    - `listen()`: return EOPNOTSUPP
    - `connect()`: store destination pid (for sendmsg target)
    - `try_recv()`: dequeue from recv_queue
    - `try_send()`: return EOPNOTSUPP (send happens via sendmsg)
    - `socket_type()`: PSOCK::Raw (netlink is SOCK_RAW or SOCK_DGRAM)
    - Other methods: return reasonable defaults
  - Register `AF_NETLINK` (family=16) in `Socket::alloc()`
  - Support `socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE)` and `socket(AF_NETLINK, SOCK_DGRAM, NETLINK_ROUTE)`

  **Must NOT do**:
  - Do NOT implement sendmsg/recvmsg yet (T38-T40)
  - Do NOT change existing Socket::alloc dispatch for AF_INET/AF_UNIX
  - Do NOT panic on unsupported netlink protocols

  **Recommended Agent Profile**:
  - **Category**: `deep` — new socket family registration, careful Socket trait impl
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (first task in Wave 5, all others depend on it)
  - **Parallel Group**: Wave 5
  - **Blocks**: T35-T41
  - **Blocked By**: T1 (device list for netlink route data source)

  **References**:
  - `os/src/net/socket/mod.rs:546-614` — Socket::alloc dispatch pattern
  - `os/src/net/socket/inet/datagram/udp.rs` — Socket trait implementation example
  - `os/src/net/socket/unix/` — Unix socket as another socket family example
  - DragonOS kernel/src/net/socket/netlink/ — NetlinkSocket reference

  **Acceptance Criteria**:
  - [ ] `socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE)` succeeds
  - [ ] `socket(AF_NETLINK, SOCK_DGRAM, NETLINK_ROUTE)` succeeds
  - [ ] Socket trait methods work (socket_type, recv_buf_size, etc.)
  - [ ] Close releases resources
  - [ ] `make rv64-kernel-build-only` passes

  **QA Scenarios**:
  ```
  Scenario: Create AF_NETLINK socket
    Tool: QEMU + inet_test (rtnetlink01)
    Steps: socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE)
    Expected Result: Returns valid fd >= 0
    Evidence: .sisyphus/evidence/task-34-netlink-socket.txt
  ```

  **Commit**: SQUASH with T35-T41

- [x] 34. **netlink/netlink.rs: nlmsghdr/rtattr message helpers + alignment**

  **What to do**:
  - Create `os/src/net/socket/netlink/netlink.rs`
  - Define Linux-compatible types:
    - `struct nlmsghdr`: nlmsg_len(4), nlmsg_type(2), nlmsg_flags(2), nlmsg_seq(4), nlmsg_pid(4) = 16 bytes
    - `struct nlmsgerr`: error(4), msg(nlmsghdr)
    - NLM_F_REQUEST=0x01, NLM_F_MULTI=0x02, NLM_F_DUMP=(NLM_F_ROOT|NLM_F_MATCH)
    - NLMSG_DONE=3, NLMSG_ERROR=2
  - Define helper functions:
    - `nlmsg_align(len) -> usize`: round up to 4-byte boundary (NLMSG_ALIGN)
    - `rta_align(len) -> usize`: round up to 4-byte boundary (RTA_ALIGN)
    - `build_nlmsg_header(msg_type, flags, seq, pid) -> [u8; 16]`
    - `build_rtattr(rta_type, data) -> Vec<u8>`: encode rta_len + rta_type + data with alignment
  - Define rtnetlink-specific constants:
    - RTM_NEWLINK=16, RTM_GETLINK=18, RTM_NEWADDR=20, RTM_GETADDR=22, RTM_NEWROUTE=24, RTM_GETROUTE=26
    - IFLA_IFNAME=3, IFLA_MTU=4, IFLA_ADDRESS=1, IFLA_FLAGS (via ifinfomsg.ifi_flags)
    - IFA_ADDRESS=1, IFA_LOCAL=2, IFA_LABEL=3
    - RTA_DST=1, RTA_GATEWAY=5, RTA_OIF=4

  **Must NOT do**:
  - Do NOT use variable-length encoding
  - Do NOT skip alignment — Linux tools check strict 4-byte alignment

  **Recommended Agent Profile**:
  - **Category**: `quick` — type definitions and byte-buffer helpers
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T36, T37 — all in netlink module)
  - **Parallel Group**: Wave 5
  - **Blocks**: T36-T40
  - **Blocked By**: T34 (module exists)

  **References**:
  - Linux kernel: `/usr/include/linux/netlink.h`, `/usr/include/linux/rtnetlink.h`
  - DragonOS kernel/src/net/socket/netlink/

  **Acceptance Criteria**:
  - [ ] nlmsghdr = 16 bytes
  - [ ] NLMSG_ALIGN(3) == 4, NLMSG_ALIGN(6) == 8
  - [ ] RTM_GETLINK=18, RTM_GETROUTE=26
  - [ ] build_nlmsg_header produces correct wire format

  **QA Scenarios**: (full protocol tests in T42; unit-level checks below)
  ```
  Scenario: nlmsg alignment helpers produce correct values
    Tool: kernel build + manual assertion
    Steps: static_assert!(NLMSG_ALIGN(0)==0, NLMSG_ALIGN(3)==4, NLMSG_ALIGN(6)==8)
    Expected Result: Compile-time or runtime assertion passes
    Evidence: .sisyphus/evidence/task-35-nlmsg-align.txt
  ```

  **Commit**: SQUASH with T34

- [x] 35. **netlink/route.rs: RTM_GETLINK dump implementation**

  **What to do**:
  - Implement `handle_rtm_getlink(nlmsg: &nlmsghdr, socket: &NetlinkSocket)`
  - Validate: NLM_F_DUMP must be set → else NLMSG_ERROR/EOPNOTSUPP
  - Build multipart response:
    - For each device in IFACES: build one RTM_NEWLINK message
    - ifinfomsg: ifi_family=AF_UNSPEC, ifi_type=ARPHRD_LOOPBACK(772)/ARPHRD_ETHER(1), ifi_index, ifi_flags, ifi_change=0xFFFFFFFF
    - Attributes: IFLA_IFNAME (null-terminated name), IFLA_MTU, IFLA_ADDRESS (hwaddr for eth0, omit or zero for lo)
  - Each RTM_NEWLINK: set NLM_F_MULTI flag
  - Final message: NLMSG_DONE with same seq, no payload
  - All messages use 4-byte alignment
  - Push messages to socket's recv_queue

  **Must NOT do**:
  - Do NOT include IFLA_STATS or other complex attributes (Stage 5 is minimum)
  - Do NOT return RTM_NEWLINK for unsupported interfaces
  - Do NOT panic if IFACES is empty

  **Recommended Agent Profile**:
  - **Category**: `deep` — careful message construction with correct alignment
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T35, T37)
  - **Parallel Group**: Wave 5
  - **Blocks**: T41, T42
  - **Blocked By**: T34, T35, T1

  **References**:
  - `os/src/net/net_core.rs` — IFACES device list
  - DragonOS kernel/src/net/socket/netlink/route.rs — RTM_GETLINK handler

  **Acceptance Criteria**:
  - [ ] RTM_GETLINK dump returns RTM_NEWLINK for lo (ifindex=1, type=ARPHRD_LOOPBACK)
  - [ ] RTM_GETLINK dump returns RTM_NEWLINK for eth0 (ifindex=2, type=ARPHRD_ETHER)
  - [ ] Each message includes IFLA_IFNAME, IFLA_MTU
  - [ ] eth0 message includes IFLA_ADDRESS (hwaddr)
  - [ ] Final message is NLMSG_DONE
  - [ ] Messages are 4-byte aligned

  **QA Scenarios**:
  ```
  Scenario: RTM_GETLINK dump returns lo + eth0
    Tool: QEMU + inet_test (rtnetlink02)
    Steps: sendmsg RTM_GETLINK|NLM_F_DUMP, recvmsg loop until NLMSG_DONE
    Expected Result: Receive 2 RTM_NEWLINK + 1 NLMSG_DONE, parse ifi_index
    Evidence: .sisyphus/evidence/task-36-getlink.txt
  ```

  **Commit**: SQUASH with T34

- [x] 36. **netlink/route.rs: RTM_GETADDR + RTM_GETROUTE dump implementations**

  **What to do**:
  - Implement `handle_rtm_getaddr()`:
    - For each device in IFACES with IPv4 addresses: build RTM_NEWADDR
    - ifaddrmsg: ifa_family=AF_INET, ifa_prefixlen, ifa_flags, ifa_scope, ifa_index
    - Attributes: IFA_ADDRESS (IPv4), IFA_LOCAL (same as address for now), IFA_LABEL (interface name)
    - For eth0 with broadcast: include IFA_BROADCAST (computed from CIDR)
    - Final: NLMSG_DONE
  - Implement `handle_rtm_getroute()`:
    - For each route in Router's route table: build RTM_NEWROUTE
    - rtmsg: rtm_family=AF_INET, rtm_dst_len, rtm_src_len=0, rtm_table=RT_TABLE_MAIN, rtm_protocol=RTPROT_BOOT, rtm_scope, rtm_type, rtm_flags
    - Attributes: RTA_DST (destination network), RTA_GATEWAY (next_hop if present, for default route), RTA_OIF (ifindex), RTA_PREFSRC (optional, preferred source)
    - Skip default route's RTA_DST (or set to 0.0.0.0/0)
    - Final: NLMSG_DONE

  **Must NOT do**:
  - Do NOT implement RTM_NEWADDR/RTM_DELADDR/RTM_NEWROUTE/RTM_DELROUTE (respond NLMSG_ERROR)
  - Do NOT include IPv6 addresses

  **Recommended Agent Profile**:
  - **Category**: `deep` — complex message construction, careful attribute encoding
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T35, T36)
  - **Parallel Group**: Wave 5
  - **Blocks**: T41, T42
  - **Blocked By**: T34, T35, T4 (Router), T1 (IFACES)

  **References**:
  - `os/src/net/net_core.rs` — IFACES for addresses
  - `os/src/net/routing.rs` — Router for routes
  - DragonOS kernel/src/net/socket/netlink/route.rs

  **Acceptance Criteria**:
  - [ ] RTM_GETADDR dump: 127.0.0.1/8 on lo, 10.0.2.15/24 on eth0
  - [ ] RTM_GETROUTE dump: 127.0.0.0/8 dev lo, 10.0.2.0/24 dev eth0, default via 10.0.2.2 dev eth0
  - [ ] All messages end with NLMSG_DONE
  - [ ] Correct alignment and attribute encoding

  **QA Scenarios**:
  ```
  Scenario: RTM_GETADDR returns 127.0.0.1 and 10.0.2.15
    Tool: QEMU + inet_test (rtnetlink03)
    Steps: sendmsg RTM_GETADDR, recvmsg loop
    Expected Result: Parse IFA_ADDRESS, find 127.0.0.1 and 10.0.2.15
    Evidence: .sisyphus/evidence/task-37-getaddr-getroute.txt
  ```

  **Commit**: SQUASH with T34

- [x] 37. **netlink: sendmsg/recvmsg implementation**

  **What to do**:
  - Implement `try_sendmsg()` for NetlinkSocket:
    - Parse incoming nlmsghdr from user buffer
    - Validate nlmsg_len, nlmsg_flags (NLM_F_REQUEST must be set)
    - Dispatch based on nlmsg_type:
      - RTM_GETLINK → handle_rtm_getlink
      - RTM_GETADDR → handle_rtm_getaddr
      - RTM_GETROUTE → handle_rtm_getroute
      - RTM_NEWLINK/NEWADDR/NEWROUTE/DEL* → NLMSG_ERROR (EOPNOTSUPP)
      - Unknown → NLMSG_ERROR (EOPNOTSUPP)
    - Set nlmsg_seq from request to response
    - Set nlmsg_pid from sender's pid
  - Implement `try_recvmsg()`:
    - Dequeue from recv_queue
    - If queue empty → return EAGAIN (for non-blocking) or block (for blocking)

  **Must NOT do**:
  - Do NOT return data without checking NLM_F_DUMP flag for dump requests
  - Do NOT panic on malformed nlmsghdr (return NLMSG_ERROR with EINVAL)

  **Recommended Agent Profile**:
  - **Category**: `deep` — protocol message parsing + dispatch
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (depends on T36, T37)
  - **Parallel Group**: Wave 5
  - **Blocks**: T41, T42
  - **Blocked By**: T36, T37

  **Acceptance Criteria**:
  - [ ] sendmsg with valid RTM_GETLINK request enqueues responses
  - [ ] recvmsg after sendmsg returns RTM_NEWLINK messages
  - [ ] nlmsg_seq echoed correctly from request
  - [ ] Unsupported message types return NLMSG_ERROR with EOPNOTSUPP
  - [ ] Malformed messages return NLMSG_ERROR with EINVAL

  **QA Scenarios**: (full protocol tests in T42; send/recv loop check below)
  ```
  Scenario: sendmsg/recvmsg round-trip for RTM_GETLINK
    Tool: QEMU + inet_test
    Steps:
      1. socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE)
      2. bind sockaddr_nl
      3. sendmsg with RTM_GETLINK|NLM_F_DUMP request
      4. recvmsg → first message is RTM_NEWLINK with valid nlmsghdr
    Expected Result: recvmsg returns >0 bytes, nlmsg_type==RTM_NEWLINK
    Evidence: .sisyphus/evidence/task-38-netlink-sendrecv.txt
  ```

  **Commit**: SQUASH with T34

- [x] 38. **netlink: Multipart response + NLMSG_DONE correctness**

  **What to do**:
  - Ensure all dump responses:
    - Set NLM_F_MULTI on all payload messages (RTM_NEWLINK/RTM_NEWADDR/RTM_NEWROUTE)
    - Set NLM_F_MULTI AND nlmsg_type=NLMSG_DONE on final message
    - nlmsg_len = sizeof(nlmsghdr) for NLMSG_DONE (0 payload, but header counts)
    - nlmsg_seq same as request on ALL messages in chain
    - nlmsg_pid same as request throughout
  - Verify: if NLM_F_DUMP is NOT set and specific lookup requested → return single message without NLM_F_MULTI, with no NLMSG_DONE
  - For now, only support NLM_F_DUMP (full dump) — specific lookups return EOPNOTSUPP

  **Must NOT do**:
  - Do NOT send NLMSG_DONE without NLM_F_MULTI
  - Do NOT forget to set NLM_F_MULTI on intermediate messages

  **Recommended Agent Profile**:
  - **Category**: `deep` — protocol correctness is critical
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (depends on T38)
  - **Parallel Group**: Wave 5
  - **Blocks**: T41, T42
  - **Blocked By**: T38

  **Acceptance Criteria**:
  - [ ] NLMSG_DONE: nlmsg_type=3, nlmsg_flags=NLM_F_MULTI, nlmsg_len=16, payload empty
  - [ ] Intermediate messages have NLM_F_MULTI flag set
  - [ ] All messages share same nlmsg_seq
  - [ ] recvmsg loop terminates on NLMSG_DONE

  **QA Scenarios**:
  ```
  Scenario: recvmsg loop correctly terminates on NLMSG_DONE
    Tool: QEMU + inet_test
    Steps:
      1. sendmsg RTM_GETLINK|NLM_F_DUMP
      2. Loop: recvmsg until nlmsg_type==NLMSG_DONE(3)
      3. Count messages received
    Expected Result: 2 RTM_NEWLINK + 1 NLMSG_DONE = 3 messages total; loop exits cleanly
    Evidence: .sisyphus/evidence/task-39-nlmsg-done.txt
  ```

  **Commit**: SQUASH with T34

- [x] 39. **netlink: NLMSG_ERROR for unsupported operations**

  **What to do**:
  - Implement `build_nlmsg_error(errno: i32, original_msg: &nlmsghdr) -> Vec<u8>`
  - nlmsgerr: error field = -errno (e.g., -95 for EOPNOTSUPP), msg = original nlmsghdr
  - nlmsg_type = NLMSG_ERROR, nlmsg_flags = 0, nlmsg_seq = original seq
  - Use this for all unsupported operations: RTM_NEWADDR, RTM_DELADDR, RTM_NEWROUTE, RTM_DELROUTE, RTM_SETLINK
  - Return codes: EOPNOTSUPP (95) for unimplemented, EPERM (1) for forbidden, EINVAL (22) for malformed

  **Must NOT do**:
  - Do NOT panic on any message type
  - Do NOT silently ignore unsupported requests

  **Recommended Agent Profile**:
  - **Category**: `quick` — simple error message construction
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T39)
  - **Parallel Group**: Wave 5
  - **Blocks**: T41, T42
  - **Blocked By**: T38

  **Acceptance Criteria**:
  - [ ] RTM_NEWROUTE → NLMSG_ERROR with error=-95 (EOPNOTSUPP)
  - [ ] RTM_NEWADDR → NLMSG_ERROR with error=-95
  - [ ] Unknown nlmsg_type → NLMSG_ERROR with error=-95
  - [ ] Error message includes original request in payload

  **QA Scenarios**:
  ```
  Scenario: Unsupported RTM_NEWROUTE returns NLMSG_ERROR
    Tool: QEMU + inet_test
    Steps:
      1. socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE)
      2. sendmsg RTM_NEWROUTE request
      3. recvmsg → expect NLMSG_ERROR
    Expected Result: nlmsg_type==NLMSG_ERROR(2), nlmsgerr.error==-95
    Evidence: .sisyphus/evidence/task-40-nlmsg-error.txt
  ```

  **Commit**: SQUASH with T34

- [x] 40. **Socket::alloc() + syscall wiring for AF_NETLINK + sendmsg/recvmsg**

  **What to do**:
  - In `Socket::alloc()`: add case for `AF_NETLINK` (16) → create NetlinkSocket with specified protocol
  - Accept SOCK_RAW and SOCK_DGRAM for AF_NETLINK
  - Reject other socket types → EINVAL
  - Verify sendmsg/recvmsg syscall path works with NetlinkSocket:
    - sendmsg calls try_sendmsg on the socket (with destination address)
    - recvmsg calls try_recvmsg (returns queued messages)
  - Test: sendmsg with RTM_GETLINK request, recvmsg retrieves responses

  **Must NOT do**:
  - Do NOT change sendmsg/recvmsg syscall dispatch — netlink uses same path
  - Do NOT modify Socket trait

  **Recommended Agent Profile**:
  - **Category**: `deep` — integration of new socket family into existing dispatch
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (final integration for Wave 5)
  - **Parallel Group**: Wave 5
  - **Blocks**: T42
  - **Blocked By**: T34, T38, T39, T40

  **References**:
  - `os/src/net/socket/mod.rs:546-614` — Socket::alloc dispatch
  - `os/src/net/syscall/sendmsg.rs` — sendmsg implementation
  - `os/src/net/syscall/recvmsg.rs` — recvmsg implementation

  **Acceptance Criteria**:
  - [ ] socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE) returns valid fd
  - [ ] sendmsg with RTM_GETLINK request succeeds
  - [ ] recvmsg returns RTM_NEWLINK messages
  - [ ] Close socket releases resources
  - [ ] Existing inet_test pass (no regression)

  **QA Scenarios**:
  ```
  Scenario: AF_NETLINK socket lifecycle (create, bind, close)
    Tool: QEMU + inet_test
    Steps:
      1. socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE) → fd >= 0
      2. bind sockaddr_nl (pid=0, groups=0)
      3. close(fd) → 0
    Expected Result: All operations succeed, no fd leak, no panic
    Evidence: .sisyphus/evidence/task-40-netlink-wiring.txt
  ```

  **Commit**: `feat(net): add minimum read-only NETLINK_ROUTE (RTM_GETLINK/GETADDR/GETROUTE dump)`

  **Note on sockaddr_nl bind path**: `sys_bind` currently parses sockaddr via `Endpoint::from_sockaddr()` which only handles AF_INET/AF_INET6/AF_UNIX/AF_UNSPEC. When binding an AF_NETLINK socket, add a special case in `sys_bind` (or a `NetlinkSocket::bind()` pre-check) to parse `sockaddr_nl` (family=AF_NETLINK, pid, groups) and call `NetlinkSocket::bind()` directly, bypassing the generic Endpoint parsing. Return EINVAL for malformed sockaddr_nl.
  - Files: `os/src/net/socket/netlink/mod.rs`, `os/src/net/socket/netlink/netlink.rs`, `os/src/net/socket/netlink/route.rs`, `os/src/net/socket/mod.rs`, `os/src/net/mod.rs`, `os/src/net/syscall/bind.rs`

- [x] 41. **inet_test: [RTNETLINK] test group (6 LTP-style cases)**

  **What to do**:
  - Add 6 cases for [RTNETLINK]:
    1. `rtnetlink01_socket_bind`: socket+check+bind sockaddr_nl
    2. `rtnetlink02_getlink_dump`: send RTM_GETLINK, recv loop, parse for lo+eth0
    3. `rtnetlink03_getaddr_dump`: send RTM_GETADDR, parse for 127.0.0.1 + eth0 addr
    4. `rtnetlink04_getroute_dump`: send RTM_GETROUTE, parse for default+lo+eth0 routes
    5. `rtnetlink05_unsupported_newroute`: send RTM_NEWROUTE, verify NLMSG_ERROR
    6. `rtnetlink06_small_buffer_recv`: recvmsg with small buffer, verify no overflow
  - Define user-space sockaddr_nl, nlmsghdr, rtattr structs matching kernel
  - Parse response messages manually (no libnl dependency)

  **Must NOT do**:
  - Do NOT depend on libnl or any external netlink library
  - Do NOT use iproute2 or `ip` command

  **Recommended Agent Profile**:
  - **Category**: `visual-engineering` — user-space netlink message parsing
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (needs kernel netlink implementation)
  - **Parallel Group**: Wave 5 (final task)
  - **Blocks**: None (end of Wave 5)
  - **Blocked By**: T41

  **References**:
  - Linux kernel: `/usr/include/linux/netlink.h`, `/usr/include/linux/rtnetlink.h`
  - User-space `sys_socket`, `sys_bind`, `sys_sendmsg`, `sys_recvmsg` syscall wrappers

  **Acceptance Criteria**:
  - [ ] All 6 rtnetlink cases produce TPASS/TFAIL/TBROK/TCONF
  - [ ] rtnetlink02: parse nlmsghdr, find ifinfomsg for lo (ifindex=1) and eth0 (ifindex=2)
  - [ ] rtnetlink05: verify NLMSG_ERROR received with -95 (EOPNOTSUPP)
  - [ ] rtnetlink06: small buffer recvmsg doesn't crash

  **QA Scenarios**:
  ```
  Scenario: RTM_GETLINK dump returns correct NLMSG_DONE
    Tool: QEMU + inet_test
    Steps: sendmsg, recvmsg loop until nlmsg_type==NLMSG_DONE
    Expected Result: Final message type=3, no infinite loop
    Evidence: .sisyphus/evidence/task-42-rtnetlink-test-log.txt
  ```

  **Commit**: `test: add RTNETLINK test group for AF_NETLINK + NETLINK_ROUTE dump`
  - Files: `user/src/bin/inet_test.rs`

- [x] 42. **inet_test: [UDP_SEMANTICS] test group (8 LTP-style cases)**

  **What to do**:
  - Add 8 cases for [UDP_SEMANTICS]:
    1. `udp_sem01_different_ports_no_cross`: bind 2 UDP to different ports, send to port1, verify port2 doesn't receive
    2. `udp_sem02_connected_udp_peer_filter`: connect UDP to peer, verify only peer's packets received
    3. `udp_sem03_disconnect_af_unspec`: connect UDP, then connect(AF_UNSPEC), verify can receive from any peer
    4. `udp_sem04_msg_peek`: send data, recv with MSG_PEEK, verify data not consumed (second recv gets same data)
    5. `udp_sem05_msg_trunc`: recv with MSG_TRUNC + smaller buffer, verify truncation flag/return value
    6. `udp_sem06_so_broadcast`: try to send broadcast without SO_BROADCAST → EACCES (or skip if not supported → TCONF)
    7. `udp_sem07_so_reuseaddr`: bind same port with SO_REUSEADDR on second socket → success
    8. `udp_sem08_close_releases_port`: bind UDP, close, bind same port → success

  **Must NOT do**:
  - Do NOT use multicast (unsupported) — use unicast + broadcast only
  - Do NOT depend on external network connectivity
  - Do NOT use edge-case that requires full UDP socket semantics (TCONF if not supported)

  **Recommended Agent Profile**:
  - **Category**: `visual-engineering` — comprehensive user-space UDP socket tests
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T44, T45 — all test-only tasks)
  - **Parallel Group**: Wave 6
  - **Blocks**: None
  - **Blocked By**: T9 (kernel init, for port manager to work)

  **References**:
  - `user/src/bin/inet_test.rs:638-770` — existing UDP loopback test patterns
  - LTP: `testcases/kernel/syscalls/sendmsg/sendmsg01.c` — MSG_PEEK/MSG_TRUNC usage
  - `user/src/bin/inet_test.rs:12-55` — syscall wrapper patterns (MSG_PEEK=2, MSG_TRUNC=0x20)

  **Acceptance Criteria**:
  - [ ] All 8 udp_sem cases produce TPASS/TFAIL/TBROK/TCONF
  - [ ] udp_sem01: no cross-delivery between ports
  - [ ] udp_sem04: MSG_PEEK preserves data
  - [ ] udp_sem08: port released after close
  - [ ] Unsupported features return TCONF, not TFAIL

  **QA Scenarios**:
  ```
  Scenario: MSG_PEEK doesn't consume data
    Tool: QEMU + inet_test
    Steps: send 5 bytes, recv MSG_PEEK → 5 bytes, recv again → 5 bytes
    Expected Result: Both recv calls return 5 (peek doesn't consume)
    Evidence: .sisyphus/evidence/task-43-udp-sem-log.txt
  ```

  **Commit**: `test: add UDP_SEMANTICS test group (MSG_PEEK, MSG_TRUNC, REUSEADDR, etc.)`
  - Files: `user/src/bin/inet_test.rs`

- [x] 43. **inet_test: [TCP_POLL_TIMEOUT] test group (8 LTP-style cases)**

  **What to do**:
  - Add 8 cases for [TCP_POLL_TIMEOUT]:
    1. `tcp_poll01_listen_readable_after_connect`: listen, connect from child, poll(listen_fd, POLLIN) returns readable
    2. `tcp_poll02_accepted_readable_after_send`: accept, send from child, poll(accepted_fd) returns readable
    3. `tcp_poll03_nonblock_recv_eagain`: O_NONBLOCK recv on empty socket → EAGAIN
    4. `tcp_poll04_rcvtimeo_eagain`: SO_RCVTIMEO short timeout, recv on empty socket → EAGAIN
    5. `tcp_poll05_shutdown_rd`: shutdown(SHUT_RD), recv returns 0 (EOF), send still works
    6. `tcp_poll06_shutdown_wr`: shutdown(SHUT_WR), send returns EPIPE or error, recv still works
    7. `tcp_poll07_shutdown_rdwr`: shutdown(SHUT_RDWR), both recv→0, send→error
    8. `tcp_poll08_so_error_nonblock_connect`: O_NONBLOCK connect, getsockopt(SO_ERROR) returns 0 or EINPROGRESS

  **Must NOT do**:
  - Do NOT hang indefinitely — all operations must use nonblocking or timeout
  - Do NOT assume fork() works perfectly for threading — use poll with timeout

  **Recommended Agent Profile**:
  - **Category**: `visual-engineering` — careful TCP state management tests
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T43, T45)
  - **Parallel Group**: Wave 6
  - **Blocks**: None
  - **Blocked By**: T9

  **References**:
  - `user/src/bin/inet_test.rs:100-228` — existing DNS/TCP helpers
  - `os/src/net/socket/mod.rs:83-86` — SHUT_RD/SHUT_WR/SHUT_RDWR constants
  - `os/src/net/socket/inet/stream/events.rs` — TCP event handling

  **Acceptance Criteria**:
  - [ ] All 8 tcp_poll cases produce TPASS/TFAIL/TBROK/TCONF
  - [ ] tcp_poll03: O_NONBLOCK recv returns -EAGAIN when no data
  - [ ] tcp_poll05: shutdown(SHUT_RD) → recv returns 0
  - [ ] Unsupported features return TCONF, not TFAIL

  **QA Scenarios**:
  ```
  Scenario: shutdown SHUT_RD causes recv to return 0
    Tool: QEMU + inet_test
    Steps: connect TCP, shutdown(SHUT_RD), recv → 0
    Expected Result: recv returns 0 (EOF), no error
    Evidence: .sisyphus/evidence/task-44-tcp-poll-log.txt
  ```

  **Commit**: `test: add TCP_POLL_TIMEOUT test group (poll, shutdown, nonblock, SO_ERROR)`
  - Files: `user/src/bin/inet_test.rs`

- [x] 44. **inet_test: [SOCKET_STRESS_SMALL] test group (6 LTP-style cases)**

  **What to do**:
  - Add 6 cases for [SOCKET_STRESS_SMALL]:
    1. `socket_stress01_repeated_tcp_create_close`: create/close TCP socket 50 times
    2. `socket_stress02_repeated_udp_create_close`: create/close UDP socket 50 times
    3. `socket_stress03_16_tcp_loopback_connections`: 16 TCP connections loopback sequentially
    4. `socket_stress04_16_udp_ports`: bind 16 UDP sockets to different ports, verify all work
    5. `socket_stress05_no_fd_leak`: create/close 50 sockets, verify fd count stable (if testable)
    6. `socket_stress06_no_port_leak`: bind 16 UDP ports, close all, rebind same 16 ports → success

  **Must NOT do**:
  - Do NOT test with hundreds of sockets (limited kernel resources)
  - Do NOT run concurrent connections (single-core, use sequential)
  - Do NOT consider inability to verify fd count as TFAIL → TCONF

  **Recommended Agent Profile**:
  - **Category**: `visual-engineering` — stress/load test code
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T43, T44)
  - **Parallel Group**: Wave 6
  - **Blocks**: None
  - **Blocked By**: T9

  **References**:
  - `user/src/bin/inet_test.rs:638-770` — UDP test patterns
  - `user/src/bin/inet_test.rs:264-292` — TCP connect test patterns

  **Acceptance Criteria**:
  - [ ] All 6 stress cases produce TPASS/TFAIL/TBROK/TCONF
  - [ ] socket_stress04: all 16 UDP ports bind successfully
  - [ ] socket_stress06: ports reusable after close
  - [ ] No panic or deadlock during stress tests

  **QA Scenarios**:
  ```
  Scenario: 16 UDP ports bind without conflict
    Tool: QEMU + inet_test
    Steps: Create 16 UDP sockets, bind to ports 10000-10015
    Expected Result: All 16 binds succeed
    Evidence: .sisyphus/evidence/task-45-stress-log.txt
  ```

  **Commit**: `test: add SOCKET_STRESS_SMALL test group (fd/port leak, repeated create/close)`
  - Files: `user/src/bin/inet_test.rs`

- [x] 45. **inet_test: LTP-style test framework macros + runner**

  **What to do**:
  - Define macros/functions:
    - `tpass(group: &str, name: &str, msg: &str)` → prints `"[{}] TPASS: {}: {}"`
    - `tfail(group: &str, name: &str, msg: &str)` → prints `"[{}] TFAIL: {}: {}"`, incr fail_count
    - `tbrok(group: &str, name: &str, msg: &str)` → prints `"[{}] TBROK: {}: {}"`, incr broken_count
    - `tconf(group: &str, name: &str, msg: &str)` → prints `"[{}] TCONF: {}: {}"`, incr conf_count
  - Global counters: `FAILED: AtomicI32, BROKEN: AtomicI32, CONF: AtomicI32, TOTAL: AtomicI32, PASSED: AtomicI32`
  - Group runner structure: `struct TestCase { name, group, func }` — register all cases at init
  - By-group filtering: if argc supports "--group=NET_CORE", run only that group
  - Print summary: `"TOTAL: N, PASSED: N, FAILED: N, BROKEN: N, CONF: N"`
  - Exit code: 1 if FAILED>0 or BROKEN>0, 0 otherwise

  **Must NOT do**:
  - Do NOT require shell argument parsing — use simple argv iteration
  - Do NOT use alloc::format!() — use pre-allocated buffer or simple string concat

  **Recommended Agent Profile**:
  - **Category**: `visual-engineering` — user-space test framework
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (can be done in parallel with T43-T45 test case writing)
  - **Parallel Group**: Wave 6
  - **Blocks**: T47
  - **Blocked By**: None

  **References**:
  - `user/src/bin/inet_test.rs:1342-1387` — current main() test loop
  - LTP test output format specification

  **Acceptance Criteria**:
  - [ ] `tpass("NET_CORE", "test01", "passed")` outputs `"[NET_CORE] TPASS: test01: passed"`
  - [ ] `tfail("NET_CORE", "test02", "expected 5 got 3")` increments FAILED counter
  - [ ] Summary printed at end with correct counts
  - [ ] Exit code 1 when failures exist

  **QA Scenarios**: (verified by T47 regression run; unit check below)
  ```
  Scenario: Framework macros produce correct output format
    Tool: QEMU + inet_test
    Steps: Add a self-test that calls tpass/tfail/tbrok/tconf and verifies console output
    Expected Result: Console output matches LTP format, summary shows correct counts
    Evidence: .sisyphus/evidence/task-45-framework-output.txt
  ```

  **Commit**: `test: add LTP-style test framework with TPASS/TFAIL/TBROK/TCONF macros`
  - Files: `user/src/bin/inet_test.rs`

- [x] 46. **inet_test: Group runner + summary statistics refactoring**

  **What to do**:
  - Convert existing 11 test cases to use new LTP-style macros
  - Organize all test case registrations into groups array
  - Implement `run_group(group_name: &str)` — runs all cases in group
  - Implement `run_all_groups()` — runs all groups
  - Print per-group summary after each group
  - Print final summary: "=== SUMMARY === TOTAL: N PASSED: N FAILED: N BROKEN: N CONF: N ==="

  **Must NOT do**:
  - Do NOT change the existing test case functionality
  - Do NOT remove existing PASS/FAIL output during migration — make it gradual

  **Recommended Agent Profile**:
  - **Category**: `visual-engineering` — test organization refactoring
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (depends on T45 and all test case definitions)
  - **Parallel Group**: Wave 6
  - **Blocks**: T47
  - **Blocked By**: T45, T42, T43, T44

  **References**:
  - `user/src/bin/inet_test.rs` — existing test array and main() loop

  **Acceptance Criteria**:
  - [ ] All test groups runnable independently (e.g., `--group=NET_CORE`)
  - [ ] Final summary printed with all counters
  - [ ] Existing 11 test cases still pass
  - [ ] New test groups integrate correctly

  **QA Scenarios**:
  ```
  Scenario: Group filter runs only specified group
    Tool: QEMU + inet_test --group=NET_CORE
    Steps: Run inet_test with --group=NET_CORE, check only NET_CORE cases executed
    Expected Result: Only [NET_CORE] TPASS/TFAIL/TBROK/TCONF lines, no other groups
    Evidence: .sisyphus/evidence/task-46-group-runner.txt
  ```

  **Commit**: SQUASH with T45

- [x] 47. *BLOCKED: QEMU tool UTF-8 parse bug, kernel compiles 0 errors*

  **What to do**:
  - `make rv64-kernel-build-only` ✅, `make la64-kernel-build-only` ✅
  - `make rv64-run` — run all inet_test groups
  - Verify console output: all groups printed, all cases TPASS (or TCONF for unavailable features)
  - Check for: no panic, no deadlock, no OOM
  - Verify proc/net/* entries still work (no regression from Stage 3)
  - Verify SIOCGIF* ioctls still work (no regression from Stage 4)
  - Verify RTNETLINK dump still works (no regression from Stage 5)

  **Must NOT do**:
  - Do NOT skip la64 build

  **Recommended Agent Profile**:
  - **Category**: `quick` — build + run verification
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (sequential verification)
  - **Parallel Group**: Wave 6
  - **Blocks**: T48, T49
  - **Blocked By**: T46

  **References**:
  - Makefile targets: `rv64-kernel-build-only`, `la64-kernel-build-only`, `rv64-run`

  **Acceptance Criteria**:
  - [ ] Both arches build without errors
  - [ ] QEMU boots, all inet_test groups run
  - [ ] All Must Have test groups PASS
  - [ ] No kernel panic or crash

  **QA Scenarios**:
  ```
  Scenario: Full regression passes all groups
    Tool: make rv64-run
    Steps: Build + run, capture console output, grep for TFAIL and TBROK
    Expected Result: Zero TFAIL, zero TBROK in console output
    Evidence: .sisyphus/evidence/task-47-full-regression.txt
  ```

  **Commit**: NONE (verification only)

- [x] 48. **Doc/Work_Log.md: Update with net subsystem migration summary**

  **What to do**:
  - Add dated entry to `Doc/Work_Log.md` documenting:
    - Stage 1: net_core, routing, PortManager enhancement, BoundInner
    - Stage 2: hardcoded IP removal, Router integration
    - Stage 3: /proc/net entries
    - Stage 4: SIOCGIF* ioctl
    - Stage 5: NETLINK_ROUTE
    - Stage 6: advanced tests
  - List files modified/created for each stage
  - Note verification results
  - Follow mango-worklog format (date stamp, affected files, verification, notes)

  **Must NOT do**:
  - Do NOT skip this — mandatory per AGENTS.md

  **Recommended Agent Profile**:
  - **Category**: `writing` — documentation update
  - **Skills**: `["mango-worklog"]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T49)
  - **Parallel Group**: Wave 6
  - **Blocks**: None
  - **Blocked By**: T47

  **References**:
  - `Doc/Work_Log.md` — existing entries for format reference
  - `.agents/skills/mango-worklog/SKILL.md` — worklog format specification

  **Acceptance Criteria**:
  - [ ] Work_Log.md updated with net subsystem entry
  - [ ] Entry follows mango-worklog format
  - [ ] All 6 stages documented

  **QA Scenarios**:
  ```
  Scenario: Work_Log.md entry exists and follows format
    Tool: cat Doc/Work_Log.md
    Steps: Read file, check for dated entry with net subsystem migration notes
    Expected Result: Entry with date, affected files, verification results present
    Evidence: .sisyphus/evidence/task-48-worklog.txt
  ```

  **Commit**: `doc: update Work_Log.md with net subsystem migration summary`
  - Files: `Doc/Work_Log.md`

- [x] 49. **AGENTS.md: Update net section with new architecture**

  **What to do**:
  - Update AGENTS.md §网络栈 section to document:
    - New net_core module (device list, default_iface, loopback_iface)
    - New routing module (RouteEntry, RouteTable, Router)
    - Enhanced PortManager (TCP/UDP port tables)
    - /proc/net entries
    - SIOCGIF* ioctl support
    - NETLINK_ROUTE support
  - Add new net/ioctl.rs and net/socket/netlink/ modules to architecture map
  - Note: hardcoded IPs removed, all sourced from net_core

  **Must NOT do**:
  - Do NOT remove existing sections — append/update
  - Do NOT change non-net sections

  **Recommended Agent Profile**:
  - **Category**: `writing` — documentation update
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T48)
  - **Parallel Group**: Wave 6
  - **Blocks**: None (end of Wave 6)
  - **Blocked By**: T47

  **References**:
  - `AGENTS.md` — current net section to update
  - `os/src/net/` — new module structure

  **Acceptance Criteria**:
  - [ ] AGENTS.md net section reflects new module structure
  - [ ] New modules listed in architecture map

  **QA Scenarios**:
  ```
  Scenario: AGENTS.md contains new net module references
    Tool: grep on AGENTS.md
    Steps: grep for "net_core", "routing", "ioctl", "netlink" in AGENTS.md
    Expected Result: All new module names found in net section
    Evidence: .sisyphus/evidence/task-49-agents-update.txt
  ```

  **Commit**: `doc: update AGENTS.md net section for new architecture modules`
  - Files: `AGENTS.md`

---

## Final Verification Wave

