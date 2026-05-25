use super::errno::*;
use crate::fs::poll::{ppoll, pselect, FdSet, PollFd};
use crate::fs::vfs::{self, FileFlags, FileType, SeekFrom};
use crate::fs::*;
use crate::hal::BLOCK_SZ;
use crate::mm::{
    MapPermission, UserBufferReader, UserBufferWriter, UserCString, UserIoVec, UserPtr,
    UserPtrMut, UserSlice, VirtAddr,
};
use crate::syscall::utils::wait_io_core;
use crate::task::{
    current_task, current_user_token, is_executable_inode_busy, signal, WaitQueue, WaitResult,
};
use crate::timer::{current_timespec, TimeSpec};
use crate::utils::error::SyscallErr;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::mem::size_of;
use core::panic;
use log::{debug, error, info, trace, warn};
use num_enum::FromPrimitive;

// 防止用户传入过大参数导致内核 OOM 或者长时间阻塞
const MAX_SYSCALL_BUFFER_SIZE: usize = 2 * 1024 * 1024; // 限制为 2 MiB
const OPEN_ACCMODE_MASK: u32 = 0o3;

pub const AT_FDCWD: usize = 100usize.wrapping_neg();

/// 将旧的 OpenFlags 转换为新的 VFS FileFlags
fn _open_flags_to_vfs_flags(o: OpenFlags) -> vfs::FileFlags {
    let mut f = vfs::FileFlags::empty();
    if o.contains(OpenFlags::O_RDONLY) {
        f.insert(vfs::FileFlags::O_RDONLY);
    }
    if o.contains(OpenFlags::O_WRONLY) {
        f.insert(vfs::FileFlags::O_WRONLY);
    }
    if o.contains(OpenFlags::O_RDWR) {
        f.insert(vfs::FileFlags::O_RDWR);
    }
    if o.contains(OpenFlags::O_CREAT) {
        f.insert(vfs::FileFlags::O_CREAT);
    }
    if o.contains(OpenFlags::O_TRUNC) {
        f.insert(vfs::FileFlags::O_TRUNC);
    }
    if o.contains(OpenFlags::O_APPEND) {
        f.insert(vfs::FileFlags::O_APPEND);
    }
    if o.contains(OpenFlags::O_NONBLOCK) {
        f.insert(vfs::FileFlags::O_NONBLOCK);
    }
    if o.contains(OpenFlags::O_DIRECTORY) {
        f.insert(vfs::FileFlags::O_DIRECTORY);
    }
    if o.contains(OpenFlags::O_CLOEXEC) {
        f.insert(vfs::FileFlags::O_CLOEXEC);
    }
    if o.contains(OpenFlags::O_NOFOLLOW) {
        f.insert(vfs::FileFlags::O_NOFOLLOW);
    }
    if o.contains(OpenFlags::O_PATH) {
        f.insert(vfs::FileFlags::O_PATH);
    }
    f
}

/// 从 dirfd 解析起始 IndexNode（用于路径操作）
fn resolve_start_inode(dirfd: usize) -> Result<Arc<dyn vfs::IndexNode>, isize> {
    let task = current_task().unwrap();
    Ok(match dirfd {
        AT_FDCWD => task.process.fs().lock().working_inode.inode.clone(),
        fd => {
            let files_ref = task.process.files();
        let fd_table = files_ref.lock();
            fd_table.get_file(fd).map_err(|e| -(e as isize))?.inode.clone()
        }
    })
}

/// cwd 路径规范化：处理绝对/相对 chdir，处理 .  ..  //  trailing/
fn normalize_cwd(old_path: &str, new_path: &str) -> alloc::string::String {
    let mut parts: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
    if !new_path.starts_with('/') {
        for p in old_path.split('/') {
            if !p.is_empty() { parts.push(p); }
        }
    }
    for p in new_path.split('/') {
        match p {
            "" | "." => {}
            ".." => { parts.pop(); }
            _ => parts.push(p),
        }
    }
    if parts.is_empty() {
        alloc::string::String::from("/")
    } else {
        let mut s = alloc::string::String::with_capacity(parts.iter().map(|p| p.len()).sum::<usize>() + parts.len());
        for p in parts {
            s.push('/');
            s.push_str(p);
        }
        s
    }
}

fn user_cstring(token: usize, ptr: *const u8) -> Result<String, isize> {
    UserCString::new(ptr).read(token)
}

fn validate_path_len(path: &str) -> Result<(), isize> {
    if path.len() >= vfs::MAX_PATHLEN {
        return Err(ENAMETOOLONG);
    }
    if path
        .split('/')
        .any(|component| component.len() > vfs::NAME_MAX)
    {
        return Err(ENAMETOOLONG);
    }
    Ok(())
}

fn open_subject_ids() -> (u32, u32) {
    let task = current_task().unwrap();
    let inner = task.acquire_inner_lock();
    (inner.fsuid, inner.fsgid)
}

fn open_requests_write(flags: OpenFlags) -> bool {
    let access_mode = flags.bits() & OPEN_ACCMODE_MASK;
    access_mode == OpenFlags::O_WRONLY.bits()
        || access_mode == OpenFlags::O_RDWR.bits()
        || flags.contains(OpenFlags::O_TRUNC)
}

fn has_directory_write_search_access(meta: &vfs::Metadata, uid: u32, gid: u32) -> bool {
    if uid == 0 {
        return true;
    }
    (permission_class_bits(meta, uid, gid) & 0o3) == 0o3
}

fn apply_created_inode_metadata(
    parent_meta: &vfs::Metadata,
    inode: &Arc<dyn vfs::IndexNode>,
    mode: vfs::InodeMode,
    uid: u32,
    gid: u32,
) -> Result<(), isize> {
    let mut meta = inode.metadata().map_err(|e| -(e as isize))?;
    let parent_setgid = parent_meta.mode.contains(vfs::InodeMode::S_ISGID);
    let child_gid = if parent_setgid { parent_meta.gid } else { gid };

    meta.uid = uid;
    meta.gid = child_gid;
    meta.mode = vfs::InodeMode::from(meta.file_type) | (mode & vfs::InodeMode::S_IALLUGO);
    if meta.mode.contains(vfs::InodeMode::S_ISGID) && uid != 0 && gid != child_gid {
        meta.mode.remove(vfs::InodeMode::S_ISGID);
    }

    match inode.set_metadata(&meta) {
        Ok(()) | Err(SyscallErr::ENOSYS) => Ok(()),
        Err(e) => Err(-(e as isize)),
    }
}

fn metadata_to_stat(meta: &vfs::Metadata) -> Stat {
    Stat {
        st_dev: meta.dev_id as u64,
        st_ino: meta.inode_id as u64,
        st_mode: meta.mode.bits() | vfs::InodeMode::from(meta.file_type).bits(),
        st_nlink: meta.nlinks as u32,
        st_uid: meta.uid,
        st_gid: meta.gid,
        st_rdev: meta.raw_dev as u64,
        __pad: 0,
        st_size: meta.size,
        st_blksize: meta.blk_size as u32,
        __pad2: 0,
        st_blocks: meta.blocks as u64,
        st_atime: meta.atime,
        st_mtime: meta.mtime,
        st_ctime: meta.ctime,
        __unused: 0,
    }
}

fn metadata_to_statx(meta: &vfs::Metadata, mask: u32) -> Statx {
    let stat = metadata_to_stat(meta);
    let mut statx = Statx::new(
        mask,
        stat.get_nlink(),
        stat.get_mode() as u16,
        stat.get_ino() as u64,
        stat.get_size() as u64,
        stat.get_atime() as i64,
        stat.get_ctime() as i64,
        stat.get_mtime() as i64,
        ((stat.get_rdev() & 0xffff_00) >> 8) as u32,
        (stat.get_rdev() & 0xff) as u32,
        ((stat.get_dev() & 0xffff_00) >> 8) as u32,
        (stat.get_dev() & 0xff) as u32,
    );
    statx.stx_uid = stat.st_uid;
    statx.stx_gid = stat.st_gid;
    statx
}

fn open_file_at(
    dirfd: usize,
    path: &str,
    flags: OpenFlags,
    mode: vfs::InodeMode,
) -> Result<vfs::File, isize> {
    let start = resolve_start_inode(dirfd)?;
    if path.is_empty() {
        let md = start.metadata().map_err(|e| -(e as isize))?;
        return vfs::File::new_without_open(start, _open_flags_to_vfs_flags(flags), md.file_type)
            .try_clone()
            .ok_or(ENOMEM);
    }

    let (uid, gid) = open_subject_ids();
    let parent_result = check_parent_search_access(&start, path, uid, gid);
    if parent_result != SUCCESS {
        return Err(parent_result);
    }

    let follow_final = !flags.contains(OpenFlags::O_NOFOLLOW);
    match vfs_lookup(&start, path, follow_final) {
        Ok(target) => {
            if !follow_final {
                if let Ok(md) = target.metadata() {
                    if md.file_type == FileType::SymLink {
                        return Err(ELOOP);
                    }
                }
            }
            if flags.contains(OpenFlags::O_CREAT | OpenFlags::O_EXCL) {
                return Err(EEXIST);
            }
            let md = target.metadata().map_err(|e| -(e as isize))?;
            if md.file_type == FileType::Dir
                && (flags.contains(OpenFlags::O_WRONLY) || flags.contains(OpenFlags::O_RDWR))
            {
                return Err(EISDIR);
            }
            if md.file_type != FileType::Dir && flags.contains(OpenFlags::O_DIRECTORY) {
                return Err(ENOTDIR);
            }
            if open_requests_write(flags) {
                if is_executable_inode_busy(&target) {
                    return Err(ETXTBSY);
                }
                if !has_final_access(&md, FaccessatMode::W_OK, uid, gid) {
                    return Err(EACCES);
                }
            }
            if flags.contains(OpenFlags::O_TRUNC) {
                target.resize(0).map_err(|e| -(e as isize))?;
            }
            vfs::File::new(target, _open_flags_to_vfs_flags(flags)).map_err(|e| -(e as isize))
        }
        Err(errno) if errno == ENOENT => {
            if !flags.contains(OpenFlags::O_CREAT) || flags.contains(OpenFlags::O_DIRECTORY) {
                return Err(errno);
            }
            let (parent, leaf) = vfs_lookup_parent_for_start(&start, path)?;
            let parent_meta = parent.metadata().map_err(|e| -(e as isize))?;
            check_parent_write_search_access(&parent, uid, gid)?;
            let inode = parent
                .create(&leaf, FileType::File, mode & vfs::InodeMode::S_IALLUGO)
                .map_err(|e| -(e as isize))?;
            apply_created_inode_metadata(&parent_meta, &inode, mode, uid, gid)?;
            vfs::File::new(inode, _open_flags_to_vfs_flags(flags)).map_err(|e| -(e as isize))
        }
        Err(errno) => Err(errno),
    }
}

fn read_into_user(file: &vfs::File, token: usize, buf: usize, count: usize) -> isize {
    let mut kernel_buf = alloc::vec![0u8; count];
    let n = match file.read(&mut kernel_buf) {
        Ok(n) => n,
        Err(e) => return -(e as isize),
    };
    let mut writer = match UserBufferWriter::new(token, buf as *mut u8, n) {
        Ok(writer) => writer,
        Err(errno) => return errno,
    };
    // Temporary: check kernel_buf content before copy-out
    if n >= 60 {
        log::info!(
            "[read_into_user] count={} n={} kbuf[50..60]={:02x?}",
            count, n, &kernel_buf[50..60]
        );
    }
    match writer.write_from(&kernel_buf[..n]) {
        Ok(m) => {
            if m != n {
                warn!("read_into_user: copy-out {} bytes, expected {}", m, n);
            }
            // Temporary: read back user buffer to check for TLB incoherence
            if n >= 60 {
                if let Ok(reader) = UserBufferReader::new(token, buf as *const u8, n) {
                    let readback = reader.read_to_vec(512).unwrap_or_default();
                    if readback.len() >= 60 {
                        log::info!(
                            "[read_into_user] READBACK[50..60]={:02x?} kbuf[50..60]={:02x?} match={}",
                            &readback[50..60],
                            &kernel_buf[50..60],
                            readback[50..60] == kernel_buf[50..60],
                        );
                    }
                }
            }
            m as isize
        }
        Err(errno) => errno,
    }
}

