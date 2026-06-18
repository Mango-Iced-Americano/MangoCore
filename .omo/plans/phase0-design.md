# Phase 0 Design Confirmation

> Generated: 2026-06-17 | From: code reading of os/src/net/*

## 1. Current Architecture Confirmed

### 1.1 Global Lock — `NET_INTERFACE.inner: Mutex<Option<NetInterfaceInner>>`
- All data-path ops (`tcp_routed_socket`, `udp_routed_socket`, `raw_routed_socket`, `tcp_connect`, `rebind_routed_udp`) lock this
- `poll_once` holds this across all stacks + smoltcp poll + UDP dispatch
- `add_socket`, `add_routed_socket`, `remove_routed` all lock this

### 1.2 RouteSocketHandle — Bare `usize`, No Generation
```rust
// routing.rs:10
pub struct RouteSocketHandle(pub(crate) usize);  // No generation
```

### 1.3 SocketBinding — Copy, No Lifecycle
```rust
// routing.rs:26-31
#[derive(Clone, Copy)]  // ← Copy! No lifecycle tracking
pub(crate) struct SocketBinding { ifindex, handle: SocketHandle, proto }
```

### 1.4 DeviceStack — Public Fields, No Lock
```rust
// config.rs:46-52 — all pub fields
pub struct DeviceStack<'a> {
    pub nic: Arc<dyn Iface>,
    pub device: IfaceDevice,
    pub iface: Interface,
    pub sockets: SocketSet<'a>,
}
```

### 1.5 Data Path — All Through Global Lock
```
sys_sendto: kernel_buf → socket.try_sendmsg → NET_INTERFACE.tcp_routed_socket (global lock)
sys_recvfrom: socket.try_recvmsg → NET_INTERFACE.udp_routed_socket (global lock)
tcp_send/recv in inner.rs: with_tcp_mut → NET_INTERFACE.tcp_routed_socket
```

### 1.6 UDP Dispatch — Inside Global poll Lock
```
poll_once (global inner lock held):
  → dispatch_udp_packets(&mut stack.sockets)
    → scan all UDP sockets in SocketSet
    → data.to_vec() for each packet
    → lock UDP_SOCKETS, lock each UdpSocket.inner
    → rx_queue.push_back((Vec<u8>, endpoint))
```

### 1.7 UserBuffer — Exists but unused in net
- `UserBufferReader`, `UserBufferWriter`, `UserIoVec`: exist in `mm/uaccess.rs`
- `syscall/fs.rs` uses them for direct user-memory read/write
- Net syscall layer uses `kernel_buf` + `copy_from/to_user_array` instead

## 2. Design Confirmation

### 2.1 RouteSocketHandle: Keep and Upgrade
**Keep**. Change from `pub(crate) usize` to `{ index: u32, generation: u32 }`.
Upgrade bindings from `BTreeMap<RouteSocketHandle, SocketBinding>` to `Vec<BindingSlot>` where:
```rust
struct BindingSlot {
    generation: u32,
    entry: Option<Arc<BindingEntry>>,
}
struct BindingEntry {
    id: RouteSocketHandle,
    proto: InetProtocol,
    target: Mutex<BindingTarget>,
}
enum BindingTarget {
    Live { stack: Arc<DeviceStack>, ifindex: u32, handle: SocketHandle },
    Closing,
    Closed,
}
```

### 2.2 DeviceStack: Add Per-Stack Lock, Encapsulate smoltcp
Change public fields to private:
```rust
pub struct DeviceStack<'a> {
    ifindex: u32,
    nic: Arc<dyn Iface>,
    state: Mutex<DeviceStackState<'a>>,
    need_poll: AtomicBool,
}
struct DeviceStackState<'a> {
    device: IfaceDevice,
    iface: Interface,
    sockets: SocketSet<'a>,
    owners: BTreeMap<SocketHandle, RouteSocketHandle>,
}
```

### 2.3 NetInterface: Split Global Lock
```rust
pub struct NetInterface<'a> {
    stacks: RwLock<Vec<Arc<DeviceStack<'a>>>>,    // for add/remove/iterate
    bindings: RwLock<Vec<BindingSlot>>,            // for lookup/add/remove
    next_index: AtomicU32,                         // atomic ID allocation
    next_generation: AtomicU32,                    // per-slot generation
}
```

### 2.4 Data Path: Short Lookup → Per-Stack Lock
```
with_tcp_mut(rh, f):
  1. resolve_entry(rh, Tcp) → lock bindings briefly, clone Arc<BindingEntry>, unlock
  2. lock entry.target → extract (stack, handle), validate BindingTarget::Live
  3. lock stack.state → validate owners[handle] == rh → get_mut smoltcp → f(socket)
  4. drop stack.state → drop entry.target → return
```

### 2.5 Poll: Collect Inside, Deliver Outside
```
poll_once:
  1. clone stacks list (brief stack-lock)
  2. for each stack:
     a. try_lock/lock stack.state
     b. iface.poll + drain UDP/TCP events → collect to local vecs
     c. drop stack.state
  3. deliver UDP packets (may lock UDP_SOCKETS, UdpSocket.inner)
  4. update TCP readiness (may lock TcpSocket.inner)
  5. notify waiters
```

### 2.6 UDP Rebind: Typed-Remove/Add Across SocketSets
```
rebind_routed_udp(rh, new_ifindex):
  1. resolve_entry + lookup new_stack
  2. lock entry.target (serialize I/O/close/rebind)
  3. lock old_stack.state + new_stack.state (by ifindex order)
  4. validate old owners[old_handle] == rh
  5. let udp_sock = old_state.sockets.remove::<UdpSocket>(old_handle)  // typed remove
  6. old_state.owners.remove(old_handle)
  7. let new_handle = new_state.sockets.add(udp_sock)
  8. new_state.owners.insert(new_handle, rh)
  9. update entry target → Live { new_stack, new_handle }
```

## 3. Key Risks Identified

1. **smoltcp SocketHandle portability**: SocketHandle is per-SocketSet index. Must typed-remove actual socket object.
2. **Reverse lock order**: Currently poll locks global inner → UDP_SOCKETS → UdpSocket.inner. After change: poll locks stack.state → must NOT lock UdpSocket.inner. Must collect events and deliver after releasing stack lock.
3. **CURRENT_POLL_IFINDEX**: Global static that assumes serial poll. Phase 1 keeps serial poll; Phase 4 addresses for concurrent poll.
4. **TcpSocket::Inner state machine**: Some states (Init::Unbound, Connecting) hold smoltcp socket objects not yet in SocketSet. Need careful handling during migration.

## 4. Files to Touch in Phase 1

| File | Change |
|------|--------|
| `routing.rs` | RouteSocketHandle → {index, generation}, add BindingEntry/BindingTarget/BindingSlot |
| `config.rs` | NetInterface split, DeviceStack encapsulate, poll rewrite, API rewrite |
| `inner.rs` (stream) | with_tcp_mut → new route API, accept → route API |
| `udp.rs` | try_send/try_recv → new route API, rebind rewrite, dispatch refactor |
| `io.rs`, `lifecycle.rs` (stream) | Adapt to new with_tcp_mut signature |
| `raw.rs` | with_raw_mut → new route API |
| `mod.rs` (socket) | wake_tcp_waiters → lock-outside-deliver |
| All syscall/*.rs | Adapt NET_INTERFACE.try_poll() calls if signature changes |

## 5. Decision

**Proceed with Phase 1 as designed, with Oracle-approved additions (BindingEntry lifecycle guard, owners validation, poll lock-outside-deliver).**
