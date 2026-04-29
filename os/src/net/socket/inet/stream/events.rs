//! TCP 事件更新 —— 根据 smoltcp socket 状态刷新 pollee
//!
//! 各变体（Connecting / Established / Listening / SelfConnected）的 `update_io_events`
//! 已在 `inner.rs` 中作为 struct 方法实现。
//! 此处仅提供 `Inner` 枚举的包装分发。

use core::sync::atomic::AtomicUsize;

use super::inner::Inner;

impl Inner {
    /// 统一的事件刷新入口：根据当前变体类型，调用对应的 update_io_events
    /// 各变体方法定义在 `inner.rs` 中各自的 `impl` 块里。
    pub fn update_io_events(&self, pollee: &AtomicUsize) {
        match self {
            Inner::Init(_) => {}
            Inner::Connecting(c) => {
                c.update_io_events(pollee);
            }
            Inner::Listening(l) => {
                l.update_io_events(pollee);
            }
            Inner::Established(e) => {
                e.update_io_events(pollee);
            }
            Inner::SelfConnected(sc) => {
                sc.update_io_events(pollee);
            }
            Inner::Closed(_) => {}
        }
    }
}
