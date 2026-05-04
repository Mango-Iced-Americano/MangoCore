use crate::net::{Endpoint, Mutex, Socket};
use crate::net::syscall::common::MsgFlags;
use crate::{
    fs::{
        dev::pipe::{make_pipe, Pipe},
        file_trait::File,
    },
    utils::error::{SyscallErr, SyscallRet},
};
use alloc::sync::Arc;
use smoltcp::wire::IpEndpoint;
#[allow(unused)]
pub struct UnixSocket<const N: usize> {
    //file_meta: FileMeta,
    // read_end: Arc<Pipe<N>>,
    // write_end: Arc<Pipe<N>>,
    read_end: Arc<Pipe>,
    write_end: Arc<Pipe>,
}

impl<const N: usize> Socket for UnixSocket<N> {
    fn bind(&self, _endpoint: &Endpoint) -> SyscallRet {
        todo!();
    }

    fn listen(&self) -> crate::utils::error::SyscallRet {
        todo!();
    }

    fn connect(&self, _endpoint: &Endpoint) -> SyscallRet {
        todo!();
    }

    fn accept(&self, _sockfd: u32, _addr: usize, _addrlen: usize) -> SyscallRet {
        todo!();
    }

    fn socket_type(&self) -> crate::net::SocketType {
        todo!()
    }

    fn recv_buf_size(&self) -> usize {
        todo!()
    }

    fn send_buf_size(&self) -> usize {
        todo!()
    }

    fn set_recv_buf_size(&self, _size: usize) {
        todo!()
    }

    fn set_send_buf_size(&self, _size: usize) {
        todo!()
    }

    fn local_endpoint(&self) -> Option<Endpoint> {
        None
    }

    fn remote_endpoint(&self) -> Option<Endpoint> {
        None
    }

    fn shutdown(&self, how: u32) -> crate::utils::error::GeneralRet<()> {
        log::info!("[UnixSocket::shutdown] how {}", how);
        Ok(())
    }



    fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
        let n = self.read_end.read(None, buf) as isize;
        if n >= 0 {
            Ok(n)
        } else {
            Err(match n {
                x if x == -(SyscallErr::EAGAIN as isize) => SyscallErr::EAGAIN,
                _ => SyscallErr::EIO,
            })
        }
    }

    fn try_send(&self, buf: &[u8], _flags: MsgFlags) -> Result<isize, SyscallErr> {
        let n = self.write_end.write(None, buf) as isize;
        if n >= 0 {
            Ok(n)
        } else {
            Err(match n {
                x if x == -(SyscallErr::EAGAIN as isize) => SyscallErr::EAGAIN,
                _ => SyscallErr::EIO,
            })
        }
    }
}

impl<const N: usize> UnixSocket<N> {
    pub fn new(read_end: Arc<Pipe>, write_end: Arc<Pipe>) -> Self {
        Self {
            //file_meta: FileMeta::new(crate::fs::InodeMode::FileSOCK),
            // buf: Mutex::new(VecDeque::new()),
            read_end,
            write_end,
        }
    }
}

/// 创建一个 Unix socket 对（双向管道）。
/// 注意：调用者需要将返回的 UnixSocket 包装进 SocketFile 后再插入 fd_table。
pub fn make_unix_socket_pair<const N: usize>() -> (Arc<UnixSocket<N>>, Arc<UnixSocket<N>>) {
    let (read1, write1) = make_pipe();
    let (read2, write2) = make_pipe();
    let socket1 = Arc::new(UnixSocket::new(read1, write2));
    let socket2 = Arc::new(UnixSocket::new(read2, write1));
    (socket1, socket2)
}
