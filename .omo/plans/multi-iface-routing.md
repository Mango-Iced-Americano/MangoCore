# Multi-Interface Routing Architecture — 6-Phase Migration Plan

## TL;DR

> **Quick Summary**: Transform the single-smoltcp-stack architecture into a clean three-layer design (Protocol / Routing / Device) where protocol sockets are device-agnostic, routing decisions live in a central Router, and each network device owns its own smoltcp Interface + SocketSet. Reference DragonOS patterns but simplify for single-core bare-metal.
>
> **Deliverables**:
> - Three-layer architecture: Protocol Layer (sockets, device-agnostic) → Routing Layer (Router, FIB, source selection, local delivery) → Device Layer (per-device smoltcp stacks)
> - Opaque `RouteSocketHandle(usize)` — protocol layer never touches smoltcp `SocketHandle` or `Interface`
> - Per-device `DeviceStack { ifindex, iface: Interface, sockets: SocketSet, device }`
> - Route-layer local delivery for own-IP connections (like Linux RTN_LOCAL → loopback)
> - TCP lazy bind: bind() stores boxed socket, connect()/listen() attaches to target iface
> - UDP wildcard: one smoltcp socket per iface, route-based send selection
> - SelfConnected for same-socket self-connect only (not general local delivery)
>
> **Estimated Effort**: XL (6 phases, ~50 granular tasks)
> **Confidence**: High on TCP/UDP split; medium-high on local delivery (smoltcp ARP bypass needs QEMU verification)

---

## Architecture Design

### Three-Layer Boundary

```
┌─────────────────────────────────────────────┐
│ PROTOCOL LAYER                              │
│ Files: socket/*, socket/inet/stream/*,      │
│        socket/inet/datagram/*, raw/*,       │
│        socket/inet/common/*                 │
│                                             │
│ OWNS:                                       │
│  • POSIX socket trait, fd-facing behavior   │
│  • TCP state machine, UDP rx queue          │
│  • port allocation/conflict rules           │
│  • wait queues, epoll readiness             │
│  • endpoint parsing, errno semantics        │
│                                             │
│ MAY HOLD: RouteSocketHandle, Vec<Route...>  │
│ MUST NOT: SocketHandle, Interface, Device   │
│           RouteDecision, ifindex directly   │
├─────────────────────────────────────────────┤
│ ROUTING LAYER                               │
│ Files: routing.rs, config.rs (refactored)   │
│                                             │
│ OWNS:                                       │
│  • FIB/route table (Router)                 │
│  • RouteSocketHandle allocation             │
│  • handle → {ifindex, SocketHandle} mapping │
│  • source address selection                 │
│  • DeviceStack collection                   │
│  • TCP listener fanout across stacks        │
│  • UDP wildcard per-iface handles           │
│  • poll-all-stacks orchestration            │
│  • RouteKind::Local injection               │
├─────────────────────────────────────────────┤
│ DEVICE LAYER                                │
│ Files: adapter.rs, drivers/net/*            │
│                                             │
│ OWNS:                                       │
│  • SmoltcpDeviceAdapter, NullNetDevice      │
│  • physical transmit/receive                │
│  • loopback device primitive                │
│                                             │
│ MUST NOT: FIB lookup, local IP decision,    │
│           socket demux, port rules           │
└─────────────────────────────────────────────┘
```

### Core Abstraction: RouteSocketHandle

Protocol layer sees only `RouteSocketHandle(usize)` — an opaque token.
Routing layer owns the mapping:

```rust
// routing.rs
pub struct RouteSocketHandle(pub(crate) usize);

pub(crate) struct SocketBinding {
    pub ifindex: u32,
    pub handle: SocketHandle,        // smoltcp handle, valid only in its SocketSet
    pub proto: InetProtocol,         // Tcp, Udp, Raw
}
```

Protocol operations go through routing facade:

