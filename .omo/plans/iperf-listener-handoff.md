# iperf PARALLEL_TCP Listener Handoff — 修正后实施计划

## Oracle 审查修正

1. **不分 passive/active**：handoff 只存 LISTEN 状态的 handle，非 LISTEN 直接走 TCP_SOCKETS_TO_REMOVE
2. **不需要新参数**：`Listening` 已有 `self.listen_addr`
3. **接管时处理新 listener 的 bound handle**：避免 SocketSet 泄漏
4. **锁顺序**：先 drain registry 释放锁，再调 with_tcp_mut()
5. **合并重复 entry**：一次性取出同端口所有 handoff
6. **backlog cap 统一**：当前代码 cap 到 8

## Phase 1 — 数据结构和注册表

文件: `os/src/net/socket/mod.rs` (放在 TCP_SOCKETS 旁边)

```rust
struct ListenerHandoff {
    listen_addr: IpListenEndpoint,
    handles: Vec<SocketHandle>,   // 仅 LISTEN 状态的 handle
    deadline: TimeSpec,
}
static TCP_LISTENER_HANDOFFS: Mutex<Vec<ListenerHandoff>> = ...;
const HANDOFF_TIMEOUT_MS: u64 = 500;

/// 取出所有匹配的 handoff handles，释放锁后返回
fn drain_handoff(addr: IpListenEndpoint) -> Vec<SocketHandle>;
/// 定期清理过期 handoff（在 poll_once 末尾调用）
pub fn cleanup_listener_handoffs();
```

## Phase 2 — Listening::close()

文件: `os/src/net/socket/inet/stream/inner.rs`

```rust
pub fn close(&self) {
    let mut passive = Vec::new();
    for &h in &self.handles {
        with_tcp_mut(h, |socket| {
            if socket.is_listening() {
                passive.push(h);
            } else {
                socket.abort();
                TCP_SOCKETS_TO_REMOVE.lock().push(h);
            }
        });
    }
    if !passive.is_empty() {
        TCP_LISTENER_HANDOFFS.lock().push(ListenerHandoff {
            listen_addr: self.listen_addr,
            handles: passive,
            deadline: TimeSpec::now() + TimeSpec::from_ms(HANDOFF_TIMEOUT_MS),
        });
    }
}
```

## Phase 3 — Inner::listen() 接管

文件: `os/src/net/socket/inet/stream/lifecycle.rs`

在创建 backlog handles 之前，先 drain handoff。如果接管到 handles，关闭新创建的 bound socket handle（避免泄漏），用接管的 handles 补足 backlog。

## Phase 4 — 定期清理

文件: `os/src/net/config.rs`

在 `poll_once()` 末尾（inner_handler 闭包结束后）调用 `cleanup_listener_handoffs()`。

## Phase 5 — 编译 + 测试

- rv64 编译
- iperf PARALLEL_TCP 关 LOG 测试
