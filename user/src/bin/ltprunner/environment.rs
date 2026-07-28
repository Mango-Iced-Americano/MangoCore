use alloc::format;
use alloc::string::String;
use user_lib::{close, open, OpenFlags};

#[cfg(target_arch = "loongarch64")]
const LTP_TIMEOUT_MUL_ENV: &str = "LTP_TIMEOUT_MUL=2\0";
#[cfg(not(target_arch = "loongarch64"))]
const LTP_TIMEOUT_MUL_ENV: &str = "LTP_TIMEOUT_MUL=2\0";
const LTP_TIMEOUT_MUL_DISABLED_ENV: &str = "MANGO_LTP_TIMEOUT_MUL_DISABLED=1\0";

pub(crate) struct PrecomputedEnv {
    _ltp_root_s: String,
    _path_s: String,
    _tmpdir_s: String,
    _tmpbase_s: String,
    _pwd_s: String,
    pub(super) env_preload: [*const u8; 19],
    pub(super) env_no_preload: [*const u8; 19],
}

fn has_scratch_device() -> bool {
    let fd = open("/dev/vdb2\0", OpenFlags::RDWR);
    if fd >= 0 {
        let _ = close(fd as usize);
        true
    } else {
        false
    }
}

pub fn precompute_env(ltproot: &str, tmpdir: &str, libc: &str) -> PrecomputedEnv {
    let ltp_root_s = format!("LTPROOT={}\0", ltproot);
    let path_s = format!(
        "PATH=/bin:/sbin:/usr/bin:/usr/sbin:{}/testcases/bin:{}/bin:{}/testcases/lib\0",
        ltproot, ltproot, ltproot
    );
    let tmpdir_s = format!("TMPDIR={}\0", tmpdir);
    let tmpbase_s = format!("TMPBASE={}\0", tmpdir);
    let pwd_s = format!("PWD={}/testcases/bin\0", ltproot);

    let ld_preload_ptr: *const u8 = "LD_PRELOAD=/ltp_proto_compat.so\0".as_ptr();
    let null_ptr: *const u8 = core::ptr::null();
    let ltp_timeout_mul_ptr: *const u8 = if cfg!(target_arch = "riscv64") && libc == "musl" {
        LTP_TIMEOUT_MUL_DISABLED_ENV.as_ptr()
    } else {
        LTP_TIMEOUT_MUL_ENV.as_ptr()
    };
    let ltp_dev_ptr: *const u8 = if has_scratch_device() {
        "LTP_DEV=/dev/vdb2\0".as_ptr()
    } else {
        null_ptr
    };
    let ltp_single_fs_ptr: *const u8 = if has_scratch_device() {
        "LTP_SINGLE_FS_TYPE=ext4\0".as_ptr()
    } else {
        null_ptr
    };

    let env_preload: [*const u8; 19] = [
        ltp_root_s.as_ptr(),
        path_s.as_ptr(),
        tmpdir_s.as_ptr(),
        tmpbase_s.as_ptr(),
        "HOME=/\0".as_ptr(),
        pwd_s.as_ptr(),
        "SHELL=/bin/bash\0".as_ptr(),
        "TERM=dumb\0".as_ptr(),
        "LTP_COLORIZE_OUTPUT=y\0".as_ptr(),
        "LTP_DEV_FS_TYPE=ext4\0".as_ptr(),
        "LTP_IPC_PATH=/tmp\0".as_ptr(),
        "LANG=C.UTF-8\0".as_ptr(),
        "LTP_REPRODUCIBLE_OUTPUT=n\0".as_ptr(),
        ltp_timeout_mul_ptr,
        "KCONFIG_PATH=/proc/config\0".as_ptr(),
        ld_preload_ptr,
        ltp_dev_ptr,
        ltp_single_fs_ptr,
        null_ptr,
    ];
    let env_no_preload: [*const u8; 19] = [
        ltp_root_s.as_ptr(),
        path_s.as_ptr(),
        tmpdir_s.as_ptr(),
        tmpbase_s.as_ptr(),
        "HOME=/\0".as_ptr(),
        pwd_s.as_ptr(),
        "SHELL=/bin/sh\0".as_ptr(),
        "TERM=dumb\0".as_ptr(),
        "LTP_COLORIZE_OUTPUT=y\0".as_ptr(),
        "LTP_DEV_FS_TYPE=ext4\0".as_ptr(),
        "LTP_IPC_PATH=/tmp\0".as_ptr(),
        "LANG=C.UTF-8\0".as_ptr(),
        "LTP_REPRODUCIBLE_OUTPUT=n\0".as_ptr(),
        ltp_timeout_mul_ptr,
        "KCONFIG_PATH=/proc/config\0".as_ptr(),
        ltp_dev_ptr,
        ltp_single_fs_ptr,
        null_ptr,
        null_ptr,
    ];
    PrecomputedEnv {
        _ltp_root_s: ltp_root_s,
        _path_s: path_s,
        _tmpdir_s: tmpdir_s,
        _tmpbase_s: tmpbase_s,
        _pwd_s: pwd_s,
        env_preload,
        env_no_preload,
    }
}
