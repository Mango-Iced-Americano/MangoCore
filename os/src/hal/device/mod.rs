//! Device manager — query-only view of discovered platform devices.
//!
//! Provides fast lookup APIs over the `PlatformInfo.devices` list.
//! Devices are immutable after platform initialization.

mod manager;
pub use manager::{DeviceManager, DeviceQueryError};
