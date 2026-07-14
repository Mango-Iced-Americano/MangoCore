use super::common::*;

pub fn sys_utimensat(
    dirfd: usize,
    pathname: *const u8,
    times: *const [TimeSpec; 2],
    flags: u32,
) -> isize {
    const UTIME_NOW: usize = 0x3fffffff;
    const UTIME_OMIT: usize = 0x3ffffffe;
    let token = current_user_token();
    let path = if !pathname.is_null() {
        match user_cstring(token, pathname) {
            Ok(path) => path,
            Err(errno) => return errno,
        }
    } else {
        String::new()
    };
    let flags = match UtimensatFlags::from_bits(flags) {
        Some(flags) => flags,
        None => {
            warn!("[sys_utimensat] unknown flags");
            return EINVAL;
        }
    };

    info!(
        "[sys_utimensat] dirfd: {}, path: {}, times: {:?}, flags: {:?}",
        dirfd as isize, path, times, flags
    );

    let _file = match __openat(dirfd, &path) {
        Ok(file) => file,
        Err(errno) => return errno,
    };

    let now = current_timespec();
    let timespec = if !times.is_null() {
        match UserPtr::new(times).read(token) {
            Ok(timespec) => timespec,
            Err(_) => {
                log::error!("[sys_utimensat] Failed to copy from {:?}", times);
                return EFAULT;
            }
        }
    } else {
        [now; 2]
    };
    let mut atime = Some(now.tv_sec);
    let mut mtime = Some(now.tv_sec);
    if !times.is_null() {
        match timespec[0].tv_nsec {
            UTIME_NOW => (),
            UTIME_OMIT => atime = None,
            _ => atime = Some(timespec[0].tv_sec),
        }
        match timespec[1].tv_nsec {
            UTIME_NOW => (),
            UTIME_OMIT => mtime = None,
            _ => mtime = Some(timespec[1].tv_sec),
        }
    }

    if atime.is_some() || mtime.is_some() {
        if let Ok(mut metadata) = _file.metadata() {
            if let Some(atime) = atime {
                metadata.atime = TimeSpec::from_s(atime);
            }
            if let Some(mtime) = mtime {
                metadata.mtime = TimeSpec::from_s(mtime);
            }
            if let Err(e) = _file.inode.set_metadata(&metadata) {
                return -(e as isize);
            }
        }
    }
    SUCCESS
}
