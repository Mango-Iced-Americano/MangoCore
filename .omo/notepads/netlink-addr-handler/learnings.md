# Learnings: RTM_NEWADDR + RTM_DELADDR Implementation

## File Layout
- `route/addr.rs` — address handlers (RTM_NEWADDR, RTM_DELADDR)
- `route/mod.rs` — dispatch, get handlers, utility functions
- `route/link.rs` — link handlers (RTM_NEWLINK, RTM_DELLINK, RTM_SETLINK)
- `route/route.rs` — route handlers (RTM_NEWROUTE, RTM_DELROUTE)

## Netlink Error Handling
- Write operations return ACK via `build_nlmsg_error(errno, seq, pid, &orig_hdr)` pushed to `sock.recv_queue`
- Errors from handler functions propagate as `Err(SyscallErr::XXX)` and are caught by dispatch
- EOPNOTSUPP (95) is the catch-all for unhandled message types

## Interface Access Pattern
- `find_iface_by_index` pattern: get `Arc<NetNamespace>` → lock device_list → iterate → clone Arc → drop lock. Use for loop not iterator chains to avoid temporary borrow issues.
- `nic_id()` returns `usize`, ifaddrmsg.index is i32

## NLM_F Flags for RTM_NEWADDR
- `NLM_F_REPLACE` (0x100): delete existing + re-add
- `NLM_F_EXCL` (0x200): error if exists → EEXIST
- Neither: EEXIST (matches `ip addr add` behavior without replace)
