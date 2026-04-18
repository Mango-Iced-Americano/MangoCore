#![allow(unused)]

use super::{Mutex, Socket};
use crate::{
    fs::{
        OpenFlags, SeekWhence, Stat, directory_tree::DirectoryTreeNode, dirent::Dirent, fat32::{DiskInodeType, PageCache}, file_trait::File
    },
    mm::UserBuffer,
    net::{MAX_BUFFER_SIZE, SHUT_WR, config::NET_INTERFACE},
    task::{block_current_and_run_next, suspend_current_and_run_next, wait_interruptible, wait_interruptible_timeout},
    timer::TimeSpec,
    utils::error::{GeneralRet, SyscallErr, SyscallRet},
};
use alloc::{
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use log::info;
use smoltcp::{
    iface::SocketHandle,
    socket::{self, raw, raw::PacketMetadata},
    wire::{IpEndpoint, IpListenEndpoint, IpProtocol, IpVersion},
};

pub struct RawSocket {
    inner: Mutex<RawSocketInner>,
    socket_handler: SocketHandle,
}

#[allow(unused)]
struct RawSocketInner {
    local_endpoint: Option<IpListenEndpoint>,
    remote_endpoint: Option<IpEndpoint>,
    ip_version: IpVersion,
    ip_protocol: IpProtocol,
    recvbuf_size:usize,
    sendbuf_size:usize,
}

impl Socket for RawSocket {
    fn bind(&self, addr: IpListenEndpoint) -> SyscallRet {
        log::info!("[Raw::bind] bind to {:?}", addr);
        NET_INTERFACE.poll();
        todo!()
    }

    fn listen(&self) -> SyscallRet {
        todo!()
    }

    fn connect<'a>(&'a self, _addr_buf: &'a [u8]) -> SyscallRet {
        todo!()
    }

    fn accept(&self, _sockfd: u32, _addr: usize, _addrlen: usize) -> SyscallRet {
        todo!()
    }

    fn socket_type(&self) -> super::SocketType {
        super::SocketType::SOCK_RAW
    }

    fn recv_buf_size(&self) -> usize {
        self.inner.lock().recvbuf_size
    }

    fn send_buf_size(&self) -> usize {
        self.inner.lock().sendbuf_size
    }

    fn set_recv_buf_size(&self, size: usize) {
        self.inner.lock().recvbuf_size = size;
    }

    fn set_send_buf_size(&self, size: usize) {
        self.inner.lock().sendbuf_size = size; 
    }

    fn loacl_endpoint(&self) -> IpListenEndpoint {
        todo!()
    }

    fn remote_endpoint(&self) -> Option<IpEndpoint> {
        self.inner.lock().remote_endpoint
    }

    fn shutdown(&self, how: u32) -> GeneralRet<()> {
        info!("[RawSocket::shutdown] how {}", how);
        todo!()
    }

    fn set_nagle_enabled(&self, _enabled: bool) -> SyscallRet {
        todo!()
    }

    fn set_keep_alive(&self, _enabled: bool) -> SyscallRet {
        todo!()
    }

    fn send_to(&self, user_buf: &[u8], dest_addr: IpEndpoint) -> SyscallRet{
        let (version, protocol) = {
        let inner = self.inner.lock();
        (inner.ip_version, inner.ip_protocol)
        };
        match version {
            IpVersion::Ipv4 =>{
                let target_ip = match dest_addr.addr {
                smoltcp::wire::IpAddress::Ipv4(ip) => ip,
                _ => return Err(SyscallErr::EINVAL),
                };
                let mut packet_buf = vec![0u8; 20 + user_buf.len()];

                log::info!("[RawSocketsendto] make ipv4 head...");
                //封装IP头
                let mut ip_pkg = smoltcp::wire::Ipv4Packet::new_unchecked(&mut packet_buf);
                ip_pkg.set_version(4);
                ip_pkg.set_header_len(20);
                ip_pkg.set_total_len((20 + user_buf.len()) as u16);
                ip_pkg.set_next_header(protocol); // 使用刚才解锁拿到的 protocol
                ip_pkg.set_hop_limit(64);
                ip_pkg.set_dst_addr(target_ip);
                ip_pkg.set_src_addr(smoltcp::wire::Ipv4Address([127, 0, 0, 1])); //暂时先硬编码为本地回环地址
            
                ip_pkg.payload_mut().copy_from_slice(user_buf);
                ip_pkg.fill_checksum();

                NET_INTERFACE.poll();
                let ret=NET_INTERFACE.raw_socket(self.socket_handler,|socket|{
                    log::info!("[RawSocket] Sending {} bytes to {}", user_buf.len(), target_ip);
                    match socket.send_slice(ip_pkg.into_inner()) {
                    Ok(_) => Ok(user_buf.len()),
                    Err(_) => Err(SyscallErr::ENOBUFS),
                    }
                });
                NET_INTERFACE.poll();
                ret
            }
            IpVersion::Ipv6 => {todo!()}
        }
    }
}

impl RawSocket {
    pub fn new(protocol: u32) -> Self {
        let tx_buf = socket::raw::PacketBuffer::new(
            vec![PacketMetadata::EMPTY,PacketMetadata::EMPTY],
            vec![0 as u8; MAX_BUFFER_SIZE],
        );
        let rx_buf = socket::raw::PacketBuffer::new(
            vec![PacketMetadata::EMPTY,PacketMetadata::EMPTY],
            vec![0 as u8; MAX_BUFFER_SIZE],
        );
        let socket = raw::Socket::new(
            smoltcp::wire::IpVersion::Ipv4,
            smoltcp::wire::IpProtocol::from(protocol as u8),
            rx_buf,
            tx_buf
        );
        let socket_handler = NET_INTERFACE.add_socket(socket);
        log::info!("[RawSocket::new] new {}",socket_handler);
        NET_INTERFACE.poll();
        let inner = RawSocketInner{
            local_endpoint: None,          // no local address bound yet
            remote_endpoint: None,         // no remote peer
            ip_version: IpVersion::Ipv4,
            ip_protocol:  IpProtocol::from(protocol as u8),
            recvbuf_size: MAX_BUFFER_SIZE,
            sendbuf_size: MAX_BUFFER_SIZE,
        };

        Self {
            inner: Mutex::new(inner),
            socket_handler,
        }

    }
    
}