fn pread_into_user(file: &vfs::File, token: usize, buf: usize, count: usize, offset: usize) -> isize {
    let mut kernel_buf = alloc::vec![0u8; count];
    let n = match file.pread(offset, &mut kernel_buf) {
        Ok(n) => n,
        Err(e) => return -(e as isize),
    };
    let mut writer = match UserBufferWriter::new(token, buf as *mut u8, n) {
        Ok(writer) => writer,
        Err(errno) => return errno,
    };
    match writer.write_from(&kernel_buf[..n]) {
        Ok(m) => {
            if m != n {
                warn!("pread_into_user: copy-out {} bytes, expected {}", m, n);
            }
            m as isize
        }
        Err(errno) => errno,
    }
}

fn write_from_user(file: &vfs::File, token: usize, buf: usize, count: usize) -> isize {
    let reader = match UserBufferReader::new(token, buf as *const u8, count) {
        Ok(reader) => reader,
        Err(errno) => return errno,
    };
    let kernel_buf = match reader.read_to_vec(MAX_SYSCALL_BUFFER_SIZE) {
        Ok(buf) => buf,
        Err(errno) => return errno,
    };
    match file.write(&kernel_buf) {
        Ok(n) => n as isize,
        Err(e) => -(e as isize),
    }
}

fn pwrite_from_user(file: &vfs::File, token: usize, buf: usize, count: usize, offset: usize) -> isize {
    let reader = match UserBufferReader::new(token, buf as *const u8, count) {
        Ok(reader) => reader,
        Err(errno) => return errno,
    };
    let kernel_buf = match reader.read_to_vec(MAX_SYSCALL_BUFFER_SIZE) {
        Ok(buf) => buf,
        Err(errno) => return errno,
    };
    match file.pwrite(offset, &kernel_buf) {
        Ok(n) => n as isize,
        Err(e) => -(e as isize),
    }
}

// todo
pub fn sys_splice(
    fd_in: usize,
    off_in: *mut usize,
    fd_out: usize,
    off_out: *mut usize,
    len: usize,
    _flags: u32,
) -> isize {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
        let fd_table = files_ref.lock();
    let in_file = match fd_table.get_file(fd_in) {
        Ok(file) => match file.try_clone() {
            Some(f) => f,
            None => return EBADF,
        },
        Err(e) => return -(e as isize),
    };
    let out_file = match fd_table.get_file(fd_out) {
        Ok(file) => match file.try_clone() {
            Some(f) => f,
            None => return EBADF,
        },
        Err(e) => return -(e as isize),
    };
    drop(fd_table);

    info!("[sys_splice] outfd: {}, in_fd: {}", fd_out, fd_in);
    if in_file.readable().is_err() || out_file.writable().is_err() {
        return EBADF;
    }
    info!("[sys_splice] off_in: {:?}, off_out: {:?}", off_in, off_out);
    // a buffer in kernel
    const BUFFER_SIZE: usize = 4096;
    let mut buffer = Vec::<u8>::with_capacity(BUFFER_SIZE);
    let mut buffer_ptr: Option<&[u8]> = None;

    let token = task.get_user_token();
    let mut off_in_val = if off_in.is_null() {
        None
    } else {
        match UserPtrMut::new(off_in).read(token) {
            Ok(offset) => Some(offset),
            Err(errno) => return errno,
        }
    };
    let mut off_out_val = if off_out.is_null() {
        None
    } else {
        match UserPtrMut::new(off_out).read(token) {
            Ok(offset) => Some(offset),
            Err(errno) => return errno,
        }
    };

    let mut left_bytes = len;
    loop {
        let write_buffer = match buffer_ptr {
            Some(buffer_ptr) => buffer_ptr,
            None => {
                unsafe {
                    buffer.set_len(left_bytes.min(BUFFER_SIZE));
                }
                let read_size = {
                    if let Some(off_val) = off_in_val.as_mut() {
                        let n = match in_file.inode.read_at(
                            *off_val,
                            buffer.len(),
                            buffer.as_mut_slice(),
                            in_file.private_data(),
                        ) {
                            Ok(n) => n,
                            Err(e) => return -(e as isize),
                        };
                        *off_val += n;
                        n
                    } else {
                        match in_file.read(buffer.as_mut_slice()) {
                            Ok(n) => n,
                            Err(e) => return -(e as isize),
                        }
                    }
                };
                if read_size == 0 {
                    break;
                }
                unsafe {
                    buffer.set_len(read_size);
                }
                buffer.as_slice()
            }
        };

        let read_size = write_buffer.len();

        let write_size = {
            if let Some(off_val) = off_out_val.as_mut() {
                let n = match out_file.inode.write_at(
                    *off_val,
                    write_buffer.len(),
                    write_buffer,
                    out_file.private_data(),
                ) {
                    Ok(n) => n,
                    Err(e) => return -(e as isize),
                };
                *off_val += n;
                n
            } else {
                match out_file.write(write_buffer) {
                    Ok(n) => n,
                    Err(e) => return -(e as isize),
                }
            }
        };
        if write_size == 0 {
            break;
        }

        buffer_ptr = if write_size < read_size {
            Some(&write_buffer[write_size..])
        } else {
            None
        };
        left_bytes -= write_size;
    }
    let send_size = len - left_bytes;
    if let Some(offset) = off_in_val {
        if UserPtrMut::new(off_in).write(token, &offset).is_err() {
            return EFAULT;
        }
    }
    if let Some(offset) = off_out_val {
        if UserPtrMut::new(off_out).write(token, &offset).is_err() {
            return EFAULT;
        }
    }
    info!("[sys_sendfile] send bytes: {}", send_size);
    send_size as isize
}

/// # Warning
/// `fs` & `files` is locked in this function
fn __openat(dirfd: usize, path: &str) -> Result<vfs::File, isize> {
    open_file_at(dirfd, path, OpenFlags::O_RDONLY, vfs::InodeMode::S_IRWXUGO)
}

pub fn sys_getcwd(buf: usize, size: usize) -> isize {
    let task = current_task().unwrap();
    let fs_ref = task.process.fs();
    let fs_lock = fs_ref.lock();
    let working_dir = fs_lock.working_path.clone();
    drop(fs_lock);
    // ERANGE must be checked BEFORE buffer validation:
    // Linux returns ERANGE if buffer is too small, even if buf is partially invalid
    if working_dir.len() + 1 > size {
        return ERANGE;
    }
    let vm_ref = task.process.vm();
    if !vm_ref
        .lock()
        .contains_valid_buffer(buf, size, MapPermission::W)
    {
        return EFAULT;
    }
    let token = task.get_user_token();
    let write_len = working_dir.len() + 1;
    let mut user_buf = match UserBufferWriter::new(token, buf as *mut u8, write_len) {
        Ok(writer) => writer,
        Err(errno) => return errno,
    };
    let mut cwd = Vec::with_capacity(write_len);
    cwd.extend_from_slice(working_dir.as_bytes());
    cwd.push(0);
    if let Err(errno) = user_buf.write_from(&cwd) {
        return errno;
    }
    buf as isize
}

pub fn sys_lseek(fd: usize, offset: isize, whence: u32) -> isize {
    // whence is not valid
    let whence = match SeekWhence::from_bits(whence) {
        Some(whence) => whence,
        None => {
            warn!("[sys_lseek] unknown flags");
            return EINVAL;
        }
    };
    info!(
        "[sys_lseek] fd: {}, offset: {}, whence: {:?}",
        fd, offset, whence,
    );
    let task = current_task().unwrap();
    let files_ref = task.process.files();
        let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    let seek_from = match whence.bits() {
        0 => SeekFrom::SeekSet(offset as i64),
        2 => SeekFrom::SeekEnd(offset as i64),
        _ => SeekFrom::SeekCurrent(offset as i64),
    };
    match file.lseek(seek_from) {
        Ok(pos) => pos as isize,
        Err(e) => -(e as isize),
    }
}

pub fn sys_read(fd: usize, buf: usize, count: usize) -> isize {
    let count = count.min(MAX_SYSCALL_BUFFER_SIZE);
    let task = current_task().unwrap();
    let file = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        match fd_table.get_file(fd) {
            Ok(fd_ref) => match fd_ref.try_clone() { Some(f) => f, None => return EBADF, },
            Err(e) => return -(e as isize),
        }
    };
    if file.readable().is_err() {
        return EBADF;
    }
    let is_nonblock = file.is_nonblock();
    let token = task.get_user_token();
    if is_nonblock {
        read_into_user(&file, token, buf, count)
    } else if let Some(wq) = file.inode.read_wait_queue() {
        match WaitQueue::wait_until_interruptible(wq, || {
            let ret = read_into_user(&file, token, buf, count);
            if ret == -(SyscallErr::EAGAIN as isize) { None } else { Some(ret) }
        }) {
            WaitResult::Ready(n) => n,
            WaitResult::Interrupted => -(SyscallErr::ERESTART as isize),
            WaitResult::TimedOut => -(SyscallErr::EAGAIN as isize),
        }
    } else {
        // Fallback: regular files and legacy File implementations may not expose a WaitQueue yet.
        wait_io_core(|| read_into_user(&file, token, buf, count), is_nonblock)
    }
}

pub fn sys_write(fd: usize, buf: usize, count: usize) -> isize {
    let count = count.min(MAX_SYSCALL_BUFFER_SIZE);
    let task = current_task().unwrap();
    let file = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        match fd_table.get_file(fd) {
            Ok(fd_ref) => match fd_ref.try_clone() { Some(f) => f, None => return EBADF, },
            Err(e) => return -(e as isize),
        }
    };
    if file.writable().is_err() {
        return EBADF;
    }
    let is_nonblock = file.is_nonblock();
    let token = task.get_user_token();
    if is_nonblock {
        write_from_user(&file, token, buf, count)
    } else if let Some(wq) = file.inode.write_wait_queue() {
        match WaitQueue::wait_until_interruptible(wq, || {
            let ret = write_from_user(&file, token, buf, count);
            if ret == -(SyscallErr::EAGAIN as isize) { None } else { Some(ret) }
        }) {
            WaitResult::Ready(n) => n,
            WaitResult::Interrupted => -(SyscallErr::ERESTART as isize),
            WaitResult::TimedOut => -(SyscallErr::EAGAIN as isize),
        }
    } else {
        // Fallback: regular files and legacy File implementations may not expose a WaitQueue yet.
        wait_io_core(|| write_from_user(&file, token, buf, count), is_nonblock)
    }
}

pub fn sys_pread(fd: usize, buf: usize, count: usize, offset: usize) -> isize {
    let count = count.min(MAX_SYSCALL_BUFFER_SIZE);
    let task = current_task().unwrap();
    let files_ref = task.process.files();
        let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    // fd is not open for reading
    if file.readable().is_err() {
        return EBADF;
    }
    let token = task.get_user_token();
    pread_into_user(&file, token, buf, count, offset)
}

pub fn sys_pwrite(fd: usize, buf: usize, count: usize, offset: usize) -> isize {
    let count = count.min(MAX_SYSCALL_BUFFER_SIZE);
    let task = current_task().unwrap();
    let files_ref = task.process.files();
        let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    // fd is not open for writing
    if file.writable().is_err() {
        return EBADF;
    }
    let token = task.get_user_token();
    pwrite_from_user(&file, token, buf, count, offset)
}

pub fn sys_readv(fd: usize, iov: usize, iovcnt: usize) -> isize {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
        let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    // fd is not open for reading
    if file.readable().is_err() {
        return EBADF;
    }
    let token = task.get_user_token();
    let user_iov = match UserIoVec::read_user_iovecs(
        token,
        iov as *const crate::fs::iov::IOVec,
        iovcnt,
        MAX_SYSCALL_BUFFER_SIZE,
    ) {
        Ok(iov) => iov,
        Err(errno) => return errno,
    };
    let user_buf = match user_iov.writer_buffer() {
        Ok(buffer) => buffer,
        Err(errno) => return errno,
    };
    let count = user_buf.len();
    let mut kernel_buf = alloc::vec![0u8; count];
    let n = match file.read(&mut kernel_buf) {
        Ok(n) => n,
        Err(e) => return -(e as isize),
    };
    let mut user_buf = user_buf;
    user_buf.write(&kernel_buf[..n]);
    n as isize
}

