use super::{address::SocketAddrv4, config::NET_INTERFACE, Mutex, Socket, MAX_BUFFER_SIZE};
use crate::net::config::lookup_source_ip;
use crate::utils::random::RNG;
use crate::{
    fs::{file_trait::File, OpenFlags},
    net::address,
    task::wait_interruptible,
    utils::error::{GeneralRet, SyscallErr, SyscallRet},
};
use alloc::vec;
use log::info;
use smoltcp::{
    iface::SocketHandle,
    phy::PacketMeta,
    socket::{
        self,
        udp::{PacketMetadata, SendError, UdpMetadata},
    },
    wire::{IpEndpoint, IpListenEndpoint},
};

use crate::fs::directory_tree::DirectoryTreeNode;
use crate::fs::fat32::PageCache;
use crate::fs::Dirent;
use crate::fs::DiskInodeType;
use crate::fs::SeekWhence;
use crate::fs::Stat;
use crate::mm::UserBuffer;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::sync::Weak;
use alloc::vec::Vec;

pub struct UdpSocket {
    inner: Mutex<UdpSocketInner>,
    socket_handler: SocketHandle,
}

#[allow(unused)]
struct UdpSocketInner {
    remote_endpoint: Option<IpEndpoint>,
    recvbuf_size: usize,
    sendbuf_size: usize,
}

impl Socket for UdpSocket {
    fn bind(&self, addr: IpListenEndpoint) -> SyscallRet {
        log::info!("[Udp::bind] bind to {:?}", addr);
        NET_INTERFACE.poll();
        NET_INTERFACE.udp_socket(self.socket_handler, |socket| {
            socket.bind(addr).ok().ok_or(SyscallErr::EINVAL)
        })?;
        NET_INTERFACE.poll();
        Ok(0)
    }

    fn listen(&self) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn connect<'a>(&'a self, addr_buf: &'a [u8]) -> crate::utils::error::SyscallRet {
        let remote_endpoint = address::endpoint(addr_buf)?;
        log::info!("[Udp::connect] connect to {:?}", remote_endpoint);
        let mut inner = self.inner.lock();
        inner.remote_endpoint = Some(remote_endpoint);
        NET_INTERFACE.poll();
        NET_INTERFACE.udp_socket(self.socket_handler, |socket| {
            let local = socket.endpoint();
            info!("[Udp::connect] local: {:?}", local);
            if local.port == 0 {
                info!("[Udp::connect] don't have local");
                let src_ip = lookup_source_ip(remote_endpoint.addr);
                let port = (unsafe { RNG.positive_u32() } % 16384 + 49152) as u16;

                let endpoint = IpListenEndpoint {
                    addr: Some(src_ip),
                    port,
                };

                let ret = socket.bind(endpoint);
                if ret.is_err() {
                    match ret.err().unwrap() {
                        socket::udp::BindError::Unaddressable => {
                            info!("[Udp::bind] unaddr");
                            return Err(SyscallErr::EINVAL);
                        }
                        socket::udp::BindError::InvalidState => {
                            info!("[Udp::bind] invaild state");
                            return Err(SyscallErr::EINVAL);
                        }
                    }
                }
                log::info!("[Udp::bind] bind to {:?}", endpoint);
                Ok(())
            } else {
                Ok(())
            }
        })?;
        NET_INTERFACE.poll();
        Ok(0)
    }

    fn accept(
        &self,
        _sockfd: u32,
        _addr: usize,
        _addrlen: usize,
    ) -> crate::utils::error::SyscallRet {
        todo!();
    }

    fn socket_type(&self) -> super::SocketType {
        super::SocketType::SOCK_DGRAM
    }

    fn recv_buf_size(&self) -> usize {
        self.inner.lock().recvbuf_size
    }

    fn set_recv_buf_size(&self, size: usize) {
        self.inner.lock().recvbuf_size = size;
    }

    fn send_buf_size(&self) -> usize {
        self.inner.lock().sendbuf_size
    }

    fn set_send_buf_size(&self, size: usize) {
        self.inner.lock().sendbuf_size = size;
    }

    fn loacl_endpoint(&self) -> IpListenEndpoint {
        NET_INTERFACE.poll();
        let local = NET_INTERFACE.udp_socket(self.socket_handler, |socket| socket.endpoint());
        NET_INTERFACE.poll();
        local
    }

    fn remote_endpoint(&self) -> Option<IpEndpoint> {
        self.inner.lock().remote_endpoint
    }

    fn shutdown(&self, how: u32) -> GeneralRet<()> {
        log::info!("[UdpSocket::shutdown] how {}", how);
        Ok(())
    }

    fn set_nagle_enabled(&self, _enabled: bool) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn set_keep_alive(&self, _enabled: bool) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn send_to(&self, buf: &[u8], dest_addr: IpEndpoint) -> SyscallRet {
        todo!();
    }
}

