use super::common::*;

const UTIME_NOW: usize = 0x3fffffff;
const UTIME_OMIT: usize = 0x3ffffffe;

pub fn sys_utimensat(
    dirfd: usize,
    pathname: *const u8,
    times: *const [TimeSpec; 2],
    flags: u32,
) -> isize {
    let token = current_user_token();
    // NULL pathname → EFAULT (Linux: null user pointer before any other check)
    if pathname.is_null() {
        return EFAULT;
    }
    let path = match user_cstring(token, pathname) {
        Ok(path) => path,
        Err(errno) => return errno,
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

    // Empty path without AT_EMPTY_PATH → ENOENT
    if path.is_empty() {
        return ENOENT;
    }

    // Resolve path, respecting AT_SYMLINK_NOFOLLOW
    let follow_final = !flags.contains(UtimensatFlags::AT_SYMLINK_NOFOLLOW);
    let start = if path.starts_with('/') {
        crate::fs::current_root_inode()
    } else {
        match resolve_start_inode(dirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    };

    // Read times pointer before path lookup so EFAULT takes priority over ENOENT
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

    let inode = match vfs_lookup(&start, &path, follow_final) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };

    // EROFS takes priority over EACCES/EPERM: check read-only mount before DAC
    if let Some(mnt) = inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
        if mnt.mount_fs.mount_flags().contains(vfs::MountFlags::RDONLY) {
            return EROFS;
        }
    }

    let md = match inode.metadata() {
        Ok(md) => md,
        Err(e) => return -(e as isize),
    };

    let (uid, fsgid, groups) = caller_ids_and_groups();

    // Permission checking (Linux semantics):
    //   - Specific time (tv_nsec is a real value): need ownership → EPERM
    //   - UTIME_NOW or times==NULL: need W_OK or ownership → EACCES
    //   - UTIME_OMIT: no permission needed
    let mut needs_ownership = false;
    let mut needs_write_or_own = false;

    if times.is_null() {
        // Both atime and mtime set to "now" (equivalent to UTIME_NOW)
        needs_write_or_own = true;
    } else {
        for ts in timespec.iter() {
            match ts.tv_nsec {
                UTIME_NOW => needs_write_or_own = true,
                UTIME_OMIT => { /* no permission needed */ }
                _ => needs_ownership = true,
            }
        }
    }

    // Check ownership first (EPERM takes priority over EACCES)
    let is_owner = uid == 0 || uid == md.uid;
    if needs_ownership && !is_owner {
        return EPERM;
    }
    if needs_write_or_own && !is_owner {
        if !has_final_access(&md, FaccessatMode::W_OK, uid, fsgid, &groups) {
            return EACCES;
        }
    }

    // Compute new timestamps
    let mut atime = Some(now.tv_sec);
    let mut mtime = Some(now.tv_sec);
    if !times.is_null() {
        if timespec[0].tv_nsec == UTIME_OMIT {
            atime = None;
        } else if timespec[0].tv_nsec != UTIME_NOW {
            atime = Some(timespec[0].tv_sec);
        }
        if timespec[1].tv_nsec == UTIME_OMIT {
            mtime = None;
        } else if timespec[1].tv_nsec != UTIME_NOW {
            mtime = Some(timespec[1].tv_sec);
        }
    }

    // Apply changes via set_metadata on the target inode directly
    if atime.is_some() || mtime.is_some() {
        if let Ok(mut metadata) = inode.metadata() {
            if let Some(atime) = atime {
                metadata.atime = TimeSpec::from_s(atime);
            }
            if let Some(mtime) = mtime {
                metadata.mtime = TimeSpec::from_s(mtime);
            }
            if let Err(e) = inode.set_metadata(&metadata) {
                return -(e as isize);
            }
        }
    }
    SUCCESS
}
