// 根据 fd 拿 socket
#[macro_export]
macro_rules! get_socket {
    ($sockfd:expr) => {{
        let task = crate::task::current_task().unwrap();
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        let file = match fd_table.get_file($sockfd as usize) {
            Err(e) => return -(e as isize),
            Ok(f) => {
                // O_PATH 打开的 fd 视为 inoperable，应返回 EBADF
                if f.flags().contains(crate::fs::vfs::FileFlags::O_PATH) {
                    return -(crate::utils::error::SyscallErr::EBADF as isize);
                }
                f
            }
        };
        // downcast IndexNode → SocketFile → 取 .inner 拿到 Arc<dyn Socket>
        let any_ref = file.inode.as_any_ref();
        match any_ref.downcast_ref::<crate::net::SocketFile>() {
            Some(socket_file) => socket_file.inner.clone(),
            None => return crate::syscall::errno::ENOTSOCK,
        }
    }};
}