```rust
NET_INTERFACE.with_tcp_mut(route_handle, |sock| ...)
NET_INTERFACE.tcp_connect(route_handle, remote, local)
NET_INTERFACE.ensure_udp_send_handle(&handles, remote) -> RouteSocketHandle
```

Protocol code NEVER imports `smoltcp::iface::SocketHandle`, never accesses `Interface` or `SocketSet`.

### Smoltcp Constraint

smoltcp `SocketHandle` is only valid within its `SocketSet`. TCP `connect()` requires `Interface::context()`.
A connected TCP socket CANNOT be moved between SocketSets (retransmission, congestion control are bound to the stack).

This is why:
- TCP bind is lazy: socket is created but not added to any SocketSet until connect()/listen()
- UDP wildcard gets one socket per iface, not per-datagram rehome
- Connected TCP stays on one iface for its lifetime

---

## Design Decisions (Definitive)

### Decision 1: TCP Lazy Bind — YES

`bind()` stores `Box<tcp::Socket>` + `IpEndpoint` in `Init::Bound`, does NOT call `SocketSet::add()`.
`connect()` or `listen()` chooses the target iface and attaches the socket.

**Reasoning**:
- smoltcp `SocketSet::remove()` preserves socket state ("without changing its state")
- `tcp::Socket` initial state `Closed` + `tuple=None` is a safe intermediate state
- `bind(0.0.0.0)` cannot choose iface — the target is unknown until connect/sendto
- DragonOS's listen creates per-iface backlog sockets, which is lazy allocation

**Implementation**:
```
Init::Bound { socket: Box<tcp::Socket>, local: IpEndpoint }  // no RouteSocketHandle
connect() → route_output(remote) → attach to target iface → smoltcp connect
listen() → concrete bind attaches to one iface; wildcard binds to all ifaces
```

### Decision 2: UDP Wildcard — Per-Iface Sockets, NOT Rehome

Wildcard UDP gets one smoltcp socket per iface (`BTreeMap<ifindex, RouteSocketHandle>`).
`sendto()` selects the right iface socket via `route_output(remote)`. No remove-add dance.

**Reasoning**:
- Technically remove→add works, but causes handle churn, tx buffer hazards
- Per-iface sockets are simpler and more predictable
- Connected UDP stays single-iface; only wildcard fans out

**Implementation**:
```
UdpSocket { socket_handlers: Mutex<Vec<RouteSocketHandle>>, tx_handler: Mutex<Option<RouteSocketHandle>> }
sendto() → ensure_udp_send_handle(remote) → picks iface socket or creates one
recvfrom() → scans all iface sockets, returns earliest readable datagram
```

### Decision 3: Own-IP Connect — Route-Layer Local Delivery (Option D)

When connecting to any local IP (including own eth0 IP), the routing layer detects it and injects the packet locally. `SelfConnected` only for same-socket `connect(self_addr:self_port)`.

**Linux reference**: `ip_route_output_key_hash_rcu()` returns `RTN_LOCAL` → `dev_out = loopback_dev` → `loopback_xmit` → `__netif_rx` injects to RX path. Physical NIC never touched. Destination IP remains unchanged (10.0.2.15, not rewritten to 127.0.0.1).

**smoltcp limitation**: smoltcp's `route()` returns own address as next-hop, then `lookup_hardware_addr()` ARPs for own IP — nobody replies → packet stuck.

**Solution**:
```
route_output(dst) → check if dst belongs to any local iface
  → RouteKind::Local { dst_ifindex }
  → inject IP packet into target iface's RX/local-delivery queue
  → bypass smoltcp's ARP resolution
```

**SelfConnected**: Only when `connect(local_addr:local_port)` on the same fd — the Linux "self-connect" special case. Uses VecDeque buffer, not smoltcp.

---

## Phases & Tasks

### Phase 1: Routing Primitives (Zero Behavior Change)
**Goal**: Introduce RouteSocketHandle, SocketBinding, RouteDecision types. All new code unused. Existing behavior unchanged.
**Verify**: rv64 + la64 build, /proc/net/route output identical.

