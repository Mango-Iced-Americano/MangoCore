pub use super::address_space::{check_page_fault, AddressSpace, MemoryError};

pub type MemorySet<T> = AddressSpace<T>;
