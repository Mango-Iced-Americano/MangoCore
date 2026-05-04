pub mod unix;

use crate::net::{PSOCK, Socket, SocketFile};
use crate::utils::error::SyscallRet;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

pub enum UnixEndpoint {
    Path(String),
    Abstract(Vec<u8>),
    Unnamed,
}


pub fn _create_unix_socket(socket_type: u32) -> isize {
    todo!()
}