| # | Task | File |
|---|------|------|
| 1 | Add `RouteSocketHandle(usize)`, `InetProtocol` enum, `SocketBinding` struct to routing.rs | `net/routing.rs` |
| 2 | Add `RouteDecision { ifindex, source, next_hop, is_local }` + `RouteKind` enum | `net/routing.rs` |
| 3 | Add `lookup_route_owned() → RouteEntry` (avoids ref-from-temporary issue) | `net/routing.rs` |
| 4 | Add `route_output(dest) → Result<RouteDecision, SyscallErr>` — routing layer's sole public output API | `net/routing.rs` |
| 5 | Make `lookup_source_ip()` delegate to `route_output(dest).source` | `net/config.rs` |
| 6 | Make `route_check()` delegate to `route_output(dest).map(|_| ())` | `net/config.rs` |
| 7 | Remove duplicated reachability logic from route_check | `net/config.rs` |

### Phase 2: RouteSocketHandle Facade (Backed by Single Stack)
**Goal**: Add routing-owned binding table. Protocol code still uses old single SocketSet underneath, but through new facade. No behavior change.
**Verify**: Existing inet_test passes, TCP/UDP/RAW behavior byte-for-byte identical.

| # | Task | File |
|---|------|------|
| 8 | Import BTreeMap, RouteSocketHandle, SocketBinding, InetProtocol into config.rs | `net/config.rs` |
| 9 | Add `bindings: BTreeMap<RouteSocketHandle, SocketBinding>` + `next_socket_id: usize` to NetInterfaceInner | `net/config.rs` |
| 10 | Initialize bindings map and next_socket_id in NetInterfaceInner::new() | `net/config.rs` |
| 11 | Add `add_routed_socket(proto, socket) → RouteSocketHandle` — wraps old add_socket | `net/config.rs` |
| 12 | Add `tcp_routed_socket(handle, |sock|)` — resolves via binding table to old SocketSet | `net/config.rs` |
| 13 | Add `remove_routed(handle)` — removes via binding table lookup | `net/config.rs` |

### Phase 3: Protocol Layer Detach from smoltcp SocketHandle (★ Key Checkpoint)
**Goal**: Protocol code no longer imports `smoltcp::iface::SocketHandle`. All socket access goes through RouteSocketHandle. If this compiles, the protocol/routing boundary is physically established.
**Verify**: rv64 + la64 compile. Protocol code grep shows zero `use smoltcp::iface::SocketHandle`.

| # | Task | File |
|---|------|------|
| 14 | Change `BoundInner.socket_handle` from `SocketHandle` to `RouteSocketHandle` | `socket/inet/common/bound.rs` |
| 15 | Change `UDP_SOCKETS_TO_REMOVE` / `TCP_SOCKETS_TO_REMOVE` type to `Vec<RouteSocketHandle>` | `socket/mod.rs` |
| 16 | Change `with_tcp_mut(handle)` signature to `RouteSocketHandle`, body to `tcp_routed_socket` | `socket/inet/stream/inner.rs` |
| 17 | Change all TCP state handle fields: Init, Bound, Connecting, Listening, Established, SelfConnected → RouteSocketHandle | `socket/inet/stream/inner.rs` |
| 18 | Add `tcp_connect(handle, remote, local)` to config.rs — wraps smoltcp connect with Interface::context() | `net/config.rs` |
| 19 | TCP bind/connect/listen use `add_routed_socket` instead of raw `add_socket` | `socket/inet/stream/lifecycle.rs` |
| 20 | TCP connect uses `tcp_connect` helper (no direct Interface/SocketSet access) | `socket/inet/stream/lifecycle.rs` |
| 21 | Change `UdpSocket.socket_handler` from `SocketHandle` to `RouteSocketHandle` | `socket/inet/datagram/udp.rs` |
| 22 | UDP constructor uses `add_routed_socket` instead of raw `add_socket` | `socket/inet/datagram/udp.rs` |
| 23 | Replace all UDP `udp_socket(handle, ...)` calls with `udp_routed_socket(handle, ...)` | `socket/inet/datagram/udp.rs` |
| 24 | Change `RawSocket.socket_handler` from `SocketHandle` to `RouteSocketHandle` | `socket/inet/raw/raw.rs` |
| 25 | RAW constructor uses `add_routed_socket` instead of raw `add_socket` | `socket/inet/raw/raw.rs` |

