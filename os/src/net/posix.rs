//
// posix.rs 记录了系统调用时用到的结构
//

use bitflags::bitflags;

/// POSIX socket() 系统调用的 type 参数解析器。
/// 纯类型位（低 4 位）与控制标志（NONBLOCK / CLOEXEC）共存在同一个 u32 中，
/// 此 bitflags 用于在 syscall 入口处一次性解析，提取出纯类型后转换为 `PSOCK` 枚举。
bitflags! {
    pub struct PosixArgsSocketType: u32 {
        const STREAM    = 1;
        const DGRAM     = 2;
        const RAW       = 3;
        const RDM       = 4;
        const SEQPACKET = 5;
        const DCCP      = 6;
        const PACKET    = 10;

        const NONBLOCK  = 0x800;
        const CLOEXEC   = 1 << 19;
    }
}

impl PosixArgsSocketType {
    /// 仅保留低 4 位的纯类型位，去除控制标志。
    #[inline(always)]
    pub fn types(&self) -> PosixArgsSocketType {
        PosixArgsSocketType::from_bits(self.bits() & 0xF).unwrap()
    }

    #[inline(always)]
    pub fn is_nonblock(&self) -> bool {
        self.contains(PosixArgsSocketType::NONBLOCK)
    }

    #[inline(always)]
    pub fn is_cloexec(&self) -> bool {
        self.contains(PosixArgsSocketType::CLOEXEC)
    }
}

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

/// Linux `struct mmsghdr` used by batched message syscalls.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MMsgHdr {
    pub msg_hdr: MsgHdr,
    pub msg_len: u32,
    #[cfg(target_pointer_width = "64")]
    pub _pad: u32,
}