pub fn sys_writev(fd: usize, iov: usize, iovcnt: usize) -> isize {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
        let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    // fd is not open for writing
    if file.writable().is_err() {
        return EBADF;
    }
    let token = task.get_user_token();
    let user_iov = match UserIoVec::read_user_iovecs(
        token,
        iov as *const crate::fs::iov::IOVec,
        iovcnt,
        MAX_SYSCALL_BUFFER_SIZE,
    ) {
        Ok(iov) => iov,
        Err(errno) => return errno,
    };
    let user_buf = match user_iov.reader_buffer() {
        Ok(buffer) => buffer,
        Err(errno) => return errno,
    };
    let mut kernel_buf = alloc::vec![0u8; user_buf.len()];
    user_buf.read(&mut kernel_buf);
    match file.write(&kernel_buf) {
        Ok(n) => n as isize,
        Err(e) => -(e as isize),
    }
}

/// If offset is not NULL, then it points to a variable holding the
/// file offset from which sendfile() will start reading data from
/// in_fd.
///
/// When sendfile() returns,
/// this variable will be set to the offset of the byte following
/// the last byte that was read.
///
/// If offset is not NULL, then sendfile() does not modify the file
/// offset of in_fd; otherwise the file offset is adjusted to reflect
/// the number of bytes read from in_fd.
///
/// If offset is NULL, then data will be read from in_fd starting at
/// the file offset, and the file offset will be updated by the call.
pub fn sys_sendfile(out_fd: usize, in_fd: usize, offset: *mut usize, count: usize) -> isize {
    let count = count.min(64 * 1024 * 1024);
    let task = current_task().unwrap();
    let files_ref = task.process.files();
        let fd_table = files_ref.lock();
    let in_file = match fd_table.get_file(in_fd) {
        Ok(file) => match file.try_clone() { Some(f) => f, None => return EBADF, },
        Err(e) => return -(e as isize),
    };
    let out_file = match fd_table.get_file(out_fd) {
        Ok(file) => match file.try_clone() { Some(f) => f, None => return EBADF, },
        Err(e) => return -(e as isize),
    };
    drop(fd_table);

    info!("[sys_sendfile] outfd: {}, in_fd: {}", out_fd, in_fd);
    if in_file.readable().is_err() || out_file.writable().is_err() {
        return EBADF;
    }

    let token = task.get_user_token();
    let mut offset_val = if offset.is_null() {
        None
    } else {
        match UserPtrMut::new(offset).read(token) {
            Ok(offset) => Some(offset),
            Err(errno) => return errno,
        }
    };

    // a buffer in kernel
    const BUFFER_SIZE: usize = 4096;
    let mut buffer = Vec::<u8>::with_capacity(BUFFER_SIZE);
    let mut buffer_ptr: Option<&[u8]> = None;

    let mut left_bytes = count;
    loop {
        let write_buffer = match buffer_ptr {
            Some(buffer_ptr) => buffer_ptr,
            None => {
                unsafe {
                    buffer.set_len(left_bytes.min(BUFFER_SIZE));
                }
                let read_size = {
                    if let Some(off_val) = offset_val.as_mut() {
                        let n = match in_file.inode.read_at(
                            *off_val,
                            buffer.len(),
                            buffer.as_mut_slice(),
                            in_file.private_data(),
                        ) {
                            Ok(n) => n,
                            Err(e) => return -(e as isize),
                        };
                        *off_val += n;
                        n
                    } else {
                        match in_file.read(buffer.as_mut_slice()) {
                            Ok(n) => n,
                            Err(e) => return -(e as isize),
                        }
                    }
                };
                if read_size == 0 {
                    break;
                }
                unsafe {
                    buffer.set_len(read_size);
                }
                buffer.as_slice()
            }
        };

        let read_size = write_buffer.len();

        let mut fallback = |redundant_bytes: usize| {
            match offset_val.as_mut() {
                Some(offset) => *offset -= redundant_bytes,
                None => match in_file.lseek(SeekFrom::SeekCurrent(-(redundant_bytes as i64))) {
                    Ok(_) => {}
                    Err(errno) => log::error!("splice fallback lseek failed: errno {:?}", errno),
                },
            }
        };

        let write_size = match out_file.write(write_buffer) {
            Ok(n) => n,
            Err(e) => {
                fallback(read_size);
                return -(e as isize);
            }
        };
        if write_size == 0 {
            fallback(read_size);
            break;
        }

        buffer_ptr = if write_size < read_size {
            Some(&write_buffer[write_size..])
        } else {
            None
        };
        left_bytes -= write_size;
    }
    let send_size = count - left_bytes;
    if let Some(offset_value) = offset_val {
        if UserPtrMut::new(offset).write(token, &offset_value).is_err() {
            return EFAULT;
        }
    }
    info!("[sys_sendfile] send bytes: {}", send_size);
    send_size as isize
}

pub fn sys_copy_file_range(
    fd_in: usize,
    off_in: *mut usize,
    fd_out: usize,
    off_out: *mut usize,
    len: usize,
    flags: u32,
) -> isize {
    if flags != 0 {
        return EINVAL;
    }
    let len = len.min(64 * 1024 * 1024);
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let in_file = match fd_table.get_file(fd_in) {
        Ok(file) => match file.try_clone() { Some(f) => f, None => return EBADF, },
        Err(e) => return -(e as isize),
    };
    let out_file = match fd_table.get_file(fd_out) {
        Ok(file) => match file.try_clone() { Some(f) => f, None => return EBADF, },
        Err(e) => return -(e as isize),
    };
    drop(fd_table);

    if in_file.readable().is_err() || out_file.writable().is_err() {
        return EBADF;
    }

    let token = task.get_user_token();
    let mut in_offset = if off_in.is_null() {
        None
    } else {
        match UserPtrMut::new(off_in).read(token) {
            Ok(offset) => Some(offset),
            Err(errno) => return errno,
        }
    };
    let mut out_offset = if off_out.is_null() {
        None
    } else {
        match UserPtrMut::new(off_out).read(token) {
            Ok(offset) => Some(offset),
            Err(errno) => return errno,
        }
    };

    const BUFFER_SIZE: usize = 4096;
    let mut buffer = Vec::<u8>::with_capacity(BUFFER_SIZE);
    let mut copied = 0usize;

    while copied < len {
        let chunk = (len - copied).min(BUFFER_SIZE);
        unsafe { buffer.set_len(chunk); }

        let read_size = if let Some(offset) = in_offset {
            match in_file.pread(offset, buffer.as_mut_slice()) {
                Ok(n) => n,
                Err(e) => return -(e as isize),
            }
        } else {
            match in_file.read(buffer.as_mut_slice()) {
                Ok(n) => n,
                Err(e) => return -(e as isize),
            }
        };
        if read_size == 0 {
            break;
        }
        unsafe { buffer.set_len(read_size); }

        let write_size = if let Some(offset) = out_offset {
            match out_file.pwrite(offset, buffer.as_slice()) {
                Ok(n) => n,
                Err(e) => {
                    if in_offset.is_none() {
                        let _ = in_file.lseek(SeekFrom::SeekCurrent(-(read_size as i64)));
                    }
                    return -(e as isize);
                }
            }
        } else {
            match out_file.write(buffer.as_slice()) {
                Ok(n) => n,
                Err(e) => {
                    if in_offset.is_none() {
                        let _ = in_file.lseek(SeekFrom::SeekCurrent(-(read_size as i64)));
                    }
                    return -(e as isize);
                }
            }
        };

        if write_size == 0 {
            if in_offset.is_none() {
                let _ = in_file.lseek(SeekFrom::SeekCurrent(-(read_size as i64)));
            }
            break;
        }

        if let Some(offset) = in_offset.as_mut() {
            *offset += write_size;
        } else if write_size < read_size {
            let _ = in_file.lseek(SeekFrom::SeekCurrent(-((read_size - write_size) as i64)));
        }
        if let Some(offset) = out_offset.as_mut() {
            *offset += write_size;
        }

        copied += write_size;
        if write_size < read_size {
            break;
        }
    }

    if let Some(offset) = in_offset {
        if UserPtrMut::new(off_in).write(token, &offset).is_err() {
            return EFAULT;
        }
    }
    if let Some(offset) = out_offset {
        if UserPtrMut::new(off_out).write(token, &offset).is_err() {
            return EFAULT;
        }
    }

    copied as isize
}

pub fn sys_close(fd: usize) -> isize {
    info!("[sys_close] fd: {}", fd);
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();
    match fd_table.drop_fd(fd) {
        Ok(_) => SUCCESS,
        Err(e) => return -(e as isize),
    }
}

pub fn sys_close_range(first: usize, last: usize, flags: u32) -> isize {
    const CLOSE_RANGE_UNSHARE: u32 = 1 << 1;
    const CLOSE_RANGE_CLOEXEC: u32 = 1 << 2;
    const VALID_FLAGS: u32 = CLOSE_RANGE_UNSHARE | CLOSE_RANGE_CLOEXEC;

    if first > last || (flags & !VALID_FLAGS) != 0 {
        return EINVAL;
    }

    let task = current_task().unwrap();
    let files_ref = if (flags & CLOSE_RANGE_UNSHARE) != 0 {
        match task.process.unshare_files() {
            Ok(files) => files,
            Err(e) => return -(e as isize),
        }
    } else {
        task.process.files()
    };
    let mut fd_table = files_ref.lock();
    if (flags & CLOSE_RANGE_CLOEXEC) != 0 {
        fd_table.set_cloexec_range(first, last);
    } else {
        fd_table.close_range(first, last);
    }
    SUCCESS
}

/// # Warning
/// Only O_CLOEXEC is supported now
pub fn sys_pipe2(pipefd: usize, flags: u32) -> isize {
    const VALID_FLAGS: OpenFlags = OpenFlags::from_bits_truncate(
        0o2000000 /* O_CLOEXEC */ | 0o40000 /* O_DIRECT */ | 0o4000, /* O_NONBLOCK */
    );
    let flags = match OpenFlags::from_bits(flags) {
        Some(flags) => {
            // only O_CLOEXEC | O_DIRECT | O_NONBLOCK are valid in pipe2()
            if flags.difference(VALID_FLAGS).is_empty() {
                flags
            } else {
                // some flags are invalid in pipe2(), they are all valid OpenFlags though
                warn!(
                    "[sys_pipe2] invalid flags: {:?}",
                    flags.difference(VALID_FLAGS)
                );
                return EINVAL;
            }
        }
        None => {
            // contains invalid OpenFlags
            warn!("[sys_pipe2] unknown flags");
            return EINVAL;
        }
    };
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();
    let (pipe_read, pipe_write) = make_pipe();
    let cloexec = flags.contains(OpenFlags::O_CLOEXEC);
    let nonblock = flags.contains(OpenFlags::O_NONBLOCK);
    let vf_read = vfs::File::new_without_open(
        pipe_read,
        vfs::FileFlags::O_RDONLY | if nonblock { vfs::FileFlags::O_NONBLOCK } else { vfs::FileFlags::empty() },
        vfs::FileType::Pipe,
    );
    let read_fd = match fd_table.alloc_fd(vf_read, cloexec) {
        Ok(fd) => fd,
        Err(e) => return -(e as isize),
    };
    let vf_write = vfs::File::new_without_open(
        pipe_write,
        vfs::FileFlags::O_WRONLY | if nonblock { vfs::FileFlags::O_NONBLOCK } else { vfs::FileFlags::empty() },
        vfs::FileType::Pipe,
    );
    let write_fd = match fd_table.alloc_fd(vf_write, cloexec) {
        Ok(fd) => fd,
        Err(e) => {
            let _ = fd_table.drop_fd(read_fd);
            return -(e as isize);
        }
    };

    let token = task.get_user_token();
    let fds = [read_fd as u32, write_fd as u32];
    if UserSlice::new(pipefd as *const u32, 2)
        .write_array_from(token, &fds)
        .is_err()
    {
        log::error!("[sys_pipe2] Failed to copy to {:?}", pipefd);
        let _ = fd_table.drop_fd(read_fd);
        let _ = fd_table.drop_fd(write_fd);
        return EFAULT;
    };
    info!(
        "[sys_pipe2] read_fd: {}, write_fd: {}, flags: {:?}",
        read_fd, write_fd, flags
    );
    SUCCESS
}

