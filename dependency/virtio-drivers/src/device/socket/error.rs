//! This module contain the error from the VirtIO socket driver.

use core::result;

/// The error type of VirtIO socket driver.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SocketError {
    /// There is an existing connection.
    ConnectionExists,
    /// The device is not connected to any peer.
    NotConnected,
    /// Peer socket is shutdown.
    PeerSocketShutdown,
    /// The given buffer is shorter than expected.
    BufferTooShort,
    /// The given buffer for output is shorter than expected.
    OutputBufferTooShort(usize),
    /// The given buffer has exceeded the maximum buffer size.
    BufferTooLong(usize, usize),
    /// Unknown operation.
    UnknownOperation(u16),
    /// Invalid operation,
    InvalidOperation,
    /// Invalid number.
    InvalidNumber,
    /// Unexpected data in packet.
    UnexpectedDataInPacket,
    /// Peer has insufficient buffer space, try again later.
    InsufficientBufferSpaceInPeer,
    /// Recycled a wrong buffer.
    RecycledWrongBuffer,
}

pub type Result<T> = result::Result<T, SocketError>;
