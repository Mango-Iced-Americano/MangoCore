//! Map lwext4 C error codes (positive i32) → MangoCore `SyscallErr`.
//!
//! lwext4 returns 0 on success and negative-errno on failure.  The raw i32
//! error code is the absolute value of the negated Linux errno (e.g. ENOENT=2,
//! so lwext4 returns -2).  This function maps the *absolute* value to the
//! corresponding `SyscallErr` variant.

use crate::utils::error::SyscallErr;

/// Convert an lwext4 error return (raw i32, already within `Err(e)`) to a
/// VFS-compatible `SyscallErr`.  The caller must have already unwrapped the
/// `Result` — this function expects the error code **without** sign.
pub fn from_lwext4(e: i32) -> SyscallErr {
    // lwext4 C code uses Linux-standard errno values (positive).
    // We map the errno number directly to SyscallErr variants.
    match e {
        1 => SyscallErr::EPERM,
        2 => SyscallErr::ENOENT,
        5 => SyscallErr::EIO,
        12 => SyscallErr::ENOMEM,
        13 => SyscallErr::EACCES,
        16 => SyscallErr::EBUSY,
        17 => SyscallErr::EEXIST,
        18 => SyscallErr::EXDEV,
        19 => SyscallErr::ENODEV,
        20 => SyscallErr::ENOTDIR,
        21 => SyscallErr::EISDIR,
        22 => SyscallErr::EINVAL,
        28 => SyscallErr::ENOSPC,
        30 => SyscallErr::EROFS,
        36 => SyscallErr::ENAMETOOLONG,
        39 => SyscallErr::ENOTEMPTY,
        40 => SyscallErr::ELOOP,
        95 => SyscallErr::EOPNOTSUPP,
        _ => {
            log::warn!("[lwext4] unmapped errno {}, falling back to EIO", e);
            SyscallErr::EIO
        }
    }
}