/// 系统调用sys_getdents64
/// # 说明
/// + 用于获取目录项
/// # 参数
/// + fd：文件描述符
/// + dirp：用于存储获取到的目录项的指针
/// + count：要获取的目录项的数量
/// # 返回值
/// + 成功：返回获取的目录项数量
/// + 失败：返回错误码
pub fn sys_getdents64(fd: usize, dirp: *mut u8, count: usize) -> isize {
    // 防御性限制：单次 getdents64 最多返回 128KB 的目录项，防止超大 Vec 分配导致内核堆 OOM
    let count = count.min(128 * 1024);
    let task = current_task().unwrap();
    let token = task.get_user_token();

    // 获取文件描述符
    let file = match fd {
        AT_FDCWD => match task.process.fs().lock().working_inode.try_clone() { Some(f) => f, None => return EBADF, },
        fd => {
            let files_ref = task.process.files();
        let fd_table = files_ref.lock();
            match fd_table.get_file(fd) {
                Ok(file) => match file.try_clone() { Some(f) => f, None => return EBADF, },
                Err(e) => return -(e as isize),
            }
        }
    };

    let mut kernel_buf = alloc::vec![0u8; count];
    let written = match file.get_dirent64(&mut kernel_buf) {
        Ok(n) => n,
        Err(errno) => return errno,
    };

    if written == 0 {
        return 0;
    }

    let mut writer = match UserBufferWriter::new(token, dirp, written) {
        Ok(w) => w,
        Err(_) => return EFAULT,
    };
    if writer.write_from(&kernel_buf[..written]).is_err() {
        log::error!("[sys_getdents64] Failed to copy to {:?}", dirp);
        return EFAULT;
    }
    info!("[sys_getdents64] fd: {}, count: {}", fd, count);
    written as isize
}

pub fn sys_dup(oldfd: usize) -> isize {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();
    let file = match fd_table.get_file(oldfd) {
        Ok(file) => match file.try_clone() { Some(f) => f, None => return EBADF, },
        Err(e) => return -(e as isize),
    };
    let newfd = match fd_table.alloc_fd(file, false) {
        Ok(fd) => fd,
        Err(e) => return -(e as isize),
    };
    info!("[sys_dup] oldfd: {}, newfd: {}", oldfd, newfd);
    newfd as isize
}

pub fn sys_dup2(oldfd: usize, newfd: usize) -> isize {
    let task = current_task().unwrap();

    let ret = {
        let files_ref = task.process.files();
        let mut fd_table = files_ref.lock();
        if oldfd == newfd {
            return match fd_table.get_file(oldfd) {
                Ok(_) => oldfd as isize,
                Err(e) => -(e as isize),
            };
        }
        let file = match fd_table.get_file(oldfd) {
            Ok(file) => match file.try_clone() { Some(f) => f, None => return EBADF, },
            Err(e) => return -(e as isize),
        };

        match fd_table.alloc_fd_at(newfd, file, false) {
            Ok(fd) => fd as isize,
            Err(e) => -(e as isize),
        }
    };
    if ret < 0 {
        return ret;
    }
    info!("[sys_dup2] oldfd: {}, newfd: {}", oldfd, newfd);
    newfd as isize
}

pub fn sys_dup3(oldfd: usize, newfd: usize, flags: u32) -> isize {
    info!(
        "[sys_dup3] oldfd: {}, newfd: {}, flags: {:X}",
        oldfd, newfd, flags
    );
    if oldfd == newfd {
        return EINVAL;
    }
    // Only O_CLOEXEC is valid for dup3; use direct bit check
    const O_CLOEXEC: u32 = 0o2000000;
    if flags & !O_CLOEXEC != 0 {
        warn!("[sys_dup3] invalid flags: {:X}", flags);
        return EINVAL;
    }
    let is_cloexec = (flags & O_CLOEXEC) != 0;
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();

    let file = match fd_table.get_file(oldfd) {
        Ok(file) => match file.try_clone() { Some(f) => f, None => return EBADF, },
        Err(e) => return -(e as isize),
    };
    match fd_table.alloc_fd_at(newfd, file, is_cloexec) {
        Ok(fd) => fd as isize,
        Err(e) => -(e as isize),
    }
}

// This syscall is not complete at all, only /read proc/self/exe
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
    if path.is_empty() {
        return ENOENT;
    }
    let real_path = if path.as_str() == "/proc/self/exe" {
        let exe_path = task.process.exe_path();
        if exe_path.is_empty() {
            return ENOENT;
        }
        exe_path
    } else {
        let start = match resolve_start_inode(dirfd) { Ok(s) => s, Err(e) => return e, };

        // 使用新 VFS 路径解析 (不跟随最终符号链接)
        let inode = match vfs_lookup(&start, &path, false) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        };
        let md = match inode.metadata() {
            Ok(md) => md,
            Err(_) => return EINVAL,
        };
        if md.file_type != vfs::FileType::SymLink {
            warn!(
                "[sys_readlinkat] not a symbolic link! dirfd: {}, path: {}",
                dirfd as isize, path
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

bitflags! {
    pub struct FstatatFlags: u32 {
        const AT_EMPTY_PATH = 0x1000;
        const AT_NO_AUTOMOUNT = 0x800;
        const AT_SYMLINK_NOFOLLOW = 0x100;
    }
}

pub fn sys_fstatat(dirfd: usize, path: *const u8, buf: *mut u8, flags: u32) -> isize {
    let token = current_user_token();
    let path = match user_cstring(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    let flags = match FstatatFlags::from_bits(flags) {
        Some(flags) => flags,
        None => {
            warn!("[sys_fstatat] unknown flags");
            return EINVAL;
        }
    };

    info!(
        "[sys_fstatat] dirfd: {}, path: {:?}, flags: {:?}",
        dirfd as isize, path, flags,
    );

    let no_follow = flags.contains(FstatatFlags::AT_SYMLINK_NOFOLLOW);
    let start = match resolve_start_inode(dirfd) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };

    if no_follow {
        // AT_SYMLINK_NOFOLLOW: 使用新 VFS 路径解析
        let inode = match vfs_lookup(&start, &path, false) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        };
        let stat = match inode.metadata() {
            Ok(meta) => metadata_to_stat(&meta),
            Err(e) => return -(e as isize),
        };
        if UserPtrMut::new(buf as *mut Stat).write(token, &stat).is_err() {
            return EFAULT;
        }
        SUCCESS
    } else {
        let inode = match vfs_lookup(&start, &path, true) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        };
        let stat = match inode.metadata() {
            Ok(meta) => metadata_to_stat(&meta),
            Err(e) => return -(e as isize),
        };
        if UserPtrMut::new(buf as *mut Stat).write(token, &stat).is_err() {
            log::error!("[sys_fstatat] Failed to copy to {:?}", buf);
            return EFAULT;
        };
        SUCCESS
    }
}

/// warning: 此函数没有完全实现，没有实现根据mask来填充statx的值，并且没有直接维护statx结构体，通过stat结构体间接实现
pub fn sys_statx(dirfd: usize, path: *const u8, flags: u32, mask: u32, buf: *mut u8) -> isize {
    let token = current_user_token();
    let path = match user_cstring(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    let flags = match FstatatFlags::from_bits(flags) {
        Some(flags) => flags,
        None => {
            warn!("[sys_statx] unknown flags");
            return EINVAL;
        }
    };

    info!(
        "[sys_statx] dirfd: {}, path: {:?}, flags: {:?}",
        dirfd as isize, path, flags,
    );

    let start = match resolve_start_inode(dirfd) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };

    let no_follow = flags.contains(FstatatFlags::AT_SYMLINK_NOFOLLOW);
    if no_follow {
        // AT_SYMLINK_NOFOLLOW: 使用新 VFS 路径解析
        let inode = match vfs_lookup(&start, &path, false) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        };
        let statx = match inode.metadata() {
            Ok(meta) => metadata_to_statx(&meta, mask),
            Err(e) => return -(e as isize),
        };
        if UserPtrMut::new(buf as *mut Statx).write(token, &statx).is_err() {
            return EFAULT;
        }
        SUCCESS
    } else {
        let inode = match vfs_lookup(&start, &path, true) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        };
        let statx = match inode.metadata() {
            Ok(meta) => metadata_to_statx(&meta, mask),
            Err(e) => return -(e as isize),
        };
        if UserPtrMut::new(buf as *mut Statx).write(token, &statx).is_err() {
            log::error!("[sys_statx] Failed to copy to {:?}", buf);
            return EFAULT;
        };
        log::debug!("[sys_statx] statx:\n{:?}", statx);
        SUCCESS
    }
}

pub fn sys_fstat(fd: usize, statbuf: *mut u8) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();

    info!("[sys_fstat] fd: {}", fd);
    let file = match fd {
        AT_FDCWD => match task.process.fs().lock().working_inode.try_clone() { Some(f) => f, None => return EBADF, },
        fd => {
            let files_ref = task.process.files();
        let fd_table = files_ref.lock();
            match fd_table.get_file(fd) {
                Ok(file) => match file.try_clone() { Some(f) => f, None => return EBADF, },
                Err(e) => return -(e as isize),
            }
        }
    };
    let stat = match file.metadata() {
        Ok(meta) => metadata_to_stat(&meta),
        Err(e) => return -(e as isize),
    };
    if UserPtrMut::new(statbuf as *mut Stat).write(token, &stat).is_err() {
        log::error!("[sys_fstat] Failed to copy to {:?}", statbuf);
        return EFAULT;
    };
    SUCCESS
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Statfs {
    /// Type of filesystem
    f_type: usize,
    /// Optimal transfer block size
    f_bsize: usize,
    /// Total data blocks in filesystem
    f_blocks: u64,
    /// Free blocks in filesystem
    f_bfree: u64,
    /// Free blocks available to
    /// unprivileged user
    f_bavail: u64,
    /// Total file nodes in filesystem
    f_files: u64,
    /// Free file nodes in filesystem
    f_ffree: u64,
    /// Filesystem ID
    f_fsid: [i32; 2],
    /// Maximum length of filenames
    f_namelen: usize,
    /// Fragment size (since Linux 2.6)
    f_frsize: usize,
    /// Mount flags of filesystem
    f_flag: usize,
    /// Padding bytes reserved for future use
    f_spare: [usize; 4],
}
/// Fake implement for statfs syscall
pub fn sys_statfs(_path: *const u8, buf: *mut Statfs) -> isize {
    let statfs = Box::new(Statfs {
        f_type: 0xf2f52010,
        f_bsize: BLOCK_SZ,
        f_blocks: 10000,
        f_bfree: 9000,
        f_bavail: 9000,
        f_files: 1000,
        f_ffree: 960,
        f_fsid: [114, 514],
        f_namelen: 256,
        f_frsize: 0,
        f_flag: 0,
        f_spare: [0; 4],
    });
    let token = current_task().unwrap().get_user_token();
    if UserPtrMut::new(buf).write(token, statfs.as_ref()).is_err() {
        log::error!("[sys_statfs] Failed to copy to {:?}", buf);
        return EFAULT;
    };
    SUCCESS
}

