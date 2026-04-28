//
// posix.rs 记录了系统调用时用到的结构
//

#[repr(C)]
#[derive(Debug, Clone, Copy)]

pub struct MsgHdr {
    /// 指向一个SockAddr结构体的指针
    pub msg_name: *mut u8,
    /// SockAddr结构体的大小
    pub msg_namelen: u32,
    /// Padding to keep the same layout as Linux `struct msghdr` on 64-bit.
    #[cfg(target_pointer_width = "64")]
    pub _pad0: u32,
    /// scatter/gather array
    pub msg_iov: *mut crate::fs::iov::IOVec,
    /// elements in msg_iov
    pub msg_iovlen: usize,
    /// 辅助数据
    pub msg_control: *mut u8,
    /// 辅助数据长度
    pub msg_controllen: usize,
    /// 接收到的消息的标志
    pub msg_flags: i32,
    /// Padding to keep the same layout as Linux `struct msghdr` on 64-bit.
    #[cfg(target_pointer_width = "64")]
    pub _pad1: i32,
}