impl File for RawSocket {
    fn deep_clone(&self) -> Arc<dyn File> {
        todo!()
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
            //权衡写法，将正数err转为负数错误类型
            Err(err) => err.as_errno_ret(),
        }
    }

    fn write(&self, _offset: Option<&mut usize>, buf: &[u8]) -> usize {
        NET_INTERFACE.poll();
        let ret = NET_INTERFACE.raw_socket(self.socket_handler, |socket|{
            if ! socket.can_send() {
                log::info!("[RawSendFuture::poll] cannot send yet");
                suspend_current_and_run_next();
                return SyscallErr::EAGAIN as usize;
            }
            log::info!("[RawSendFuture::poll] start to send...");
            match socket.send_slice(buf) {
                Ok(()) => buf.len(),
                Err(_) => SyscallErr::ENOBUFS as usize,
            }
           
        });
        NET_INTERFACE.poll();
        ret
    }

    fn r_ready(&self) -> bool {
        todo!()
    }

    fn w_ready(&self) -> bool {
        todo!()
    }

    fn read_user(&self, _offset: Option<usize>, _buf: UserBuffer) -> usize {
        todo!()
    }

    fn write_user(&self, _offset: Option<usize>, _buf: UserBuffer) -> usize {
        todo!()
    }

    fn get_size(&self) -> usize {
        todo!()
    }

    fn get_stat(&self) -> Stat {
        todo!()
    }

    fn get_file_type(&self) -> DiskInodeType {
        todo!()
    }

    fn info_dirtree_node(&self, _dirnode_ptr: Weak<DirectoryTreeNode>) {
        todo!()
    }

    fn get_dirtree_node(&self) -> Option<Arc<DirectoryTreeNode>> {
        todo!()
    }

    fn open(&self, _flags: OpenFlags, _special_use: bool) -> Arc<dyn File> {
        todo!()
    }

    fn open_subfile(&self) -> Result<Vec<(String, Arc<dyn File>)>, isize> {
        todo!()
    }

    fn create(&self, _name: &str, _file_type: DiskInodeType) -> Result<Arc<dyn File>, isize> {
        todo!()
    }

    fn link_child(&self, _name: &str, _child: &Self) -> Result<(), isize>
    where
        Self: Sized,
    {
        todo!()
    }

    fn unlink(&self, _delete: bool) -> Result<(), isize> {
        todo!()
    }

    fn get_dirent(&self, _count: usize) -> Vec<Dirent> {
        todo!()
    }

    fn lseek(&self, _offset: isize, _whence: SeekWhence) -> Result<usize, isize> {
        todo!()
    }

    fn modify_size(&self, _diff: isize) -> Result<(), isize> {
        todo!()
    }

    fn truncate_size(&self, _new_size: usize) -> Result<(), isize> {
        todo!()
    }

    fn set_timestamp(&self, _ctime: Option<usize>, _atime: Option<usize>, _mtime: Option<usize>) {
        todo!()
    }

    fn get_single_cache(&self, _offset: usize) -> Result<Arc<Mutex<PageCache>>, ()> {
        todo!()
    }

    fn get_all_caches(&self) -> Result<Vec<Arc<Mutex<PageCache>>>, ()> {
        todo!()
    }

    fn oom(&self) -> usize {
        todo!()
    }

    fn hang_up(&self) -> bool {
        todo!()
    }

    fn ioctl(&self, _cmd: u32, _argp: usize) -> isize {
        todo!()
    }

    fn fcntl(&self, _cmd: u32, _arg: u32) -> isize {
        todo!()
    }
    
}

impl RawSocket {
    fn _read<'a>(&'a self, buf: &'a mut [u8]) -> GeneralRet<usize> {
        loop {
            NET_INTERFACE.poll();
            let ret = NET_INTERFACE.raw_socket(self.socket_handler,|socket|{
                if !socket.can_recv() {
                    // panic!();
                    log::trace!("[RawRecvFuture::poll] cannot recv yet");
                    return Err(SyscallErr::EAGAIN);
                }
                log::info!("[RawRecvFuture::poll] start to recv...");
                
                match socket.recv_slice(buf) {
                    Ok(nbytes) => {
                        info!("[TcpRecvFuture::poll] recv {} bytes", nbytes);
                        let packet = smoltcp::wire::Ipv4Packet::new_unchecked(&buf[..nbytes]);        
                        let src_addr = packet.src_addr();
                        let mut inner = self.inner.lock();
                        inner.remote_endpoint = Some(IpEndpoint::new(src_addr.into(), 0));
                        Ok(nbytes)
                    }
                    Err(_) => return Err(SyscallErr::ENOTCONN),
                }

            });

            NET_INTERFACE.poll();
            match ret {
                Ok(result) => return GeneralRet::Ok(result),
                Err(SyscallErr::EAGAIN) => {
                    //等待SIGALRM信号，进入Interruptible状态而不是Ready状态
                    wait_interruptible()?;
                    continue;
                }
                Err(err) => return GeneralRet::Err(err),
            }
        }
    }
}