pub fn sys_fsync(fd: usize) -> isize {
    let task = current_task().unwrap();

    info!("[sys_fsync] fd: {}", fd);
    let files_ref = task.process.files();
        let fd_table = files_ref.lock();
    let inode = match fd_table.get_file(fd) {
        Ok(file) => file.inode.clone(),
        Err(e) => return -(e as isize),
    };
    drop(fd_table);
    match inode.sync() {
        Ok(()) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

pub fn sys_sync() -> isize {
    crate::fs::flush_all_page_caches();
    // Also flush ext4 metadata cache and dirty inodes
    let guard = crate::fs::ext4::ext4fs::GLOBAL_EXT4FS.lock();
    if let Some(fs) = guard.as_ref().and_then(|w| w.upgrade()) {
        fs.flush_metadata_cache();
    }
    SUCCESS
}

pub fn sys_syncfs(_fd: usize) -> isize {
    ENOSYS
}

pub fn sys_fchmodat(dirfd: usize, path: *const u8, mode: u32, _flags: u32) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path_str = match UserCString::from_addr(path as usize).read(token) {
        Ok(s) => s,
        Err(_) => return EFAULT,
    };
    if let Err(errno) = validate_path_len(&path_str) {
        return errno;
    }
    if path_str.is_empty() {
        return ENOENT;
    }
    let inode = if path_str.starts_with('/') {
        match vfs_lookup_absolute(&path_str) {
            Ok(inode) => inode,
            Err(e) => return e,
        }
    } else {
        let start = match resolve_start_inode(dirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        };
        match vfs_lookup(&start, &path_str, true) {
            Ok(inode) => inode,
            Err(e) => return e,
        }
    };
    let new_mode = vfs::InodeMode::from_bits_truncate(mode);
    let mut meta = match inode.metadata() {
        Ok(m) => m,
        Err(e) => return -(e as isize),
    };
    let file_type = meta.mode & vfs::InodeMode::S_IFMT;
    meta.mode = file_type | (new_mode & vfs::InodeMode::S_IALLUGO);
    match inode.set_metadata(&meta) {
        Ok(()) => 0,
        Err(e) => -(e as isize),
    }
}

pub fn sys_fchmod(fd: usize, mode: u32) -> isize {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    let new_mode = vfs::InodeMode::from_bits_truncate(mode);
    let mut meta = match file.inode.metadata() {
        Ok(m) => m,
        Err(e) => return -(e as isize),
    };
    let file_type = meta.mode & vfs::InodeMode::S_IFMT;
    meta.mode = file_type | (new_mode & vfs::InodeMode::S_IALLUGO);
    match file.inode.set_metadata(&meta) {
        Ok(()) => 0,
        Err(e) => -(e as isize),
    }
}

pub fn sys_chmod(path: *const u8, mode: u32) -> isize {
    sys_fchmodat(crate::syscall::AT_FDCWD, path, mode, 0)
}

bitflags! {
    pub struct FchownatFlags: u32 {
        const AT_SYMLINK_NOFOLLOW = 0x100;
        const AT_NO_AUTOMOUNT = 0x800;
        const AT_EMPTY_PATH = 0x1000;
    }
}

pub fn sys_fchownat(
    dirfd: usize,
    path: *const u8,
    owner: u32,
    group: u32,
    flags: u32,
) -> isize {
    const CHOWN_ID_NO_CHANGE: u32 = u32::MAX;

    let token = current_user_token();
    let path = match user_cstring(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }

    let flags = match FchownatFlags::from_bits(flags) {
        Some(flags) => flags,
        None => return EINVAL,
    };
    if path.is_empty() && !flags.contains(FchownatFlags::AT_EMPTY_PATH) {
        return ENOENT;
    }

    let follow_final = !flags.contains(FchownatFlags::AT_SYMLINK_NOFOLLOW);
    let inode = if path.is_empty() {
        match resolve_start_inode(dirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    } else {
        let start = if path.starts_with('/') {
            vfs_root().mountpoint_root_inode()
        } else {
            match resolve_start_inode(dirfd) {
                Ok(inode) => inode,
                Err(errno) => return errno,
            }
        };
        match vfs_lookup(&start, &path, follow_final) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    };

    let mut meta = match inode.metadata() {
        Ok(meta) => meta,
        Err(e) => return -(e as isize),
    };

    let chown_requested = owner != CHOWN_ID_NO_CHANGE || group != CHOWN_ID_NO_CHANGE;
    if owner != CHOWN_ID_NO_CHANGE {
        meta.uid = owner;
    }
    if group != CHOWN_ID_NO_CHANGE {
        meta.gid = group;
    }
    if chown_requested {
        meta.mode.remove(vfs::InodeMode::S_ISUID);
        if meta.mode.contains(vfs::InodeMode::S_IXGRP) {
            meta.mode.remove(vfs::InodeMode::S_ISGID);
        }
    }

    match inode.set_metadata(&meta) {
        Ok(()) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

pub fn sys_mknodat(dirfd: usize, path: *const u8, mode: u32, _dev: usize) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path_str = match UserCString::from_addr(path as usize).read(token) {
        Ok(s) => s,
        Err(_) => return EFAULT,
    };
    if path_str.is_empty() {
        return ENOENT;
    }
    let start = match resolve_start_inode(dirfd) {
        Ok(inode) => inode,
        Err(e) => return e,
    };
    let (parent, leaf) = match vfs_lookup_parent_for_start(&start, &path_str) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let file_type = match vfs::InodeMode::from_bits_truncate(mode) & vfs::InodeMode::S_IFMT {
        m if m == vfs::InodeMode::S_IFIFO => FileType::Pipe,
        m if m == vfs::InodeMode::S_IFBLK => FileType::BlockDevice,
        m if m == vfs::InodeMode::S_IFCHR => FileType::CharDevice,
        m if m == vfs::InodeMode::S_IFSOCK => FileType::Socket,
        m if m == vfs::InodeMode::S_IFREG || m == vfs::InodeMode::S_IFDIR => return EINVAL,
        _ => return EINVAL,
    };
    let perm = vfs::InodeMode::from_bits_truncate(mode) & vfs::InodeMode::S_IALLUGO;
    match parent.create(&leaf, file_type, perm) {
        Ok(_) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

pub fn sys_chdir(path: *const u8) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path = match user_cstring(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    info!("[sys_chdir] path: {}", path);
    if path.is_empty() {
        return ENOENT;
    }
    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }

    // 克隆当前 cwd 状态后释放锁，避免在 find/open 持锁
    let (cwd_inode, old_path) = {
        let fs_ref = task.process.fs();
        let lock = fs_ref.lock();
        (lock.working_inode.clone(), lock.working_path.clone())
    };

    let target = match vfs_lookup(&cwd_inode.inode, &path, true) {
        Ok(inode) => match vfs::File::new(inode, vfs::FileFlags::O_RDONLY) {
            Ok(f) => f,
            Err(e) => return -(e as isize),
        },
        Err(errno) => return errno,
    };
    let fs_ref = task.process.fs();
    let mut lock = fs_ref.lock();
    lock.working_inode = Arc::new(target);
    lock.working_path = normalize_cwd(&old_path, &path);
    SUCCESS
}

pub fn sys_fchdir(fd: usize) -> isize {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    if !file.is_dir() {
        return ENOTDIR;
    }
    let inode = file.inode.clone();
    drop(fd_table);
    let file = match vfs::File::new(inode, vfs::FileFlags::O_RDONLY) {
        Ok(f) => f,
        Err(e) => return -(e as isize),
    };
    let fs_ref = task.process.fs();
    let mut lock = fs_ref.lock();
    let old_path = lock.working_path.clone();
    lock.working_inode = Arc::new(file);
    // fchdir: 路径不变 (无法确定 fd 对应的路径名)
    SUCCESS
}

pub fn sys_flock(_fd: usize, _operation: u32) -> isize {
    ENOSYS
}

pub fn sys_openat(dirfd: usize, path: *const u8, flags: u32, mode: u32) -> isize {
    let mode_bits = mode;
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path = match user_cstring(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }
    let flags = match OpenFlags::from_bits(flags) {
        Some(flags) => flags,
        None => {
            warn!("[sys_openat] unknown flags");
            return EINVAL;
        }
    };
    let _mode = StatMode::from_bits(mode);
    info!(
        "[sys_openat] dirfd: {}, path: {}, flags: {:?}, mode: {:?}",
        dirfd as isize, path, flags, _mode
    );
    let create_mode = vfs::InodeMode::from_bits_truncate(mode_bits) & vfs::InodeMode::S_IALLUGO;
    let new_file = match open_file_at(dirfd, &path, flags, create_mode) {
        Ok(file) => file,
        Err(errno) => return errno,
    };

    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();
    let new_fd = match fd_table.alloc_fd(new_file, flags.contains(OpenFlags::O_CLOEXEC)) {
        Ok(fd) => fd,
        Err(e) => return -(e as isize),
    };
    new_fd as isize
}

pub fn sys_renameat2(
    olddirfd: usize,
    oldpath: *const u8,
    newdirfd: usize,
    newpath: *const u8,
    _flags: u32,
) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let oldpath_str = match user_cstring(token, oldpath) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    let newpath_str = match user_cstring(token, newpath) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    info!(
        "[sys_renameat2] old: dirfd={} path={}, new: dirfd={} path={}",
        olddirfd as isize, oldpath_str, newdirfd as isize, newpath_str
    );

    let old_start = match resolve_start_inode(olddirfd) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };
    let new_start = match resolve_start_inode(newdirfd) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };

    // 解析 oldpath: 获取父目录 + 叶子名
    let (old_parent, old_leaf) = match vfs_lookup_parent_for_start(&old_start, &oldpath_str) {
        Ok(pair) => pair,
        Err(errno) => return errno,
    };

    // 解析 newpath: 获取父目录 + 叶子名
    let (new_parent, new_leaf) = match vfs_lookup_parent_for_start(&new_start, &newpath_str) {
        Ok(pair) => pair,
        Err(errno) => return errno,
    };

    match old_parent.rename(&old_leaf, &new_parent, &new_leaf) {
        Ok(_) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

const FIONREAD: u32 = 0x541B;

pub fn sys_ioctl(fd: usize, cmd: u32, arg: usize) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };

    if cmd == FIONREAD {
        let md = match file.metadata() {
            Ok(m) => m,
            Err(e) => return -(e as isize),
        };
        let remaining = (md.size as usize).saturating_sub(file.offset());
        let val = remaining.min(i32::MAX as usize) as i32;
        match crate::mm::translated_refmut(token, arg as *mut i32) {
            Ok(r) => *r = val,
            Err(_) => return EFAULT,
        }
        return 0;
    }

    match file.inode.ioctl(cmd, arg, file.private_data()) {
        Ok(n) => n as isize,
        Err(e) => -(e as isize),
    }
}

pub fn sys_ppoll(fds: usize, nfds: usize, tmo_p: usize, sigmask: usize) -> isize {
    ppoll(
        fds as *mut PollFd,
        nfds,
        tmo_p as *const TimeSpec,
        sigmask as *const crate::task::Signals,
    )
}

pub fn sys_mkdirat(dirfd: usize, path: *const u8, mode: u32) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path = match user_cstring(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    info!(
        "[sys_mkdirat] dirfd: {}, path: {}, mode: {:?}",
        dirfd as isize,
        path,
        StatMode::from_bits(mode)
    );
    let start = match resolve_start_inode(dirfd) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };
    // Root directory "/" already exists
    if path == "/" || path == "." {
        return EEXIST;
    }
    let (parent, leaf) = match vfs_lookup_parent_for_start(&start, &path) {
        Ok(result) => result,
        Err(errno) => return errno,
    };
    let dir_mode = vfs::InodeMode::from_bits_truncate(mode) & vfs::InodeMode::S_IALLUGO;
    match parent.mkdir(&leaf, dir_mode) {
        Ok(_) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

bitflags! {
    pub struct UnlinkatFlags: u32 {
        const AT_REMOVEDIR = 0x200;
    }
}

/// # Warning
/// Currently we have no hard-link so this syscall will remove file directly.
pub fn sys_unlinkat(dirfd: usize, path: *const u8, flags: u32) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path = match user_cstring(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    let flags = match UnlinkatFlags::from_bits(flags) {
        Some(flags) => flags,
        None => {
            warn!("[sys_unlinkat] unknown flags");
            return EINVAL;
        }
    };
    info!(
        "[sys_unlinkat] dirfd: {}, path: {}, flags: {:?}",
        dirfd as isize, path, flags
    );

    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }

    let start = match resolve_start_inode(dirfd) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };
    let (parent, leaf) = match vfs_lookup_parent_for_start(&start, &path) {
        Ok(result) => result,
        Err(errno) => return errno,
    };
    let result = if flags.contains(UnlinkatFlags::AT_REMOVEDIR) {
        parent.rmdir(&leaf)
    } else {
        parent.unlink(&leaf)
    };
    match result {
        Ok(()) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

bitflags! {
    pub struct UmountFlags: u32 {
        const MNT_FORCE           =   1;
        const MNT_DETACH          =   2;
        const MNT_EXPIRE          =   4;
        const UMOUNT_NOFOLLOW     =   8;
    }
}

pub fn sys_umount2(target: *const u8, flags: u32) -> isize {
    if target.is_null() {
        return EINVAL;
    }
    let token = current_user_token();
    let target = match user_cstring(token, target) {
        Ok(target) => target,
        Err(errno) => return errno,
    };
    let flags = match UmountFlags::from_bits(flags) {
        Some(flags) => flags,
        None => return EINVAL,
    };
    info!("[sys_umount2] target: {}, flags: {:?}", target, flags);
    let (lookup_inode, lookup_path) = {
        let task = current_task().unwrap();
        let fs_ref = task.process.fs();
        let fs = fs_ref.lock();
        if target.starts_with('/') {
            let root: Arc<dyn vfs::IndexNode> = crate::fs::vfs_root().mountpoint_root_inode();
            (root, target)
        } else {
            let cwd_inode: Arc<dyn vfs::IndexNode> = fs.working_inode.inode.clone();
            let path = alloc::format!("{}/{}", fs.working_path, target);
            (cwd_inode, path)
        }
    };
    let inode = match vfs_lookup(&lookup_inode, &lookup_path, false) {
        Ok(inode) => inode,
        Err(errno) => {
            error!("[sys_umount2] vfs_lookup failed for path '{}': errno={}", lookup_path, errno);
            return errno;
        }
    };
    match inode.umount() {
        Ok(_) => SUCCESS,
        Err(e) => {
            error!("[sys_umount2] inode.umount() failed for '{}': errno={}", lookup_path, e as isize);
            -(e as isize)
        }
    }
}

bitflags! {
    pub struct MountFlags: usize {
        const MS_RDONLY         =   1;
        const MS_NOSUID         =   2;
        const MS_NODEV          =   4;
        const MS_NOEXEC         =   8;
        const MS_SYNCHRONOUS    =   16;
        const MS_REMOUNT        =   32;
        const MS_MANDLOCK       =   64;
        const MS_DIRSYNC        =   128;
        const MS_NOATIME        =   1024;
        const MS_NODIRATIME     =   2048;
        const MS_BIND           =   4096;
        const MS_MOVE           =   8192;
        const MS_REC            =   16384;
        const MS_SILENT         =   32768;
        const MS_POSIXACL       =   (1<<16);
        const MS_UNBINDABLE     =   (1<<17);
        const MS_PRIVATE        =   (1<<18);
        const MS_SLAVE          =   (1<<19);
        const MS_SHARED         =   (1<<20);
        const MS_RELATIME       =   (1<<21);
        const MS_KERNMOUNT      =   (1<<22);
        const MS_I_VERSION      =   (1<<23);
        const MS_STRICTATIME    =   (1<<24);
        const MS_LAZYTIME       =   (1<<25);
        const MS_NOREMOTELOCK   =   (1<<27);
        const MS_NOSEC          =   (1<<28);
        const MS_BORN           =   (1<<29);
        const MS_ACTIVE         =   (1<<30);
        const MS_NOUSER         =   (1<<31);
    }
}

/// Bind mount: make source subtree visible at target.
fn do_bind_mount(
    source: *const u8,
    token: usize,
    lookup_inode: &Arc<dyn vfs::IndexNode>,
    lookup_path: &str,
    target_inode: Arc<dyn vfs::IndexNode>,
    mountflags: MountFlags,
) -> isize {
    let source_path = if source.is_null() {
        return EINVAL;
    } else {
        match user_cstring(token, source) {
            Ok(s) => s,
            Err(errno) => return errno,
        }
    };

    let source_inode = match vfs_lookup(lookup_inode, &source_path, true) {
        Ok(inode) => inode,
        Err(errno) => {
            error!("[do_bind_mount] vfs_lookup source '{}' failed: {}", source_path, errno);
            return errno;
        }
    };

    let source_mfs_inode = match source_inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
        Some(mfs) => mfs,
        None => return EINVAL,
    };
    let source_mount_fs = source_mfs_inode.mount_fs.clone();
    let source_inner: Arc<dyn vfs::IndexNode> = source_mfs_inode.inner_inode.clone();

    // Collect recursive bind snapshot BEFORE creating base mount, so the
    // new mnt_fs doesn't pollute the source mount tree during snapshotting.
    let rbind_snapshot: Option<Vec<(Arc<vfs::MountFS>, Arc<vfs::MountFS>, alloc::string::String)>> =
        if mountflags.contains(MountFlags::MS_REC) {
            Some(collect_rbind_snapshot(source_mount_fs.clone(), source_inner.clone()))
        } else {
            None
        };

    let target_mfs_inode = match target_inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
        Some(mfs) => mfs,
        None => return EINVAL,
    };

    let mnt_flags = vfs::MountFlags::from_bits_truncate(mountflags.bits() as u32);
    let mnt_fs = match target_mfs_inode.mount_subtree(
        source_mount_fs.inner_filesystem(),
        source_inner,
        mnt_flags,
        Some(alloc::string::String::from(lookup_path)),
    ) {
        Ok(fs) => fs,
        Err(e) => return -(e as isize),
    };
    mnt_fs.set_mount_source(Some(source_path));

    if let Some(snapshot) = rbind_snapshot {
        if let Err(e) = apply_rbind_snapshot(
            &snapshot,
            source_mount_fs,
            mnt_fs.clone(),
            lookup_path,
        ) {
            let _ = mnt_fs.umount();
            return -(e as isize);
        }
    }

    SUCCESS
}

