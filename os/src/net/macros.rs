// 宏已废弃：impl_file_for_socket! 已由 SocketFile 统一取代。
// 所有 socket 类型（Tcp/Udp/Raw）不再各自实现 File，
// 而是通过 SocketFile { inner: Arc<dyn Socket> } 统一包装。
// 参见 net/mod.rs 中的 impl File for SocketFile。