impl UdpSocket {
    pub fn new() -> Self {
        let tx_buf = socket::udp::PacketBuffer::new(
            vec![PacketMetadata::EMPTY; 128],
            vec![0 as u8; MAX_BUFFER_SIZE],
        );
        let rx_buf = socket::udp::PacketBuffer::new(
            vec![PacketMetadata::EMPTY; 128],
            vec![0 as u8; MAX_BUFFER_SIZE],
        );
        let socket = socket::udp::Socket::new(rx_buf, tx_buf);
        let socket_handler = NET_INTERFACE.add_socket(socket);
        log::info!("[UdpSocket::new] new {}", socket_handler);
        NET_INTERFACE.poll();
        Self {
            inner: Mutex::new(UdpSocketInner {
                remote_endpoint: None,
                recvbuf_size: MAX_BUFFER_SIZE,
                sendbuf_size: MAX_BUFFER_SIZE,
            }),
            socket_handler,
        }
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        log::info!(
            "[UdpSocket::drop] drop socket {}, remoteep {:?}",
            self.socket_handler,
            self.inner.lock().remote_endpoint
        );
        NET_INTERFACE.udp_socket(self.socket_handler, |socket| {
            if socket.is_open() {
                socket.close();
            }
        });
        NET_INTERFACE.remove(self.socket_handler);
        NET_INTERFACE.poll();
    }
}
use crate::task::suspend_current_and_run_next;
impl File for UdpSocket {
    fn deep_clone(&self) -> Arc<dyn File> {
        todo!();
    }
    fn readable(&self) -> bool {
        true
    }
    fn writable(&self) -> bool {
        true
    }
    fn read(&self, _offset: Option<&mut usize>, buf: &mut [u8]) -> usize {
        match self._read(buf) {
            Ok(ret) => ret,
            Err(err) => err.as_errno_ret(),
        }
    }
    fn write(&self, _offset: Option<&mut usize>, buf: &[u8]) -> usize {
        NET_INTERFACE.poll();
        // const MAX_UDP_PAYLOAD: usize = 1472; // 1500 - 20(IP) - 8(UDP)
        // if buf.len() > MAX_UDP_PAYLOAD {
        //     log::error!(
        //         "[UdpSocket] packet too large: {} > {}",
        //         buf.len(),
        //         MAX_UDP_PAYLOAD
        //     );
        //     return (SyscallErr::EMSGSIZE).as_errno_ret(); //暂时先检查包大小，不然跑测试直接会卡死
        // }
        let ret = NET_INTERFACE.udp_socket(self.socket_handler, |socket| {
            if !socket.can_send() {
                log::info!("[UdpSendFuture::poll] cannot send yet");
                return (SyscallErr::EAGAIN).as_errno_ret();
            }
            log::info!("[UdpSendFuture::poll] start to send...");
            let remote = self.inner.lock().remote_endpoint;
            let meta = UdpMetadata {
                endpoint: remote.unwrap(),
                meta: PacketMeta::default(),
            };
            info!(
                "[UdpSendFuture::poll] {:?} -> {:?}",
                socket.endpoint(),
                remote
            );
            let len = buf.len();
            let ret = socket.send_slice(buf, meta);
            if let Some(err) = ret.err() {
                if err == SendError::Unaddressable {
                    return (SyscallErr::ENOTCONN).as_errno_ret();
                } else {
                    return (SyscallErr::ENOBUFS).as_errno_ret();
                }
            } else {
                log::debug!("[UdpSendFuture::poll] send {} bytes", len);
                return len;
            }
        });
        NET_INTERFACE.poll();
        ret
    }
    fn r_ready(&self) -> bool {
        NET_INTERFACE.poll();
        NET_INTERFACE.udp_socket(self.socket_handler, |socket| {
            socket.can_recv() // 只有真正有包了才返回 true
        })
    }
    fn w_ready(&self) -> bool {
        NET_INTERFACE.poll();
        NET_INTERFACE.udp_socket(self.socket_handler, |socket| socket.can_send())
    }
    fn read_user(&self, _offset: Option<usize>, buf: UserBuffer) -> usize {
        let mut buffers = buf.buffers;
        let buf = unsafe {
            core::slice::from_raw_parts_mut(buffers[0].as_mut_ptr() as *mut u8, buf.len as usize)
        };
        let ret = self._read(buf);
        match ret {
            Ok(s) => s,
            Err(err) => err.as_errno_ret(),
        }
    }
    fn write_user(&self, _offset: Option<usize>, buf: UserBuffer) -> usize {
        let mut buffers = buf.buffers;
        let buf = unsafe {
            core::slice::from_raw_parts_mut(buffers[0].as_mut_ptr() as *mut u8, buf.len as usize)
        };
        self.write(None, buf)
    }
    fn get_size(&self) -> usize {
        todo!();
    }
    fn get_stat(&self) -> Stat {
        todo!();
    }
    fn get_file_type(&self) -> DiskInodeType {
        todo!();
    }
    fn is_dir(&self) -> bool {
        todo!();
    }
    fn is_file(&self) -> bool {
        todo!();
    }
    fn info_dirtree_node(&self, _dirnode_ptr: Weak<DirectoryTreeNode>) {
        todo!();
    }
    fn get_dirtree_node(&self) -> Option<Arc<DirectoryTreeNode>> {
        todo!();
    }
    /// open
    fn open(&self, _flags: OpenFlags, _special_use: bool) -> Arc<dyn File> {
        todo!();
    }
    fn open_subfile(&self) -> Result<Vec<(String, Arc<dyn File>)>, isize> {
        todo!();
    }
    /// create
    fn create(&self, _name: &str, _file_type: DiskInodeType) -> Result<Arc<dyn File>, isize> {
        todo!();
    }
    fn link_child(&self, _name: &str, _child: &Self) -> Result<(), isize> {
        todo!();
    }
    /// delete(unlink)
    fn unlink(&self, _delete: bool) -> Result<(), isize> {
        todo!();
    }
    /// dirent
    fn get_dirent(&self, _count: usize) -> Vec<Dirent> {
        todo!();
    }
    /// offset
    fn get_offset(&self) -> usize {
        todo!();
    }
    fn lseek(&self, _offset: isize, _whence: SeekWhence) -> Result<usize, isize> {
        todo!();
    }
    /// size
    fn modify_size(&self, _diff: isize) -> Result<(), isize> {
        todo!();
    }
    fn truncate_size(&self, _new_size: usize) -> Result<(), isize> {
        todo!();
    }
    // time
    fn set_timestamp(&self, _ctime: Option<usize>, _atime: Option<usize>, _mtime: Option<usize>) {
        todo!();
    }
    /// cache
    fn get_single_cache(&self, _offset: usize) -> Result<Arc<Mutex<PageCache>>, ()> {
        todo!();
    }
    fn get_all_caches(&self) -> Result<Vec<Arc<Mutex<PageCache>>>, ()> {
        todo!();
    }
    /// memory related
    fn oom(&self) -> usize {
        todo!();
    }
    /// poll, select related
    fn hang_up(&self) -> bool {
        false
    }
    /// iotcl
    fn ioctl(&self, _cmd: u32, _argp: usize) -> isize {
        todo!();
    }
    /// fcntl
    fn fcntl(&self, _cmd: u32, _arg: u32) -> isize {
        todo!();
    }
}

impl UdpSocket {
    fn _read<'a>(&'a self, buf: &'a mut [u8]) -> GeneralRet<usize> {
        NET_INTERFACE.poll();
        let ret = NET_INTERFACE.udp_socket(self.socket_handler, |socket| {
            if !socket.can_recv() {
                // panic!();
                log::trace!("[UdpRecvFuture::poll] cannot recv yet");
                return Err(SyscallErr::EAGAIN);
            }
            log::info!("[UdpRecvFuture::poll] start to recv...");
            let (ret, meta) = socket.recv_slice(buf).ok().ok_or(SyscallErr::ENOTCONN)?;
            let remote = Some(meta.endpoint);
            info!(
                "[UdpRecvFuture::poll] {:?} <- {:?}",
                socket.endpoint(),
                remote
            );
            self.inner.lock().remote_endpoint = remote;
            log::debug!("[UdpRecvFuture::poll] recv {} bytes", ret);
            Ok(ret)
        });
        NET_INTERFACE.poll();
        match ret {
            Ok(result) => return GeneralRet::Ok(result),
            Err(err) => return GeneralRet::Err(err),
        }
    }
}