/// Collect all submounts under `source_subtree_root` within `source_mfs` tree.
///
/// Returns Vec of (child_mfs, parent_mfs, relative_name) — BFS order,
/// no mutations to the mount tree.
fn collect_rbind_snapshot(
    source_mfs: Arc<vfs::MountFS>,
    source_subtree_root: Arc<dyn vfs::IndexNode>,
) -> Vec<(Arc<vfs::MountFS>, Arc<vfs::MountFS>, alloc::string::String)> {
    let mut queue: VecDeque<(Arc<vfs::MountFS>, Arc<dyn vfs::IndexNode>)> = VecDeque::new();
    let mut result: Vec<(Arc<vfs::MountFS>, Arc<vfs::MountFS>, alloc::string::String)> = Vec::new();
    let mut seen: Vec<usize> = Vec::new();
    let root_ptr = Arc::as_ptr(&source_mfs) as usize;
    seen.push(root_ptr);

    // Seed: submounts directly reachable from source_subtree_root
    {
        let mps = source_mfs.mountpoints.lock();
        for (&ino, child_mfs) in mps.iter() {
            let ptr = Arc::as_ptr(child_mfs) as usize;
            if seen.contains(&ptr) {
                continue;
            }
            // Find the name of this mountpoint under subtree_root
            if let Ok(dirents) = source_subtree_root.list_dirents() {
                if let Some((name, _, _)) = dirents.iter().find(|(_, i, _)| *i == ino) {
                    seen.push(ptr);
                    queue.push_back((child_mfs.clone(), child_mfs.mountpoint_root_inode()));
                    result.push((child_mfs.clone(), source_mfs.clone(), name.clone()));
                }
            }
        }
    }

    // BFS: for each child, collect its submounts
    while let Some((child_mfs, child_root)) = queue.pop_front() {
        let mps = child_mfs.mountpoints.lock();
        for (&grand_ino, grandchild) in mps.iter() {
            let ptr = Arc::as_ptr(grandchild) as usize;
            if seen.contains(&ptr) {
                continue;
            }
            seen.push(ptr);
            // Find name using child's inner inode (no mountpoint crossing)
            if let Some(ref child_mfs_inode) = child_root.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
                if let Ok(dirents) = child_mfs_inode.inner_inode.list_dirents() {
                    if let Some((name, _, _)) = dirents.iter().find(|(_, i, _)| *i == grand_ino) {
                        result.push((grandchild.clone(), child_mfs.clone(), name.clone()));
                        queue.push_back((grandchild.clone(), grandchild.mountpoint_root_inode()));
                    }
                }
            }
        }
    }
    result
}

/// Apply previously collected rbind snapshot to the target mount tree.
///
/// Uses source→target parent mapping (by Arc pointer) to locate the correct
/// target parent for each submount. All-or-nothing: any failure rolls back
/// all created mounts.
fn apply_rbind_snapshot(
    snapshot: &[(Arc<vfs::MountFS>, Arc<vfs::MountFS>, alloc::string::String)],
    source_mfs: Arc<vfs::MountFS>,
    target_mfs: Arc<vfs::MountFS>,
    _target_base_path: &str,
) -> Result<(), SyscallErr> {
    let mut mnt_map: BTreeMap<usize, Arc<vfs::MountFS>> = BTreeMap::new();
    mnt_map.insert(Arc::as_ptr(&source_mfs) as usize, target_mfs.clone());

    let mut created: Vec<Arc<vfs::MountFS>> = Vec::new();

    for (child_mfs, source_parent_mfs, child_name) in snapshot {
        let source_parent_ptr = Arc::as_ptr(source_parent_mfs) as usize;
        let target_parent = match mnt_map.get(&source_parent_ptr) {
            Some(p) => p.clone(),
            None => {
                rollback_mounts(&created);
                return Err(SyscallErr::EINVAL);
            }
        };

        let target_parent_inode: Arc<dyn vfs::IndexNode> = target_parent.mountpoint_root_inode();
        let target_child_inode = match target_parent_inode.find(child_name) {
            Ok(inode) => inode,
            Err(_) => {
                rollback_mounts(&created);
                return Err(SyscallErr::ENOENT);
            }
        };

        let target_mfs_inode = match target_child_inode
            .as_any_ref()
            .downcast_ref::<vfs::MountFSInode>()
        {
            Some(m) => vfs::MountFSInode::new(m.inner_inode.clone(), target_parent.clone()),
            None => {
                rollback_mounts(&created);
                return Err(SyscallErr::EINVAL);
            }
        };

        let mount_path = match target_parent.mount_path() {
            Some(ref p) => alloc::format!("{}/{}", p, child_name),
            None => alloc::format!("/{}", child_name),
        };

        match target_mfs_inode.mount_subtree(
            child_mfs.inner_filesystem(),
            child_mfs.root_inner_inode(),
            vfs::MountFlags::empty(),
            Some(mount_path),
        ) {
            Ok(new_mnt) => {
                if let Some(src) = child_mfs.mount_source() {
                    new_mnt.set_mount_source(Some(src));
                }
                let child_ptr = Arc::as_ptr(child_mfs) as usize;
                mnt_map.insert(child_ptr, new_mnt.clone());
                created.push(new_mnt);
            }
            Err(_) => {
                rollback_mounts(&created);
                return Err(SyscallErr::EIO);
            }
        }
    }

    drop(mnt_map);
    Ok(())
}

fn rollback_mounts(created: &[Arc<vfs::MountFS>]) {
    for mnt in created.iter().rev() {
        let _ = mnt.umount();
    }
}

