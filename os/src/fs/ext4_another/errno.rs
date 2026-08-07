use crate::drivers::block::BlockDeviceError;
use crate::utils::error::SyscallErr;

pub(crate) fn from_another(error: another_ext4::ErrCode) -> SyscallErr {
    match error {
        another_ext4::ErrCode::EPERM => SyscallErr::EPERM,
        another_ext4::ErrCode::ENOENT => SyscallErr::ENOENT,
        another_ext4::ErrCode::EIO => {
            log::error!("[ext4_another] BRIDGE EIO: raw={:?}", error);
            SyscallErr::EIO
        }
        another_ext4::ErrCode::EAGAIN => SyscallErr::EAGAIN,
        another_ext4::ErrCode::ENXIO => SyscallErr::ENXIO,
        another_ext4::ErrCode::E2BIG => SyscallErr::E2BIG,
        another_ext4::ErrCode::ENOMEM => SyscallErr::ENOMEM,
        another_ext4::ErrCode::EACCES => SyscallErr::EACCES,
        another_ext4::ErrCode::EFAULT => SyscallErr::EFAULT,
        another_ext4::ErrCode::EEXIST => SyscallErr::EEXIST,
        another_ext4::ErrCode::ENODEV => SyscallErr::ENODEV,
        another_ext4::ErrCode::ENOTDIR => SyscallErr::ENOTDIR,
        another_ext4::ErrCode::EISDIR => SyscallErr::EISDIR,
        another_ext4::ErrCode::EINVAL => SyscallErr::EINVAL,
        another_ext4::ErrCode::EFBIG => SyscallErr::EFBIG,
        another_ext4::ErrCode::ENOSPC => SyscallErr::ENOSPC,
        another_ext4::ErrCode::EROFS => SyscallErr::EROFS,
        another_ext4::ErrCode::EMLINK => SyscallErr::EMLINK,
        another_ext4::ErrCode::ERANGE => SyscallErr::ERANGE,
        another_ext4::ErrCode::ENAMETOOLONG => SyscallErr::ENAMETOOLONG,
        another_ext4::ErrCode::ENOTEMPTY => SyscallErr::ENOTEMPTY,
        another_ext4::ErrCode::ENODATA => SyscallErr::ENODATA,
        another_ext4::ErrCode::ENOTSUP => SyscallErr::EOPNOTSUPP,
    }
}

/// Preserve the backend error mapping while naming a failing hot-path operation.
pub(crate) fn from_another_op(error: &another_ext4::Ext4Error, op: &str) -> SyscallErr {
    if error.code() == another_ext4::ErrCode::EIO {
        log::error!("[ext4_another] BRIDGE EIO: op={} error={:?}", op, error);
    }
    from_another(error.code())
}

pub(crate) const fn from_block_device(error: BlockDeviceError) -> another_ext4::ErrCode {
    match error {
        BlockDeviceError::InvalidBufferLength => another_ext4::ErrCode::EINVAL,
        BlockDeviceError::OutOfBounds | BlockDeviceError::DeviceError => another_ext4::ErrCode::EIO,
        BlockDeviceError::DeviceUnavailable => another_ext4::ErrCode::ENXIO,
        BlockDeviceError::FlushUnsupported => another_ext4::ErrCode::EROFS,
    }
}
