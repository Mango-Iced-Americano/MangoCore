use super::common::*;

pub fn sys_readlinkat(dirfd: usize, pathname: *const u8, buf: *mut u8, bufsiz: usize) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path = match user_cstring(token, pathname) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }
    if bufsiz == 0 {
        return EINVAL;
    }

    // Linux: readlinkat(fd, "", buf, size) with non-AT_FDCWD dirfd reads
    // the symlink referred to by dirfd (AT_EMPTY_PATH semantics)
    if path.is_empty() {
        if dirfd == AT_FDCWD {
            return ENOENT;
        }
        // Read the symlink target from dirfd's inode
        let files_ref = current_task().unwrap().process.files();
        let fd_table = files_ref.lock();
        let file = match fd_table.get_file(dirfd) {
            Ok(f) => f,
            Err(e) => return -(e as isize),
        };
        let inode = file.inode.clone();
        drop(fd_table);
        let md = match inode.metadata() {
            Ok(m) => m,
            Err(e) => return -(e as isize),
        };
        if md.file_type != FileType::SymLink {
            return EINVAL;
        }
        let link_len = (md.size.max(0) as usize).min(4096);
        let mut link_buf = alloc::vec![0u8; link_len];
        let n = match inode.read_at(
            0,
            link_buf.len(),
            &mut link_buf,
            spin::Mutex::new(vfs::FilePrivateData::Unused).lock(),
        ) {
            Ok(n) => n,
            Err(_) => return EINVAL,
        };
        unsafe { link_buf.set_len(n) };
        let target = match String::from_utf8(link_buf) {
            Ok(s) => alloc::string::String::from(s.trim_end_matches('\0')),
            Err(_) => return EINVAL,
        };
        let len = target.len().min(bufsiz);
        let mut writer = match UserBufferWriter::new(token, buf, len) {
            Ok(w) => w,
            Err(_) => return EFAULT,
        };
        if writer.write_from(target.as_bytes()).is_err() {
            return EFAULT;
        }
        return len as isize;
    }

    let real_path = if path.as_str() == "/proc/self/exe" {
        let exe_path = task.process.exe_path();
        if exe_path.is_empty() {
            return ENOENT;
        }
        exe_path
    } else {
        let start = if path.starts_with('/') {
            crate::fs::current_root_inode()
        } else {
            match resolve_start_inode(dirfd) {
                Ok(s) => s,
                Err(e) => return e,
            }
        };

        let (uid, fsgid, groups) = caller_ids_and_groups();
        let perm_result = check_parent_search_access(&start, &path, uid, fsgid, &groups);
        if perm_result != SUCCESS {
            return perm_result;
        }

        // 使用新 VFS 路径解析 (不跟随最终符号链接)
        let inode = match vfs_lookup(&start, &path, false) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        };
        let md = match inode.metadata() {
            Ok(md) => {
                info!(
                    "[sys_readlinkat] vfs_lookup OK: path={}, file_type={:?}, size={}",
                    path, md.file_type, md.size
                );
                md
            }
            Err(e) => {
                warn!(
                    "[sys_readlinkat] metadata() failed: path={}, err={:?}",
                    path, e
                );
                return EINVAL;
            }
        };
        if md.file_type != vfs::FileType::SymLink {
            debug!(
                "[sys_readlinkat] not a symlink: path={}, file_type={:?}",
                path, md.file_type
            );
            return EINVAL;
        }
        // 读取符号链接目标内容
        let link_len = (md.size.max(0) as usize).min(4096);
        let mut link_buf = alloc::vec![0u8; link_len];
        let n = match inode.read_at(
            0,
            link_buf.len(),
            &mut link_buf,
            spin::Mutex::new(vfs::FilePrivateData::Unused).lock(),
        ) {
            Ok(n) => n,
            Err(_) => return EINVAL,
        };
        unsafe { link_buf.set_len(n) };
        match String::from_utf8(link_buf) {
            Ok(s) => alloc::string::String::from(s.trim_end_matches('\0')),
            Err(_) => return EINVAL,
        }
    };

    let len = real_path.len().min(bufsiz);
    // readlink does not add a null byte
    let bytes = real_path.as_bytes();
    let mut user_buf = match UserBufferWriter::new(token, buf, len) {
        Ok(writer) => writer,
        Err(_) => return EFAULT,
    };
    if user_buf.write_from(&bytes[..len]).is_err() {
        log::error!("[sys_readlinkat] Failed to copy to {:?}", buf);
        return EFAULT;
    }

    debug!(
        "[sys_readlinkat] dirfd: {}, pathname: {}, buf: {:?}, bufsiz: {}, written: {}",
        dirfd as isize, path, buf, bufsiz, real_path
    );

    len as isize
}