pub fn sys_mount(
    source: *const u8,
    target: *const u8,
    filesystemtype: *const u8,
    mountflags_raw: usize,
    data: *const u8,
) -> isize {
    if target.is_null() {
        return EINVAL;
    }
    let token = current_user_token();
    let target = match user_cstring(token, target) {
        Ok(target) => target,
        Err(errno) => return errno,
    };

    // Parse mountflags early — needed for flag routing
    let mountflags = match MountFlags::from_bits(mountflags_raw) {
        Some(f) => f,
        None => return EINVAL,
    };

    // Resolve target path (support CWD-relative)
    let (lookup_inode, lookup_path) = {
        let task = current_task().unwrap();
        let fs_ref = task.process.fs();
        let fs = fs_ref.lock();
        if target.starts_with('/') {
            let root: Arc<dyn vfs::IndexNode> = crate::fs::vfs_root().mountpoint_root_inode();
            (root, target)
        } else {
            let cwd_inode: Arc<dyn vfs::IndexNode> = fs.working_inode.inode.clone();
            let path = alloc::format!("{}/{}", fs.working_path, target);
            (cwd_inode, path)
        }
    };

    // Look up the target inode — must be a directory
    let target_inode = match vfs_lookup(&lookup_inode, &lookup_path, false) {
        Ok(inode) => inode,
        Err(errno) => {
            error!("[sys_mount] vfs_lookup failed for '{}': errno={}", lookup_path, errno);
            return errno;
        }
    };
    let md = match target_inode.metadata() {
        Ok(md) => md,
        Err(e) => return -(e as isize),
    };
    if md.file_type != FileType::Dir {
        return ENOTDIR;
    }
    let inode_id = md.inode_id;

    // ── Flag routing — must happen BEFORE any RamFS creation ──

    let propagation = mountflags
        & (MountFlags::MS_SHARED | MountFlags::MS_PRIVATE | MountFlags::MS_SLAVE | MountFlags::MS_UNBINDABLE);

    if !propagation.is_empty() {
        // Allow at most one propagation type
        if propagation.bits().count_ones() != 1
            || mountflags.intersects(MountFlags::MS_BIND | MountFlags::MS_MOVE | MountFlags::MS_REMOUNT)
        {
            return EINVAL;
        }
        let target_mnt_inode = match target_inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
            Some(m) => m,
            None => return EINVAL,
        };
        let prop_type = if propagation.contains(MountFlags::MS_SHARED) {
            vfs::propagation::PropagationType::Shared
        } else if propagation.contains(MountFlags::MS_PRIVATE) {
            vfs::propagation::PropagationType::Private
        } else if propagation.contains(MountFlags::MS_SLAVE) {
            vfs::propagation::PropagationType::Slave
        } else {
            vfs::propagation::PropagationType::Unbindable
        };
        let mnt = target_mnt_inode.mount_fs.clone();
        mnt.propagation().set_type(prop_type);
        if prop_type == vfs::propagation::PropagationType::Shared {
            vfs::propagation::register_peer(&mnt);
        }
        return SUCCESS;
    }

    if mountflags.intersects(MountFlags::MS_BIND) {
        return do_bind_mount(source, token, &lookup_inode, &lookup_path, target_inode, mountflags);
    }

    if mountflags.intersects(MountFlags::MS_MOVE | MountFlags::MS_REMOUNT) {
        return EINVAL;
    }

    if mountflags.intersects(MountFlags::MS_REC) {
        // MS_REC is a modifier, not a standalone operation
        return EINVAL;
    }

    // ── Normal mount path ──

    // filesystemtype is required for normal mounts (already checked NULL at entry for bind,
    // but normal mounts need it)
    if filesystemtype.is_null() {
        return EINVAL;
    }

    let source = if source.is_null() {
        String::new()
    } else {
        match user_cstring(token, source) {
            Ok(source) => source,
            Err(errno) => return errno,
        }
    };
    let filesystemtype = match user_cstring(token, filesystemtype) {
        Ok(filesystemtype) => filesystemtype,
        Err(errno) => return errno,
    };

    info!(
        "[sys_mount] source: {}, target: {}, filesystemtype: {}, mountflags: {:?}, data: {:?}",
        source, lookup_path, filesystemtype, mountflags, data
    );

    if matches!(filesystemtype.as_str(), "cgroup" | "cgroup2") {
        return ENODEV;
    }

    // Get the parent MountFS via downcast
    let parent_mount_fs = if let Some(mfs_inode) =
        (target_inode.as_any_ref().downcast_ref::<vfs::MountFSInode>())
    {
        mfs_inode.mount_fs.clone()
    } else {
        crate::fs::vfs_root().clone()
    };

    // Create a tmpfs-backed MountFS and register it
    let tmpfs = crate::fs::ramfs::RamFS::new_with_quota(4096);
    let mnt_flags = vfs::MountFlags::from_bits_truncate(mountflags.bits() as u32);
    let mnt_fs = vfs::MountFS::new(tmpfs, mnt_flags);

    parent_mount_fs.add_mount(inode_id, mnt_fs.clone())
        .map(|_| {
            if let Some(target_mnt) = target_inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
                let backref = vfs::MountFSInode::new(
                    target_mnt.inner_inode.clone(),
                    target_mnt.mount_fs.clone(),
                );
                mnt_fs.set_self_mountpoint(Some(backref));
            }
            mnt_fs.set_mount_path(Some(lookup_path));
            SUCCESS
        })
        .unwrap_or_else(|e| {
            error!("[sys_mount] add_mount failed: errno={}", e as isize);
            -(e as isize)
        })
}

bitflags! {
    pub struct UtimensatFlags: u32 {
        const AT_SYMLINK_NOFOLLOW = 0x100;
    }
}

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
            let _ = _file.inode.set_metadata(&metadata);
        }
    }
    SUCCESS
}

#[allow(non_camel_case_types)]
#[derive(Debug, Eq, PartialEq, FromPrimitive)]
#[repr(u32)]
pub enum Fcntl_Command {
    DUPFD = 0,
    GETFD = 1,
    SETFD = 2,
    GETFL = 3,
    SETFL = 4,
    GETLK = 5,
    SETLK = 6,
    SETLKW = 7,
    SETOWN = 8,
    GETOWN = 9,
    SETSIG = 10,
    GETSIG = 11,
    SETOWN_EX = 15,
    GETOWN_EX = 16,
    GETOWNER_UIDS = 17,
    OFD_GETLK = 36,
    OFD_SETLK = 37,
    OFD_SETLKW = 38,
    SETLEASE = 1024,
    GETLEASE = 1025,
    NOTIFY = 1026,
    CANCELLK = 1029,
    DUPFD_CLOEXEC = 1030,
    SETPIPE_SZ = 1031,
    GETPIPE_SZ = 1032,
    ADD_SEALS = 1033,
    GET_SEALS = 1034,
    GET_RW_HINT = 1035,
    SET_RW_HINT = 1036,
    GET_FILE_RW_HINT = 1037,
    SET_FILE_RW_HINT = 1038,
    #[num_enum(default)]
    ILLEAGAL,
}

pub fn sys_fcntl(fd: usize, cmd: u32, arg: usize) -> isize {
    const FD_CLOEXEC: usize = 1;

    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();

    info!(
        "[sys_fcntl] fd: {}, cmd: {:?}, arg: {:X}",
        fd,
        Fcntl_Command::from_primitive(cmd),
        arg
    );

    let command = Fcntl_Command::from_primitive(cmd);
    match command {
        Fcntl_Command::DUPFD | Fcntl_Command::DUPFD_CLOEXEC => {
            let cloexec = matches!(command, Fcntl_Command::DUPFD_CLOEXEC);
            let file = match fd_table.get_file(fd) {
                Ok(file) => match file.try_clone() { Some(f) => f, None => return EBADF, },
                Err(e) => return -(e as isize),
            };

            match fd_table.alloc_fd_from(arg, file, cloexec) {
                Ok(fd) => fd as isize,
                Err(e) => -(e as isize),
            }
        }
        Fcntl_Command::GETFD => {
            // Check that fd is valid first
            match fd_table.get_file(fd) { Ok(_) => {}, Err(e) => return -(e as isize), };
            fd_table.get_cloexec(fd) as isize
        }
        Fcntl_Command::SETFD => {
            match fd_table.set_cloexec(fd, (arg & FD_CLOEXEC) != 0) { Ok(_) => {}, Err(e) => return -(e as isize), };
            if (arg & !FD_CLOEXEC) != 0 {
                warn!("[fcntl] Unsupported flag exists: {:X}", arg);
            }
            SUCCESS
        }
        Fcntl_Command::SETFL => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            };

            let n_block = (arg & (OpenFlags::O_NONBLOCK.bits() as usize)) != 0;

            file.set_nonblock(n_block);
            warn!("[sys_fcntl] set fd {} nonblock to {}", fd, n_block);
            SUCCESS
        }
        Fcntl_Command::GETFL => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            };
            let flags = file.flags();
            let access = flags.access_flags();
            let mut res = if access == FileFlags::O_RDWR {
                OpenFlags::O_RDWR.bits() as isize
            } else if access == FileFlags::O_WRONLY {
                OpenFlags::O_WRONLY.bits() as isize
            } else {
                OpenFlags::O_RDONLY.bits() as isize
            };
            if file.is_nonblock() {
                res |= OpenFlags::O_NONBLOCK.bits() as isize;
            }
            res
        }
        command => {
            warn!("[fcntl] Unsupported command: {:?}", command);
            -(SyscallErr::EINVAL as isize)
        }
    }
}

pub fn sys_pselect(
    nfds: usize,
    read_fds: *mut FdSet,
    write_fds: *mut FdSet,
    exception_fds: *mut FdSet,
    timeout: *mut TimeSpec,
    sigmask_args: usize,
) -> isize {
    if (nfds as isize) < 0 {
        return EINVAL;
    }

    // pselect6 syscall (SYS_PSELECT6=72) passes sigmask via a {ss, ss_len} structure:
    //   struct { const sigset_t *ss; size_t ss_len; };
    // args[5] points to this structure in user space, NOT directly to a sigset_t.
    // The musl wrapper builds it as: long data[2] = { (long)&mask, sizeof(sigset_t) }.
    let sigmask: *const crate::task::signal::Signals = if sigmask_args != 0 {
        let token = current_user_token();
        match UserPtr::<usize>::from_addr(sigmask_args).read(token) {
            Ok(ptr) => {
                if ptr != 0 {
                    ptr as *const crate::task::signal::Signals
                } else {
                    core::ptr::null()
                }
            }
            Err(errno) => return errno,
        }
    } else {
        core::ptr::null()
    };
    let token = current_user_token();
    let mut kread_fds = match UserPtr::new(read_fds as *const FdSet).read_optional(token) {
        Ok(fds) => fds,
        Err(errno) => return errno,
    };
    let mut kwrite_fds = match UserPtr::new(write_fds as *const FdSet).read_optional(token) {
        Ok(fds) => fds,
        Err(errno) => return errno,
    };
    let mut kexception_fds = match UserPtr::new(exception_fds as *const FdSet).read_optional(token) {
        Ok(fds) => fds,
        Err(errno) => return errno,
    };
    let ktimeout = match UserPtr::new(timeout as *const TimeSpec).read_optional(token) {
        Ok(timeout) => timeout,
        Err(errno) => return errno,
    };
    let mut ret = pselect(
        nfds,
        &mut kread_fds,
        &mut kwrite_fds,
        &mut kexception_fds,
        &ktimeout,
        sigmask,
    );
    if ret < 0 {
        return ret;
    }
    /*
    WARNING! The EFAULT errno is NOT mentioned in man for Linux.
    However, it is mentioned in BSD man, so we keep it anyway.
     */
    if let Some(kread_fds) = &kread_fds {
        trace!("[pselect] read_fds: {:?}", kread_fds);
        if UserPtrMut::new(read_fds).write(token, kread_fds).is_err() {
            log::error!("[sys_pselect] Error copying to read_fds {:?}", read_fds);
            ret = EFAULT;
        };
    }
    if let Some(kwrite_fds) = &kwrite_fds {
        trace!("[pselect] write_fds: {:?}", kwrite_fds);
        if UserPtrMut::new(write_fds).write(token, kwrite_fds).is_err() {
            log::error!("[sys_pselect] Error copying to write_fds {:?}", write_fds);
            ret = EFAULT;
        };
    }
    if let Some(kexception_fds) = &kexception_fds {
        trace!("[pselect] exception_fds: {:?}", kexception_fds);
        if UserPtrMut::new(exception_fds)
            .write(token, kexception_fds)
            .is_err()
        {
            log::error!(
                "[sys_pselect] Error copying to exception_fds {:?}",
                exception_fds
            );
            ret = EFAULT;
        };
    }

    ret
}

/// umask() sets the calling process's file mode creation mask (umask) to
/// mask & 0777 (i.e., only the file permission bits of mask are used),
/// and returns the previous value of the mask.
/// # WARNING
/// In current implementation, umask is always 0. This syscall won't do anything.
pub fn sys_umask(mask: u32) -> isize {
    info!("[sys_umask] mask: {:o}", mask);
    warn!(
        "[sys_umask] In current implementation, umask is always 0. This syscall won't do anything."
    );
    0
}

bitflags! {
    pub struct FaccessatMode: u32 {
        const F_OK = 0;
        const R_OK = 4;
        const W_OK = 2;
        const X_OK = 1;
    }
    pub struct FaccessatFlags: u32 {
        const AT_SYMLINK_NOFOLLOW = 0x100;
        const AT_EACCESS = 0x200;
    }
}

fn access_subject_ids(use_effective: bool) -> (u32, u32) {
    let task = current_task().unwrap();
    let inner = task.acquire_inner_lock();
    if use_effective {
        (inner.euid, inner.egid)
    } else {
        (inner.uid, inner.gid)
    }
}

