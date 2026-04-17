pub mod block;
pub mod serial;
pub mod net;

pub use block::BLOCK_DEVICE;
pub use net::NET_DEVICE;
pub use serial::ns16550a::Ns16550a;