### Phase 4: DeviceStack Wrapper (Still Single Physical Path)
**Goal**: Wrap the single stack in DeviceStack struct, introduce ifindex-based access. Still one physical device path underneath. Poll iterates over stacks.
**Verify**: inet_test passes, all socket operations unchanged.

| # | Task | File |
|---|------|------|
| 26 | Add `DeviceStack { ifindex, iface: Interface, sockets: SocketSet, device }` struct | `net/config.rs` |
| 27 | Replace `device/iface/sockets` three fields with `stacks: Vec<DeviceStack>` | `net/config.rs` |
| 28 | Add `stack_mut(ifindex) → Option<&mut DeviceStack>` helper | `net/config.rs` |
| 29 | Update all routed accessors to resolve: binding → ifindex → stack → socket | `net/config.rs` |
| 30 | Change `poll_once()` to iterate all stacks: each stack polled, UDP dispatched per-stack | `net/config.rs` |

### Phase 5: Split lo and eth0 (Real Per-Device smoltcp Stacks) ★ Highest Risk
**Goal**: lo and eth0 each have their own smoltcp Interface + SocketSet. RoutingDevice frame-switching logic removed. DHCP only runs on eth0.
**Verify**: DHCP works, 127.0.0.1 loopback works, external routing via eth0 works. inet_test exit_code=0.

| # | Task | File |
|---|------|------|
| 31 | Add `new_loopback_stack(now) → DeviceStack` — independent lo smoltcp stack, ifindex=1, 127.0.0.1/8 | `net/config.rs` |
| 32 | Add `new_eth_stack(eth, hw_addr, now) → DeviceStack` — independent eth0 smoltcp stack, ifindex=2 | `net/config.rs` |
| 33 | Extract DHCP probe into `run_dhcp_on_eth0(stack)` — DHCP only on eth0, never lo | `net/config.rs` |
| 34 | Build `stacks = [lo_stack]` or `[lo_stack, eth_stack]` in NetInterfaceInner::new() | `net/config.rs` |
| 35 | Change `dispatch_udp_packets()` signature to take `&mut SocketSet` (per-stack dispatch) | `socket/inet/datagram/udp.rs` |
| 36 | Remove `RoutingDevice` import from config.rs | `net/config.rs` |
| 37 | Delete RoutingDevice struct and RoutingTxToken frame-inspection logic | `net/adapter.rs` |

### Phase 6: Clean Semantics — Fanout, Local Delivery, Forwarding
**Goal**: Full multi-interface semantics: wildcard fanout, route-layer local delivery, cross-interface forwarding foundation.
**Verify**: TCP server on 0.0.0.0 accepts lo + eth0 connections. UDP wildcard receives from both interfaces.

