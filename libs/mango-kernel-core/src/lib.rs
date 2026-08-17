#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod wait_queue_core;
pub mod bootargs;
pub mod time;
pub mod page_cache;
pub mod ring_buffer;
pub mod uart_rx_ring;
pub mod path;
pub mod wait_result;
pub mod recycle_alloc;

#[cfg(test)]
#[path = "wait_queue_core_tests.rs"]
mod wait_queue_core_tests;
