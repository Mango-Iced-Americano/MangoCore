//! /proc/device-tree/model — firmware-provided platform model

use crate::fs::procfs::proc_read_str;
use crate::hal::platform::platform_info;
use crate::utils::error::SyscallErr;

pub fn model_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let model = platform_info().model.as_deref().unwrap_or("");
    proc_read_str(offset, len, buf, model)
}