| # | Task | File |
|---|------|------|
| 38 | Add `listener_ifindexes(addr) → Vec<u32>` — wildcard returns all up IPv4 stacks | `net/routing.rs` |
| 39 | TCP bind lazy: `Init::Bound` stores `socket: Box<tcp::Socket>`, NOT `RouteSocketHandle` | `socket/inet/stream/inner.rs` |
| 40 | TCP listen allocates per-iface backlog sockets for wildcard bind | `socket/inet/stream/lifecycle.rs` |
| 41 | TCP connect attaches boxed socket to target iface, then calls smoltcp connect | `socket/inet/stream/lifecycle.rs` |
| 42 | TCP connect failure: remove socket from SocketSet, restore boxed socket to Bound | `socket/inet/stream/lifecycle.rs` |
| 43 | UDP wildcard: `socket_handlers: Mutex<Vec<RouteSocketHandle>>` + `tx_handler: Mutex<Option<RouteSocketHandle>>` | `socket/inet/datagram/udp.rs` |
| 44 | UDP send: `ensure_udp_send_handle(remote)` selects iface socket or creates one | `socket/inet/datagram/udp.rs` |
| 45 | Route-layer local delivery: `RouteKind::Local { dst_ifindex }` injects packet into target iface RX queue | `net/routing.rs` + `net/config.rs` |
| 46 | Own-IP ARP bypass: prevent smoltcp from ARPing for own IP; use local delivery instead | `net/config.rs` |
| 47 | SelfConnected wiring: only when `connect(self_addr:self_port)` on same fd | `socket/inet/stream/lifecycle.rs` |
| 48 | RAW receive fanout across all stacks | `socket/inet/raw/raw.rs` |
| 49 | `/proc/net/route` reads from routing layer snapshot (not temporary Router::init_default) | `fs/procfs/files/net_route.rs` |
| 50 | Regression + dual-arch: rv64 + la64 build, QEMU with NIC and without NIC, all tests | CI |

---

## Verification Strategy

### Per-Phase Checkpoints

| Phase | Compile | QEMU Test |
|-------|---------|-----------|
| 1 | rv64 + la64 | /proc/net/route unchanged |
| 2 | rv64 + la64 | inet_test byte-for-byte identical |
| 3 | rv64 + la64 ★ | grep: zero `use smoltcp::iface::SocketHandle` in protocol files |
| 4 | rv64 + la64 | inet_test passes, poll works |
| 5 | rv64 + la64 | DHCP + loopback + external route |
| 6 | rv64 + la64 | wildcard fanout + local delivery + own-IP connect |

### Key Regression Tests

```
TCP own-eth0-IP: server bind(eth0_ip:p) listen; client connect(eth0_ip:p) → handshake + read/write
TCP self-connect: socket(); bind(eth0_ip:p); connect(eth0_ip:p) → SelfConnected, write→read
TCP wildcard: server bind(0.0.0.0:p) listen; client connect(127.0.0.1:p) + connect(eth0_ip:p) → both accepted
UDP wildcard: socket(); bind(0.0.0.0:p); sendto(127.0.0.1:p) + recvfrom on remote → both work
UDP own-eth0-IP: sendto(eth0_ip:p) on same host → local delivery, no NIC transmit
Closed port: connect(eth0_ip:closed_port) → ECONNREFUSED, no ARP hang
No NIC: loopback works, external connect/sendto → ENETUNREACH
```

---

## Risk Assessment

| Risk | Phase | Likelihood | Mitigation |
|------|-------|-----------|------------|
| Stray SocketHandle in protocol code after Phase 3 | 3 | Medium | Compile-time grep gate |
| smoltcp ARP hangs on own-IP send | 5-6 | High | Local delivery bypass must work; fallback: static neighbor entry |
| TCP wildcard fanout breaks existing listen accept | 6 | Medium | Test wildcard + concrete bind scenarios |
| UDP per-iface socket creation race | 6 | Low | All under NET_INTERFACE mutex, single-core |
| lo Medium::Ip incompatibility with smoltcp | 5 | Low | Fallback: fake Ethernet loopback device |

---

## References

- **DragonOS per-device SocketSet**: `IfaceCommon.sockets: Mutex<SocketSet>` — each device owns its SocketSet
- **DragonOS BoundInner**: `(SocketHandle, Arc<dyn Iface>)` — socket aware of its device
- **Linux RTN_LOCAL**: `ip_route_output_key_hash_rcu()` returns `dev_out = loopback_dev` for own IP
- **smoltcp socket_set.rs**: `remove()` preserves socket state; `add()` consumes owned socket
- **smoltcp tcp.rs**: `tcp::Socket::new()` initial state `Closed` + `tuple=None`
