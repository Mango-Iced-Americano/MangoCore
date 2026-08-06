//! 平台设备查询入口。
//!
//! `manager` 是实现拆分，不进入调用方命名；上层统一使用
//! `hal::device::{DeviceManager, DeviceQueryError}`。

mod manager;
#[allow(
    unused_imports,
    reason = "查询接口在后续 Driver 融合批次接入，本批先稳定公共命名面"
)]
pub use manager::{DeviceManager, DeviceQueryError};