fn permission_class_bits(meta: &vfs::Metadata, uid: u32, gid: u32) -> u32 {
    let mode = meta.mode.bits() & 0o777;
    if uid == meta.uid {
        (mode >> 6) & 0o7
    } else if gid == meta.gid {
        (mode >> 3) & 0o7
    } else {
        mode & 0o7
    }
}

fn has_final_access(meta: &vfs::Metadata, mode: FaccessatMode, uid: u32, gid: u32) -> bool {
    if mode.bits() == 0 {
        return true;
    }
    if uid == 0 {
        return !mode.contains(FaccessatMode::X_OK) || (meta.mode.bits() & 0o111) != 0;
    }

    let allowed = permission_class_bits(meta, uid, gid);
    if mode.contains(FaccessatMode::R_OK) && (allowed & 0o4) == 0 {
        return false;
    }
    if mode.contains(FaccessatMode::W_OK) && (allowed & 0o2) == 0 {
        return false;
    }
    if mode.contains(FaccessatMode::X_OK) && (allowed & 0o1) == 0 {
        return false;
    }
    true
}

fn has_search_access(meta: &vfs::Metadata, uid: u32, gid: u32) -> bool {
    uid == 0 || (permission_class_bits(meta, uid, gid) & 0o1) != 0
}

fn check_parent_search_access(
    start: &Arc<dyn vfs::IndexNode>,
    path: &str,
    uid: u32,
    gid: u32,
) -> isize {
    let components = parse_path(path);
    let base = if path.starts_with('/') {
        vfs_root().mountpoint_root_inode()
    } else {
        start.clone()
    };

    let check_dir = |inode: &Arc<dyn vfs::IndexNode>| -> isize {
        let meta = match inode.metadata() {
            Ok(meta) => meta,
            Err(e) => return -(e as isize),
        };
        if meta.file_type != FileType::Dir {
            return ENOTDIR;
        }
        if !has_search_access(&meta, uid, gid) {
            return EACCES;
        }
        SUCCESS
    };

    let result = check_dir(&base);
    if result != SUCCESS {
        return result;
    }

    let mut prefix = String::new();
    for (idx, name) in components
        .iter()
        .take(components.len().saturating_sub(1))
        .enumerate()
    {
        if idx > 0 {
            prefix.push('/');
        }
        prefix.push_str(name);
        let inode = match vfs_lookup(&base, &prefix, true) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        };
        let result = check_dir(&inode);
        if result != SUCCESS {
            return result;
        }
    }
    SUCCESS
}

fn check_parent_write_search_access(
    parent: &Arc<dyn vfs::IndexNode>,
    uid: u32,
    gid: u32,
) -> Result<(), isize> {
    let meta = match parent.metadata() {
        Ok(meta) => meta,
        Err(e) => return Err(-(e as isize)),
    };
    if meta.file_type != FileType::Dir {
        return Err(ENOTDIR);
    }
    if !has_directory_write_search_access(&meta, uid, gid) {
        return Err(EACCES);
    }
    Ok(())
}

pub fn sys_faccessat2(dirfd: usize, pathname: *const u8, mode: u32, flags: u32) -> isize {
    let token = current_user_token();
    let pathname = match user_cstring(token, pathname) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    let mode = match FaccessatMode::from_bits(mode) {
        Some(mode) => mode,
        None => {
            warn!("[sys_faccessat2] unknown mode");
            return EINVAL;
        }
    };
    let flags = match FaccessatFlags::from_bits(flags) {
        Some(flags) => flags,
        None => {
            warn!("[sys_faccessat2] unknown flags");
            return EINVAL;
        }
    };

    info!(
        "[sys_faccessat2] dirfd: {}, pathname: {}, mode: {:?}, flags: {:?}",
        dirfd as isize, pathname, mode, flags
    );

    if pathname.is_empty() {
        return ENOENT;
    }
    if let Err(errno) = validate_path_len(&pathname) {
        return errno;
    }

    let nofollow = flags.contains(FaccessatFlags::AT_SYMLINK_NOFOLLOW);
    let start_inode = if pathname.starts_with('/') {
        vfs_root().mountpoint_root_inode()
    } else {
        match resolve_start_inode(dirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    };
    let (uid, gid) = access_subject_ids(flags.contains(FaccessatFlags::AT_EACCESS));
    let parent_result = check_parent_search_access(&start_inode, &pathname, uid, gid);
    if parent_result != SUCCESS {
        return parent_result;
    }
    let inode = match vfs_lookup(&start_inode, &pathname, !nofollow) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };
    let meta = match inode.metadata() {
        Ok(meta) => meta,
        Err(e) => return -(e as isize),
    };
    if mode.contains(FaccessatMode::W_OK) {
        if let Some(mnt) = inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
            if mnt.mount_fs.mount_flags().contains(vfs::MountFlags::RDONLY) {
                return EROFS;
            }
        }
    }
    if has_final_access(&meta, mode, uid, gid) {
        SUCCESS
    } else {
        EACCES
    }
}

bitflags! {
    pub struct MsyncFlags: u32 {
        const MS_ASYNC      =   1;
        const MS_INVALIDATE =   2;
        const MS_SYNC       =   4;
    }
}

pub fn sys_msync(addr: usize, length: usize, flags: u32) -> isize {
    if !VirtAddr::from(addr).aligned() {
        return EINVAL;
    }
    let flags = match MsyncFlags::from_bits(flags) {
        Some(flags) => flags,
        None => return EINVAL,
    };
    if flags.contains(MsyncFlags::MS_ASYNC) && flags.contains(MsyncFlags::MS_SYNC) {
        return EINVAL;
    }
    let task = current_task().unwrap();
    let vm_ref = task.process.vm();
    if let Err(errno) = vm_ref
        .lock()
        .validate_msync_range(addr, length, flags.contains(MsyncFlags::MS_INVALIDATE))
    {
        return errno;
    }
    info!(
        "[sys_msync] addr: {:X}, length: {:X}, flags: {:?}",
        addr, flags, flags
    );
    SUCCESS
}

pub fn sys_ftruncate(fd: usize, length: isize) -> isize {
    if length < 0 {
        return EINVAL;
    }
    let task = current_task().unwrap();
    let inode = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        let file = match fd_table.get_file(fd) {
            Ok(file) => file,
            Err(e) => return -(e as isize),
        };
        if file.is_dir() {
            return EISDIR;
        }
        if matches!(file.file_type(), vfs::FileType::Pipe | vfs::FileType::Socket) {
            return EINVAL;
        }
        if !file.flags().is_writable() {
            return EINVAL;
        }
        file.inode.clone()
    };
    match inode.resize(length as usize) {
        Ok(()) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

pub fn sys_truncate(path: *const u8, length: isize) -> isize {
    if length < 0 {
        return EINVAL;
    }
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path = match user_cstring(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }
    if path.is_empty() {
        return ENOENT;
    }
    let cwd_inode = {
        let fs_ref = task.process.fs();
        let lock = fs_ref.lock();
        lock.working_inode.inode.clone()
    };
    let inode = if path.starts_with('/') {
        match vfs_lookup_absolute(&path) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    } else {
        match vfs_lookup(&cwd_inode, &path, true) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    };
    let md = match inode.metadata() {
        Ok(md) => md,
        Err(e) => return -(e as isize),
    };
    if md.file_type == FileType::Dir {
        return EISDIR;
    }
    match inode.resize(length as usize) {
        Ok(()) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

pub fn sys_fallocate(fd: usize, mode: u32, offset: isize, len: isize) -> isize {
    if offset < 0 || len <= 0 {
        return EINVAL;
    }
    if mode != 0 {
        warn!("[sys_fallocate] unsupported mode: {:#x}", mode);
        return EOPNOTSUPP;
    }

    let end = match offset.checked_add(len) {
        Some(end) => end,
        None => return EFBIG,
    };

    let task = current_task().unwrap();
    let files_ref = task.process.files();
        let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    info!(
        "[sys_fallocate] fd: {}, mode: {:#x}, offset: {}, len: {}, end: {}",
        fd, mode, offset, len, end
    );

    if file.get_size() >= end as usize {
        return SUCCESS;
    }
    match file.truncate_size(end as usize) {
        Ok(()) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

pub fn sys_symlinkat(target: *const u8, newdirfd: usize, linkpath: *const u8) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();

    let target_str = match user_cstring(token, target) {
        Ok(s) => s,
        Err(e) => return e,
    };

    let linkpath_str = match user_cstring(token, linkpath) {
        Ok(s) => s,
        Err(e) => return e,
    };

    log::info!(
        "[sys_symlinkat] target: {}, newdirfd: {}, linkpath: {}",
        target_str,
        newdirfd as isize,
        linkpath_str
    );

    let start = match resolve_start_inode(newdirfd) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };

    let components = crate::fs::parse_path(&linkpath_str);
    let leaf = match components.last() {
        Some(n) => n.clone(),
        None => return ENOENT,
    };

    let parent_dir = if components.len() == 1 {
        if linkpath_str.starts_with('/') {
            crate::fs::vfs_root().mountpoint_root_inode()
        } else {
            start
        }
    } else {
        let parent_comps = &components[..components.len() - 1];
        let joined = parent_comps.iter()
            .map(|s| s.as_str())
            .collect::<Vec<&str>>()
            .join("/");
        let parent_path = if linkpath_str.starts_with('/') {
            if joined.is_empty() { String::from("/") } else { alloc::format!("/{}", joined) }
        } else {
            joined
        };
        match crate::fs::vfs_lookup(&start, &parent_path, true) {
            Ok(parent) => parent,
            Err(errno) => return errno,
        }
    };

    match parent_dir.symlink(&leaf, &target_str) {
        Ok(_) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

/// sys_linkat — 创建硬链接
///
/// int linkat(int olddirfd, const char *oldpath, int newdirfd, const char *newpath, int flags);
pub fn sys_linkat(
    olddirfd: usize,
    oldpath: *const u8,
    newdirfd: usize,
    newpath: *const u8,
    _flags: u32,
) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();

    let oldpath_str = match user_cstring(token, oldpath) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let newpath_str = match user_cstring(token, newpath) {
        Ok(s) => s,
        Err(e) => return e,
    };

    if let Err(errno) = validate_path_len(&oldpath_str) {
        return errno;
    }
    if let Err(errno) = validate_path_len(&newpath_str) {
        return errno;
    }

    log::info!(
        "[sys_linkat] old: dirfd={} path={}, new: dirfd={} path={}",
        olddirfd as isize,
        oldpath_str,
        newdirfd as isize,
        newpath_str
    );

    let old_start = match resolve_start_inode(olddirfd) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };

    if oldpath_str.is_empty() {
        return ENOENT;
    }

    // 查找已存在的 inode
    let existing = match crate::fs::vfs_lookup(&old_start, &oldpath_str, true) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };

    // 禁止创建目录的硬链接（POSIX 不允许，除 root 外）
    let meta = match existing.metadata() {
        Ok(m) => m,
        Err(e) => return -(e as isize),
    };
    if meta.file_type == crate::fs::vfs::FileType::Dir {
        return -(SyscallErr::EISDIR as isize);
    }

    // 解析新路径：获取父目录 + 叶子名
    let new_start = match resolve_start_inode(newdirfd) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };

    let components = crate::fs::parse_path(&newpath_str);
    let leaf = if let Some(n) = components.last() {
        n.clone()
    } else {
        return ENOENT;
    };

    let parent_dir = if components.len() == 1 {
        if newpath_str.starts_with('/') {
            crate::fs::vfs_root().mountpoint_root_inode()
        } else {
            new_start
        }
    } else {
        let parent_comps = &components[..components.len() - 1];
        let joined = parent_comps
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<&str>>()
            .join("/");
        let parent_path = if newpath_str.starts_with('/') {
            if joined.is_empty() {
                String::from("/")
            } else {
                alloc::format!("/{}", joined)
            }
        } else {
            joined
        };
        match crate::fs::vfs_lookup(&new_start, &parent_path, true) {
            Ok(parent) => parent,
            Err(errno) => return errno,
        }
    };

    match parent_dir.link(&leaf, &existing) {
        Ok(_) => SUCCESS,
        Err(e) => -(e as isize),
    }
}
