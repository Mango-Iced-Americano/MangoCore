use super::errno::*;
use crate::fs::poll::{ppoll, pselect, FdSet, PollFd};
use crate::fs::vfs::{self, FileFlags, FileType, SeekFrom, SuperBlock};
use crate::fs::vfs::fcntl::{FcntlCommand, PosixFlock, F_UNLCK};
use crate::fs::vfs::posix_lock::{init_posix_lock_manager, mgr, posix_lock_get, posix_lock_set, release_posix_for_owner, LockKey, LockOwner};
use crate::fs::*;
use crate::mm::{
    MapPermission, UserBufferReader, UserBufferWriter, UserCString, UserIoVec, UserPtr,
    UserPtrMut, UserSlice, VirtAddr,
};
use crate::syscall::utils::wait_io_core;
use crate::task::{
    current_task, current_user_token, find_process_by_pid, find_task_by_tid,
    is_executable_inode_busy, is_writable_inode_busy, signal::Signals, WaitQueue,
    WaitResult,
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
use core::sync::atomic::{AtomicUsize, Ordering};
use lazy_static::lazy_static;
use log::{debug, error, info, trace, warn};
use num_enum::TryFromPrimitive;

const OPEN_ACCMODE_MASK: u32 = 0o3;
const MFD_CLOEXEC: u32 = 0x0001;
const MFD_ALLOW_SEALING: u32 = 0x0002;
const MFD_HUGETLB: u32 = 0x0004;
const MFD_HUGE_SHIFT: u32 = 26;
const MFD_HUGE_MASK: u32 = 0x3f << MFD_HUGE_SHIFT;
const MFD_VALID_FLAGS: u32 = MFD_CLOEXEC | MFD_ALLOW_SEALING | MFD_HUGETLB | MFD_HUGE_MASK;
const MEMFD_NAME_MAX: usize = 249;
static MEMFD_COUNTER: AtomicUsize = AtomicUsize::new(0);

const SEEK_SET: i16 = 0;
const SEEK_CUR: i16 = 1;
const SEEK_END: i16 = 2;
const LOCK_SH: u32 = 1;
const LOCK_EX: u32 = 2;
const LOCK_NB: u32 = 4;
const LOCK_UN: u32 = 8;

#[derive(Clone, Copy, Debug)]
struct FlockLock {
    key: LockKey,
    owner_description: usize,
    exclusive: bool,
}

lazy_static! {
    static ref FLOCK_LOCKS: spin::Mutex<Vec<FlockLock>> = spin::Mutex::new(Vec::new());
}

pub const AT_FDCWD: usize = 100usize.wrapping_neg();
pub const AT_EMPTY_PATH: u32 = 0x1000;
pub const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
pub const AT_REMOVEDIR: u32 = 0x200;
pub const AT_SYMLINK_FOLLOW: u32 = 0x400;

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

fn validate_memfd_flags(flags: u32) -> Result<(), SyscallErr> {
    if (flags & !MFD_VALID_FLAGS) != 0 {
        return Err(SyscallErr::EINVAL);
    }
    if (flags & MFD_HUGE_MASK) != 0 && (flags & MFD_HUGETLB) == 0 {
        return Err(SyscallErr::EINVAL);
    }
    if (flags & MFD_HUGETLB) != 0 {
        return Err(SyscallErr::ENODEV);
    }
    Ok(())
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

#[inline]
fn offset_is_negative(offset: usize) -> bool {
    offset > isize::MAX as usize
}

#[inline]
fn is_stream_file(file: &vfs::File) -> bool {
    file.mode().contains(vfs::FileMode::FMODE_STREAM)
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

fn apply_current_umask(mode: vfs::InodeMode) -> vfs::InodeMode {
    let task = current_task().unwrap();
    let fs_ref = task.process.fs();
    let mask = fs_ref.lock().umask & 0o777;
    mode & vfs::InodeMode::S_IALLUGO & !vfs::InodeMode::from_bits_truncate(mask)
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
) -> Result<Arc<vfs::File>, isize> {
    let start = resolve_start_inode(dirfd)?;
    if path.is_empty() {
        let md = start.metadata().map_err(|e| -(e as isize))?;
        return Ok(vfs::File::new_without_open(start, _open_flags_to_vfs_flags(flags), md.file_type));
    }

    let (uid, gid) = open_subject_ids();
    let parent_result = check_parent_search_access(&start, path, uid, gid);
    if parent_result != SUCCESS {
        return Err(parent_result);
    }

    let follow_final = !flags.contains(OpenFlags::O_NOFOLLOW);
    match vfs_lookup(&start, path, follow_final) {
        Ok(target) => {
            // O_PATH + O_NOFOLLOW opens the symlink itself, not ELOOP (Linux semantics)
            if !follow_final && !flags.contains(OpenFlags::O_PATH) {
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
            // Named FIFO: substitute the filesystem inode with a Pipe-backed inode
            if md.file_type == FileType::Pipe {
                const O_ACCMODE: u32 = 0o3;
                let access = flags.bits() & O_ACCMODE;
                let for_read = access != OpenFlags::O_WRONLY.bits();
                let for_write = access == OpenFlags::O_WRONLY.bits()
                    || access == OpenFlags::O_RDWR.bits();
                return match crate::fs::dev::pipe::fifo_open(
                    (md.dev_id, md.inode_id),
                    for_read,
                    for_write,
                ) {
                    Some(pipe_inode) => {
                        Ok(vfs::File::new_without_open(
                            pipe_inode,
                            _open_flags_to_vfs_flags(flags),
                            vfs::FileType::Pipe,
                        ))
                    }
                    None => Err(ENOMEM),
                };
            }
            if md.file_type == FileType::Dir
                && (flags.contains(OpenFlags::O_WRONLY)
                    || flags.contains(OpenFlags::O_RDWR)
                    || flags.contains(OpenFlags::O_CREAT))
            {
                // Linux 6.6+: O_CREAT|O_DIRECTORY is EINVAL, not EISDIR
                if flags.contains(OpenFlags::O_CREAT) && flags.contains(OpenFlags::O_DIRECTORY) {
                    return Err(EINVAL);
                }
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
            let task = current_task().unwrap();
            let umask = task.acquire_inner_lock().umask;
            let effective_mode = mode & !vfs::InodeMode::from_bits_truncate(umask);
            let inode = parent
                .create(
                    &leaf,
                    FileType::File,
                    effective_mode & vfs::InodeMode::S_IALLUGO,
                )
                .map_err(|e| -(e as isize))?;
            apply_created_inode_metadata(&parent_meta, &inode, effective_mode, uid, gid)?;
            vfs::File::new(inode, _open_flags_to_vfs_flags(flags)).map_err(|e| -(e as isize))
        }
        Err(errno) => Err(errno),
    }
}

fn check_memfd_truncate_seals(file: &vfs::File, new_len: usize) -> Result<(), isize> {
    if let Some(seals) = file.memfd_seal_bits() {
        let current_len = file
            .metadata()
            .map_err(|e| -(e as isize))?
            .size
            .max(0) as usize;
        if new_len < current_len && (seals & vfs::F_SEAL_SHRINK) != 0 {
            return Err(EPERM);
        }
        if new_len > current_len && (seals & vfs::F_SEAL_GROW) != 0 {
            return Err(EPERM);
        }
    }
    Ok(())
}

fn open_proc_self_fd(path: &str, flags: OpenFlags) -> Option<Result<Arc<vfs::File>, isize>> {
    let fd_text = path.strip_prefix("/proc/self/fd/")?;
    if fd_text.is_empty() || fd_text.as_bytes().iter().any(|b| !b.is_ascii_digit()) {
        return Some(Err(ENOENT));
    }
    let fd = match fd_text.parse::<usize>() {
        Ok(fd) => fd,
        Err(_) => return Some(Err(ENOENT)),
    };

    let task = current_task().unwrap();
    let (inode, file_type, seals) = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        let file = match fd_table.get_file(fd) {
            Ok(file) => file,
            Err(e) => return Some(Err(-(e as isize))),
        };
        (file.inode.clone(), file.file_type(), file.memfd_seals())
    };

    let reopened = match vfs::File::new(inode.clone(), _open_flags_to_vfs_flags(flags)) {
        Ok(file) => file,
        Err(e) => return Some(Err(-(e as isize))),
    };
    let seals = seals?;
    reopened.set_memfd_seals(seals);

    if flags.contains(OpenFlags::O_TRUNC) {
        if file_type == vfs::FileType::Dir {
            return Some(Err(EISDIR));
        }
        if !reopened.flags().is_writable() {
            return Some(Err(EACCES));
        }
        if let Err(errno) = check_memfd_truncate_seals(&reopened, 0) {
            return Some(Err(errno));
        }
        if let Err(e) = inode.resize(0) {
            return Some(Err(-(e as isize)));
        }
    }

    Some(Ok(reopened))
}

#[inline]
fn writable_len_for_read(token: usize, user_addr: usize, want: usize) -> Result<usize, isize> {
    let accessible = crate::mm::user_accessible_len(
        token,
        user_addr as *const u8,
        want,
        crate::mm::UserAccess::Write,
    );
    if accessible != 0 {
        return Ok(accessible);
    }
    // Fault-in before consuming file data: lazy page succeeds, bad pointer
    // fails without advancing the file offset.
    UserBufferWriter::new(token, user_addr as *mut u8, 1).map(|_| ())?;
    let accessible = crate::mm::user_accessible_len(
        token,
        user_addr as *const u8,
        want,
        crate::mm::UserAccess::Write,
    );
    Ok(accessible.max(1).min(want))
}

#[inline]
fn iov_writable_len_for_read(
    user_iov: &UserIoVec,
    offset: usize,
    want: usize,
) -> Result<usize, isize> {
    let accessible = user_iov.accessible_len_at(offset, want, crate::mm::UserAccess::Write);
    if accessible != 0 {
        return Ok(accessible);
    }
    let ubuf = user_iov.writer_buffer_at(offset, 1)?;
    if ubuf.len() == 0 {
        return Err(EFAULT);
    }
    let accessible = user_iov.accessible_len_at(offset, want, crate::mm::UserAccess::Write);
    Ok(accessible.max(1).min(want))
}

fn read_into_user(file: &vfs::File, token: usize, buf: usize, count: usize) -> isize {
    if count == 0 {
        return match file.read(&mut []) {
            Ok(n) => n as isize,
            Err(e) => -(e as isize),
        };
    }

    let chunk_cap = count.min(crate::hal::IO_CHUNK_SIZE);
    let mut kbuf = alloc::vec::Vec::new();
    if kbuf.try_reserve(chunk_cap).is_err() {
        return -(SyscallErr::ENOMEM as isize);
    }
    unsafe { kbuf.set_len(chunk_cap); }

    let mut total = 0usize;
    while total < count {
        let want = (count - total).min(chunk_cap);

        // Validate destination chunk BEFORE reading from file
        let user_addr = match buf.checked_add(total) {
            Some(v) => v,
            None => return if total > 0 { total as isize } else { -(SyscallErr::EFAULT as isize) },
        };
        let accessible = match writable_len_for_read(token, user_addr, want) {
            Ok(n) => n,
            Err(errno) => return if total > 0 { total as isize } else { errno },
        };

        let n = match file.read(&mut kbuf[..accessible]) {
            Ok(n) => n,
            Err(e) => {
                let ret = -(e as isize);
                return if total > 0 { total as isize } else { ret };
            }
        };
        if n == 0 {
            break;
        }

        // Write to user one page at a time — each page fault-in is independent
        let mut copied = 0usize;
        while copied < n {
            let this_addr = user_addr.saturating_add(copied);
            let page_remain = crate::config::PAGE_SIZE - (this_addr & (crate::config::PAGE_SIZE - 1));
            let chunk = (n - copied).min(page_remain.max(1));
            let mut writer = match UserBufferWriter::new(token, this_addr as *mut u8, chunk) {
                Ok(w) => w,
                Err(errno) => {
                    if copied > 0 { total += copied; }
                    return if total > 0 { total as isize } else { errno };
                }
            };
            let c = match writer.write_from(&kbuf[copied..copied + chunk]) {
                Ok(c) => c,
                Err(errno) => {
                    if copied > 0 { total += copied; }
                    return if total > 0 { total as isize } else { errno };
                }
            };
            copied += c;
            if c < chunk { break; }
        }

        total += copied;
        if copied < n {
            break;
        }

        if let Some(task) = current_task() {
            if crate::task::has_actionable_signal(&task) {
                break;
            }
        }
    }
    total as isize
}

fn pread_into_user(file: &vfs::File, token: usize, buf: usize, count: usize, offset: usize) -> isize {
    if count == 0 {
        return match file.pread(offset, &mut []) {
            Ok(n) => n as isize,
            Err(e) => -(e as isize),
        };
    }

    let chunk_cap = count.min(crate::hal::IO_CHUNK_SIZE);
    let mut kbuf = alloc::vec::Vec::new();
    if kbuf.try_reserve(chunk_cap).is_err() {
        return -(SyscallErr::ENOMEM as isize);
    }
    unsafe { kbuf.set_len(chunk_cap); }

    let mut total = 0usize;
    while total < count {
        let want = (count - total).min(chunk_cap);
        let file_off = match offset.checked_add(total) {
            Some(v) => v,
            None => return if total > 0 { total as isize } else { -(SyscallErr::EINVAL as isize) },
        };

        let user_addr = match buf.checked_add(total) {
            Some(v) => v,
            None => return if total > 0 { total as isize } else { -(SyscallErr::EFAULT as isize) },
        };
        let accessible = match writable_len_for_read(token, user_addr, want) {
            Ok(n) => n,
            Err(errno) => return if total > 0 { total as isize } else { errno },
        };

        let n = match file.pread(file_off, &mut kbuf[..accessible]) {
            Ok(n) => n,
            Err(e) => {
                let ret = -(e as isize);
                return if total > 0 { total as isize } else { ret };
            }
        };
        if n == 0 {
            break;
        }

        // Write to user one page at a time — each page fault-in is independent
        let mut copied = 0usize;
        while copied < n {
            let this_addr = user_addr.saturating_add(copied);
            let page_remain = crate::config::PAGE_SIZE - (this_addr & (crate::config::PAGE_SIZE - 1));
            let chunk = (n - copied).min(page_remain.max(1));
            let mut writer = match UserBufferWriter::new(token, this_addr as *mut u8, chunk) {
                Ok(w) => w,
                Err(errno) => {
                    if copied > 0 { total += copied; }
                    return if total > 0 { total as isize } else { errno };
                }
            };
            let c = match writer.write_from(&kbuf[copied..copied + chunk]) {
                Ok(c) => c,
                Err(errno) => {
                    if copied > 0 { total += copied; }
                    return if total > 0 { total as isize } else { errno };
                }
            };
            copied += c;
            if c < chunk { break; }
        }

        total += copied;
        if copied < n {
            break;
        }

        if let Some(task) = current_task() {
            if crate::task::has_actionable_signal(&task) {
                break;
            }
        }
    }
    total as isize
}

fn write_from_user(file: &vfs::File, token: usize, buf: usize, count: usize) -> isize {
    if count == 0 {
        return match file.write(&[]) {
            Ok(n) => n as isize,
            Err(e) => -(e as isize),
        };
    }

    let chunk_cap = count.min(crate::hal::IO_CHUNK_SIZE);
    let mut kbuf = alloc::vec::Vec::new();
    if kbuf.try_reserve(chunk_cap).is_err() {
        return -(SyscallErr::ENOMEM as isize);
    }
    unsafe { kbuf.set_len(chunk_cap); }

    let mut total = 0usize;
    while total < count {
        let want = (count - total).min(chunk_cap);
        let user_addr = match buf.checked_add(total) {
            Some(v) => v,
            None => return if total > 0 { total as isize } else { -(SyscallErr::EFAULT as isize) },
        };

        let mut accessible = crate::mm::user_accessible_len(
            token,
            user_addr as *const u8,
            want,
            crate::mm::UserAccess::Read,
        );
        if accessible == 0 {
            if total > 0 {
                return total as isize;
            }
            accessible = want.min(crate::config::PAGE_SIZE);
        }

        let reader = match UserBufferReader::new(token, user_addr as *const u8, accessible) {
            Ok(r) => r,
            Err(errno) => return if total > 0 { total as isize } else { errno },
        };
        let copied = match reader.read_into(&mut kbuf[..accessible]) {
            Ok(n) => n,
            Err(errno) => return if total > 0 { total as isize } else { errno },
        };

        let n = match file.write(&kbuf[..copied]) {
            Ok(n) => n,
            Err(e) => {
                let ret = -(e as isize);
                return if total > 0 { total as isize } else { ret };
            }
        };

        total += n;
        if n == 0 || n < copied {
            break;
        }

        if let Some(task) = current_task() {
            if crate::task::has_actionable_signal(&task) {
                break;
            }
        }
    }
    total as isize
}

fn pwrite_from_user(file: &vfs::File, token: usize, buf: usize, count: usize, offset: usize) -> isize {
    if count == 0 {
        return match file.pwrite(offset, &[]) {
            Ok(n) => n as isize,
            Err(e) => -(e as isize),
        };
    }

    let chunk_cap = count.min(crate::hal::IO_CHUNK_SIZE);
    let mut kbuf = alloc::vec::Vec::new();
    if kbuf.try_reserve(chunk_cap).is_err() {
        return -(SyscallErr::ENOMEM as isize);
    }
    unsafe { kbuf.set_len(chunk_cap); }

    let mut total = 0usize;
    while total < count {
        let want = (count - total).min(chunk_cap);
        let file_off = match offset.checked_add(total) {
            Some(v) => v,
            None => return if total > 0 { total as isize } else { -(SyscallErr::EINVAL as isize) },
        };

        let user_addr = match buf.checked_add(total) {
            Some(v) => v,
            None => return if total > 0 { total as isize } else { -(SyscallErr::EFAULT as isize) },
        };

        let mut accessible = crate::mm::user_accessible_len(
            token,
            user_addr as *const u8,
            want,
            crate::mm::UserAccess::Read,
        );
        if accessible == 0 {
            if total > 0 {
                return total as isize;
            }
            accessible = want.min(crate::config::PAGE_SIZE);
        }

        let reader = match UserBufferReader::new(token, user_addr as *const u8, accessible) {
            Ok(r) => r,
            Err(errno) => return if total > 0 { total as isize } else { errno },
        };
        let copied = match reader.read_into(&mut kbuf[..accessible]) {
            Ok(n) => n,
            Err(errno) => return if total > 0 { total as isize } else { errno },
        };

        let n = match file.pwrite(file_off, &kbuf[..copied]) {
            Ok(n) => n,
            Err(e) => {
                let ret = -(e as isize);
                return if total > 0 { total as isize } else { ret };
            }
        };

        total += n;
        if n == 0 || n < copied {
            break;
        }

        if let Some(task) = current_task() {
            if crate::task::has_actionable_signal(&task) {
                break;
            }
        }
    }
    total as isize
}

fn write_start_offset(file: &vfs::File) -> usize {
    if file.flags().contains(FileFlags::O_APPEND) {
        if let Ok(metadata) = file.metadata() {
            if metadata.size > 0 {
                return metadata.size as usize;
            }
        }
    }
    file.offset()
}

fn pwrite_start_offset(file: &vfs::File, offset: usize) -> usize {
    if file.flags().contains(FileFlags::O_APPEND) {
        if let Ok(metadata) = file.metadata() {
            if metadata.size > 0 {
                return metadata.size as usize;
            }
        }
    }
    offset
}

fn raise_sigxfsz() {
    if let Some(task) = current_task() {
        task.acquire_inner_lock().add_signal(Signals::SIGXFSZ);
    }
}

fn apply_fsize_limit(
    file: &vfs::File,
    count: usize,
    offset: usize,
    fsize_limit: usize,
) -> Result<usize, isize> {
    if count == 0 || file.file_type() != FileType::File || fsize_limit == usize::MAX {
        return Ok(count);
    }
    if offset >= fsize_limit {
        raise_sigxfsz();
        return Err(EFBIG);
    }
    Ok(count.min(fsize_limit - offset))
}

// todo
pub fn sys_splice(
    fd_in: usize,
    off_in: *mut usize,
    fd_out: usize,
    off_out: *mut usize,
    len: usize,
    flags: u32,
) -> isize {
    if flags & !SPLICE_VALID_FLAGS != 0 {
        return EINVAL;
    }
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let in_file = match fd_table.get_file(fd_in) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    let out_file = match fd_table.get_file(fd_out) {
        Ok(file) => file,
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
    let nonblock =
        flags & SPLICE_F_NONBLOCK != 0 || in_file.is_nonblock() || out_file.is_nonblock();
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
                        match splice_read_stream(&in_file, buffer.as_mut_slice(), nonblock) {
                            Ok(n) => n,
                            Err(errno) => return errno,
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
                match splice_write_stream(&out_file, write_buffer, nonblock) {
                    Ok(n) => n,
                    Err(errno) => return errno,
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

fn splice_read_stream(
    file: &vfs::File,
    buf: &mut [u8],
    nonblock: bool,
) -> Result<usize, isize> {
    let mut read_once = || match file.read(buf) {
        Ok(n) => n as isize,
        Err(e) => -(e as isize),
    };
    let ret = if nonblock {
        read_once()
    } else if let Some(wq) = file.inode.read_wait_queue() {
        match WaitQueue::wait_until_interruptible(wq, || {
            let ret = read_once();
            if ret == -(SyscallErr::EAGAIN as isize) {
                None
            } else {
                Some(ret)
            }
        }) {
            WaitResult::Ready(n) => n,
            WaitResult::Interrupted => -(SyscallErr::ERESTART as isize),
            WaitResult::TimedOut => -(SyscallErr::EAGAIN as isize),
        }
    } else {
        read_once()
    };
    if ret < 0 {
        Err(ret)
    } else {
        Ok(ret as usize)
    }
}

fn splice_write_stream(file: &vfs::File, buf: &[u8], nonblock: bool) -> Result<usize, isize> {
    let mut write_once = || match file.write(buf) {
        Ok(n) => n as isize,
        Err(e) => -(e as isize),
    };
    let ret = if nonblock {
        write_once()
    } else if let Some(wq) = file.inode.write_wait_queue() {
        match WaitQueue::wait_until_interruptible(wq, || {
            let ret = write_once();
            if ret == -(SyscallErr::EAGAIN as isize) {
                None
            } else {
                Some(ret)
            }
        }) {
            WaitResult::Ready(n) => n,
            WaitResult::Interrupted => -(SyscallErr::ERESTART as isize),
            WaitResult::TimedOut => -(SyscallErr::EAGAIN as isize),
        }
    } else {
        write_once()
    };
    if ret < 0 {
        Err(ret)
    } else {
        Ok(ret as usize)
    }
}

/// # Warning
/// `fs` & `files` is locked in this function
fn __openat(dirfd: usize, path: &str) -> Result<Arc<vfs::File>, isize> {
    open_file_at(dirfd, path, OpenFlags::O_RDONLY, vfs::InodeMode::S_IRWXUGO)
}

pub fn sys_getcwd(buf: usize, size: usize) -> isize {
    let task = current_task().unwrap();
    let fs_ref = task.process.fs();
    let (cwd_inode, cached_path) = {
        let fs_lock = fs_ref.lock();
        (fs_lock.working_inode.inode.clone(), fs_lock.working_path.clone())
    };
    let working_dir = match cwd_inode.absolute_path() {
        Ok(path) => {
            if path != cached_path {
                fs_ref.lock().working_path = path.clone();
            }
            path
        }
        Err(_) => cached_path,
    };
    // ERANGE must be checked BEFORE buffer validation:
    // Linux returns ERANGE if buffer is too small, even if buf is partially invalid
    if working_dir.len() + 1 > size {
        return ERANGE;
    }
    let vm_ref = task.process.vm();
    let write_len = working_dir.len() + 1;
    if !vm_ref
        .lock()
        .contains_valid_buffer(buf, write_len, MapPermission::W)
    {
        return EFAULT;
    }
    let token = task.get_user_token();
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
    info!(
        "[sys_lseek] fd: {}, offset: {}, whence: {}",
        fd, offset, whence,
    );
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };

    // Explicit numeric match — SeekWhence bitflags lets 5,6,7 slip through
    match whence {
        0 => { /* SEEK_SET */ }
        1 => { /* SEEK_CUR */ }
        2 => { /* SEEK_END */ }
        3 | 4 => { /* SEEK_DATA / SEEK_HOLE */ }
        _ => {
            warn!("[sys_lseek] unknown whence: {}", whence);
            return EINVAL;
        }
    }

    // SEEK_DATA(3) / SEEK_HOLE(4): non-sparse files (treat all as dense)
    if whence == 3 || whence == 4 {
        let off = offset as i64;
        if off < 0 {
            return EINVAL;
        }
        // Release fd_table before I/O
        drop(fd_table);

        // Seekability: same check as File::lseek — non-seekable FDs return ESPIPE
        let ftype = file.file_type();
        if ftype != FileType::File && ftype != FileType::Dir {
            return ESPIPE;
        }

        let md = match file.metadata() {
            Ok(md) => md,
            Err(e) => return -(e as isize),
        };
        let file_size = md.size;
        if off >= file_size {
            return ENXIO;
        }
        match whence {
            3 => { // SEEK_DATA: return current offset (entire file is data)
                file.set_offset(off as usize);
                return off as isize;
            }
            4 => { // SEEK_HOLE: return file_size (hole at EOF)
                file.set_offset(file_size as usize);
                return file_size as isize;
            }
            _ => unreachable!(),
        }
    }

    drop(fd_table);
    let seek_from = match whence {
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
    let count = count.min(crate::hal::MAX_RW_COUNT);
    let task = current_task().unwrap();
    let file = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        match fd_table.get_file(fd) {
            Ok(fd_ref) => Arc::clone(&fd_ref),
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
    let mut count = count.min(crate::hal::MAX_RW_COUNT);
    let task = current_task().unwrap();
    let file = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        match fd_table.get_file(fd) {
            Ok(fd_ref) => Arc::clone(&fd_ref),
            Err(e) => return -(e as isize),
        }
    };
    if file.writable().is_err() {
        return EBADF;
    }
    let fsize_limit = task.acquire_inner_lock().fsize_limit_cur;
    count = match apply_fsize_limit(&file, count, write_start_offset(&file), fsize_limit) {
        Ok(count) => count,
        Err(errno) => return errno,
    };
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
    let count = count.min(crate::hal::MAX_RW_COUNT);
    let task = current_task().unwrap();
    let file = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        match fd_table.get_file(fd) {
            Ok(fd_ref) => Arc::clone(&fd_ref),
            Err(e) => return -(e as isize),
        }
    };
    // fd is not open for reading
    if file.readable().is_err() {
        return EBADF;
    }
    if offset_is_negative(offset) {
        return EINVAL;
    }
    let token = task.get_user_token();
    pread_into_user(&file, token, buf, count, offset)
}

pub fn sys_pwrite(fd: usize, buf: usize, count: usize, offset: usize) -> isize {
    let mut count = count.min(crate::hal::MAX_RW_COUNT);
    let task = current_task().unwrap();
    let file = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        match fd_table.get_file(fd) {
            Ok(fd_ref) => Arc::clone(&fd_ref),
            Err(e) => return -(e as isize),
        }
    };
    if offset_is_negative(offset) {
        return EINVAL;
    }
    if is_stream_file(&file) {
        return ESPIPE;
    }
    // fd is not open for writing
    if file.writable().is_err() {
        return EBADF;
    }
    let fsize_limit = task.acquire_inner_lock().fsize_limit_cur;
    count = match apply_fsize_limit(&file, count, pwrite_start_offset(&file, offset), fsize_limit) {
        Ok(count) => count,
        Err(errno) => return errno,
    };
    let token = task.get_user_token();
    pwrite_from_user(&file, token, buf, count, offset)
}

pub fn sys_preadv(fd: usize, iov: usize, iovcnt: usize, offset: usize) -> isize {
    let task = current_task().unwrap();
    let file = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        match fd_table.get_file(fd) {
            Ok(fd_ref) => Arc::clone(&fd_ref),
            Err(e) => return -(e as isize),
        }
    };
    if file.readable().is_err() {
        return EBADF;
    }
    if offset_is_negative(offset) {
        return EINVAL;
    }
    let token = task.get_user_token();
    let user_iov = match UserIoVec::read_user_iovecs(
        token,
        iov as *const crate::fs::iov::IOVec,
        iovcnt,
        crate::hal::MAX_RW_COUNT,
    ) {
        Ok(iov) => iov,
        Err(errno) => return errno,
    };
    let total_len = user_iov.capped_len();
    if total_len == 0 {
        return 0;
    }

    let chunk_cap = total_len.min(crate::hal::IO_CHUNK_SIZE);
    let mut kbuf = alloc::vec::Vec::new();
    if kbuf.try_reserve(chunk_cap).is_err() {
        return -(SyscallErr::ENOMEM as isize);
    }
    unsafe { kbuf.set_len(chunk_cap); }

    let mut done = 0usize;
    while done < total_len {
        let want = (total_len - done).min(chunk_cap);
        let file_off = match offset.checked_add(done) {
            Some(v) => v,
            None => return if done > 0 { done as isize } else { -(SyscallErr::EINVAL as isize) },
        };
        let accessible = match iov_writable_len_for_read(&user_iov, done, want) {
            Ok(n) => n,
            Err(errno) => return if done > 0 { done as isize } else { errno },
        };

        let n = match file.pread(file_off, &mut kbuf[..accessible]) {
            Ok(n) => n,
            Err(e) => {
                let ret = -(e as isize);
                return if done > 0 { done as isize } else { ret };
            }
        };
        if n == 0 {
            break;
        }

        // Write to user iovec one page at a time for cross-page fault-in
        let mut copied = 0usize;
        while copied < n {
            let chunk = (n - copied).min(crate::config::PAGE_SIZE);
            let mut ubuf = match user_iov.writer_buffer_at(done + copied, chunk) {
                Ok(b) => b,
                Err(errno) => {
                    if copied > 0 { done += copied; }
                    return if done > 0 { done as isize } else { errno };
                }
            };
            let c = ubuf.write_at(0, &kbuf[copied..copied + chunk]);
            copied += c;
            if c < chunk { break; }
        }

        done += copied;
        if copied < n {
            break;
        }

        if let Some(task) = current_task() {
            if crate::task::has_actionable_signal(&task) {
                break;
            }
        }
    }
    done as isize
}

pub fn sys_pwritev(fd: usize, iov: usize, iovcnt: usize, offset: usize) -> isize {
    let task = current_task().unwrap();
    let file = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        match fd_table.get_file(fd) {
            Ok(fd_ref) => Arc::clone(&fd_ref),
            Err(e) => return -(e as isize),
        }
    };
    if offset_is_negative(offset) {
        return EINVAL;
    }
    if is_stream_file(&file) {
        return ESPIPE;
    }
    if file.writable().is_err() {
        return EBADF;
    }
    let token = task.get_user_token();
    let user_iov = match UserIoVec::read_user_iovecs(
        token,
        iov as *const crate::fs::iov::IOVec,
        iovcnt,
        crate::hal::MAX_RW_COUNT,
    ) {
        Ok(iov) => iov,
        Err(errno) => return errno,
    };
    let total_len = user_iov.capped_len();
    if total_len == 0 {
        return 0;
    }

    let fsize_limit = task.acquire_inner_lock().fsize_limit_cur;
    let allowed = match apply_fsize_limit(
        &file,
        total_len,
        pwrite_start_offset(&file, offset),
        fsize_limit,
    ) {
        Ok(count) => count,
        Err(errno) => return errno,
    };

    let chunk_cap = allowed.min(crate::hal::IO_CHUNK_SIZE);
    let mut kbuf = alloc::vec::Vec::new();
    if kbuf.try_reserve(chunk_cap).is_err() {
        return -(SyscallErr::ENOMEM as isize);
    }
    unsafe { kbuf.set_len(chunk_cap); }

    let mut done = 0usize;
    while done < allowed {
        let want = (allowed - done).min(chunk_cap);
        let file_off = match offset.checked_add(done) {
            Some(v) => v,
            None => return if done > 0 { done as isize } else { -(SyscallErr::EINVAL as isize) },
        };
        let mut accessible = user_iov.accessible_len_at(done, want, crate::mm::UserAccess::Read);
        if accessible == 0 {
            if done > 0 {
                return done as isize;
            }
            accessible = want.min(crate::config::PAGE_SIZE);
        }

        let ubuf = match user_iov.reader_buffer_at(done, accessible) {
            Ok(b) => b,
            Err(errno) => return if done > 0 { done as isize } else { errno },
        };
        let copied = ubuf.read(&mut kbuf[..accessible]);

        let n = match file.pwrite(file_off, &kbuf[..copied.min(accessible)]) {
            Ok(n) => n,
            Err(e) => {
                let ret = -(e as isize);
                return if done > 0 { done as isize } else { ret };
            }
        };

        done += n;
        if n == 0 || n < copied {
            break;
        }

        if let Some(task) = current_task() {
            if crate::task::has_actionable_signal(&task) {
                break;
            }
        }
    }
    done as isize
}

fn split_offset64(offset_low: usize, offset_high: usize) -> usize {
    (offset_low & 0xffff_ffff) | ((offset_high & 0xffff_ffff) << 32)
}

pub fn sys_preadv2(
    fd: usize,
    iov: usize,
    iovcnt: usize,
    offset_low: usize,
    offset_high: usize,
    flags: usize,
) -> isize {
    if flags != 0 {
        return EOPNOTSUPP;
    }
    let offset = split_offset64(offset_low, offset_high);
    if offset == usize::MAX {
        sys_readv(fd, iov, iovcnt)
    } else {
        sys_preadv(fd, iov, iovcnt, offset)
    }
}

pub fn sys_pwritev2(
    fd: usize,
    iov: usize,
    iovcnt: usize,
    offset_low: usize,
    offset_high: usize,
    flags: usize,
) -> isize {
    if flags != 0 {
        return EOPNOTSUPP;
    }
    let offset = split_offset64(offset_low, offset_high);
    if offset == usize::MAX {
        sys_writev(fd, iov, iovcnt)
    } else {
        sys_pwritev(fd, iov, iovcnt, offset)
    }
}

pub fn sys_readv(fd: usize, iov: usize, iovcnt: usize) -> isize {
    let task = current_task().unwrap();
    let file = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        match fd_table.get_file(fd) {
            Ok(fd_ref) => Arc::clone(&fd_ref),
            Err(e) => return -(e as isize),
        }
    };
    if file.readable().is_err() {
        return EBADF;
    }
    let token = task.get_user_token();
    let user_iov = match UserIoVec::read_user_iovecs(
        token,
        iov as *const crate::fs::iov::IOVec,
        iovcnt,
        crate::hal::MAX_RW_COUNT,
    ) {
        Ok(iov) => iov,
        Err(errno) => return errno,
    };
    let total_len = user_iov.capped_len();
    if total_len == 0 {
        return 0;
    }

    let chunk_cap = total_len.min(crate::hal::IO_CHUNK_SIZE);
    let mut kbuf = alloc::vec::Vec::new();
    if kbuf.try_reserve(chunk_cap).is_err() {
        return -(SyscallErr::ENOMEM as isize);
    }
    unsafe { kbuf.set_len(chunk_cap); }

    let mut done = 0usize;
    while done < total_len {
        let want = (total_len - done).min(chunk_cap);
        let accessible = match iov_writable_len_for_read(&user_iov, done, want) {
            Ok(n) => n,
            Err(errno) => return if done > 0 { done as isize } else { errno },
        };

        let n = match file.read(&mut kbuf[..accessible]) {
            Ok(n) => n,
            Err(e) => {
                let ret = -(e as isize);
                return if done > 0 { done as isize } else { ret };
            }
        };
        if n == 0 {
            break;
        }

        // Write to user iovec one page at a time for cross-page fault-in
        let mut copied = 0usize;
        while copied < n {
            let chunk = (n - copied).min(crate::config::PAGE_SIZE);
            let mut ubuf = match user_iov.writer_buffer_at(done + copied, chunk) {
                Ok(b) => b,
                Err(errno) => {
                    if copied > 0 { done += copied; }
                    return if done > 0 { done as isize } else { errno };
                }
            };
            let c = ubuf.write_at(0, &kbuf[copied..copied + chunk]);
            copied += c;
            if c < chunk { break; }
        }

        done += copied;
        if copied < n {
            break;
        }

        if let Some(task) = current_task() {
            if crate::task::has_actionable_signal(&task) {
                break;
            }
        }
    }
    done as isize
}

pub fn sys_writev(fd: usize, iov: usize, iovcnt: usize) -> isize {
    let task = current_task().unwrap();
    let file = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        match fd_table.get_file(fd) {
            Ok(fd_ref) => Arc::clone(&fd_ref),
            Err(e) => return -(e as isize),
        }
    };
    if file.writable().is_err() {
        return EBADF;
    }
    let token = task.get_user_token();
    let user_iov = match UserIoVec::read_user_iovecs(
        token,
        iov as *const crate::fs::iov::IOVec,
        iovcnt,
        crate::hal::MAX_RW_COUNT,
    ) {
        Ok(iov) => iov,
        Err(errno) => return errno,
    };
    let total_len = user_iov.capped_len();
    if total_len == 0 {
        return 0;
    }

    let fsize_limit = task.acquire_inner_lock().fsize_limit_cur;
    let allowed = match apply_fsize_limit(
        &file,
        total_len,
        write_start_offset(&file),
        fsize_limit,
    ) {
        Ok(count) => count,
        Err(errno) => return errno,
    };

    let chunk_cap = allowed.min(crate::hal::IO_CHUNK_SIZE);
    let mut kbuf = alloc::vec::Vec::new();
    if kbuf.try_reserve(chunk_cap).is_err() {
        return -(SyscallErr::ENOMEM as isize);
    }
    unsafe { kbuf.set_len(chunk_cap); }

    let mut done = 0usize;
    while done < allowed {
        let want = (allowed - done).min(chunk_cap);
        let mut accessible = user_iov.accessible_len_at(done, want, crate::mm::UserAccess::Read);
        if accessible == 0 {
            if done > 0 {
                return done as isize;
            }
            accessible = want.min(crate::config::PAGE_SIZE);
        }

        let ubuf = match user_iov.reader_buffer_at(done, accessible) {
            Ok(b) => b,
            Err(errno) => return if done > 0 { done as isize } else { errno },
        };
        let copied = ubuf.read(&mut kbuf[..accessible]);

        let n = match file.write(&kbuf[..copied.min(accessible)]) {
            Ok(n) => n,
            Err(e) => {
                let ret = -(e as isize);
                return if done > 0 { done as isize } else { ret };
            }
        };

        done += n;
        if n == 0 || n < copied {
            break;
        }

        if let Some(task) = current_task() {
            if crate::task::has_actionable_signal(&task) {
                break;
            }
        }
    }
    done as isize
}

const SPLICE_F_MOVE: u32 = 0x01;
const SPLICE_F_NONBLOCK: u32 = 0x02;
const SPLICE_F_MORE: u32 = 0x04;
const SPLICE_F_GIFT: u32 = 0x08;
const SPLICE_VALID_FLAGS: u32 = SPLICE_F_MOVE | SPLICE_F_NONBLOCK | SPLICE_F_MORE | SPLICE_F_GIFT;
const VMSPLICE_VALID_FLAGS: u32 = SPLICE_VALID_FLAGS;

pub fn sys_vmsplice(fd: usize, iov: usize, iovcnt: usize, flags: u32) -> isize {
    if flags & !VMSPLICE_VALID_FLAGS != 0 {
        return EINVAL;
    }
    let task = current_task().unwrap();
    let file = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        match fd_table.get_file(fd) {
            Ok(fd_ref) => Arc::clone(&fd_ref),
            Err(e) => return -(e as isize),
        }
    };
    if file.file_type() != FileType::Pipe {
        return EINVAL;
    }
    if file.writable().is_err() {
        return EBADF;
    }

    let token = task.get_user_token();
    let user_iov = match UserIoVec::read_user_iovecs(
        token,
        iov as *const crate::fs::iov::IOVec,
        iovcnt,
        crate::hal::MAX_RW_COUNT,
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

    let mut try_write = || match file.write(&kernel_buf) {
        Ok(n) => n as isize,
        Err(e) => -(e as isize),
    };
    let is_nonblock = file.is_nonblock() || flags & SPLICE_F_NONBLOCK != 0;
    if is_nonblock {
        return try_write();
    }
    if let Some(wq) = file.inode.write_wait_queue() {
        match WaitQueue::wait_until_interruptible(wq, || {
            let ret = try_write();
            if ret == -(SyscallErr::EAGAIN as isize) {
                None
            } else {
                Some(ret)
            }
        }) {
            WaitResult::Ready(n) => n,
            WaitResult::Interrupted => -(SyscallErr::ERESTART as isize),
            WaitResult::TimedOut => -(SyscallErr::EAGAIN as isize),
        }
    } else {
        try_write()
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
    let count = count.min(crate::hal::MAX_RW_COUNT);
    let task = current_task().unwrap();
    let files_ref = task.process.files();
        let fd_table = files_ref.lock();
    let in_file = match fd_table.get_file(in_fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    let out_file = match fd_table.get_file(out_fd) {
        Ok(file) => file,
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
                            Err(e) => {
                                let ret = -(e as isize);
                                if count - left_bytes > 0 {
                                    break;
                                }
                                return ret;
                            }
                        };
                        *off_val += n;
                        n
                    } else {
                        match in_file.read(buffer.as_mut_slice()) {
                            Ok(n) => n,
                            Err(e) => {
                                let ret = -(e as isize);
                                if count - left_bytes > 0 {
                                    break;
                                }
                                return ret;
                            }
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
                if count - left_bytes == 0 {
                    return -(e as isize);
                }
                fallback(write_buffer.len());
                break;
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
    let len = len.min(crate::hal::MAX_RW_COUNT);
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let in_file = match fd_table.get_file(fd_in) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    let out_file = match fd_table.get_file(fd_out) {
        Ok(file) => file,
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
                Err(e) => {
                    if copied > 0 {
                        break;
                    }
                    return -(e as isize);
                }
            }
        } else {
            match in_file.read(buffer.as_mut_slice()) {
                Ok(n) => n,
                Err(e) => {
                    if copied > 0 {
                        break;
                    }
                    return -(e as isize);
                }
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
                    if copied > 0 {
                        if in_offset.is_none() {
                            let _ = in_file.lseek(SeekFrom::SeekCurrent(-(read_size as i64)));
                        }
                        break;
                    }
                    return -(e as isize);
                }
            }
        } else {
            match out_file.write(buffer.as_slice()) {
                Ok(n) => n,
                Err(e) => {
                    if copied > 0 {
                        if in_offset.is_none() {
                            let _ = in_file.lseek(SeekFrom::SeekCurrent(-(read_size as i64)));
                        }
                        break;
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
        Ok(file) => {
            let mut flock_releases = Vec::new();
            record_flock_close(&mut flock_releases, &file);
            drop(fd_table);
            release_closed_flock_descriptions(flock_releases);
            SUCCESS
        }
        Err(e) => -(e as isize),
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
        let mut flock_releases = Vec::new();
        for fd in first..=last {
            if let Ok(file) = fd_table.drop_fd(fd) {
                record_flock_close(&mut flock_releases, &file);
            } else if fd >= fd_table.len() {
                break;
            }
        }
        drop(fd_table);
        release_closed_flock_descriptions(flock_releases);
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
        AT_FDCWD => task.process.fs().lock().working_inode.clone(),
        fd => {
            let files_ref = task.process.files();
        let fd_table = files_ref.lock();
            match fd_table.get_file(fd) {
                Ok(file) => file,
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
        Ok(file) => file,
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
            Ok(file) => file,
            Err(e) => return -(e as isize),
        };
        let replaced_flock = fd_table.get_file(newfd).ok().map(|file| {
            (
                file.description_id(),
                Arc::strong_count(&file),
            )
        });

        let ret = match fd_table.alloc_fd_at(newfd, file, false) {
            Ok(fd) => fd as isize,
            Err(e) => -(e as isize),
        };
        drop(fd_table);
        if ret >= 0 {
            if let Some((description, refs)) = replaced_flock {
                if refs <= 1 {
                    release_flock_description(description);
                }
            }
        }
        ret
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
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    let replaced_flock = fd_table.get_file(newfd).ok().map(|file| {
        (
            file.description_id(),
            Arc::strong_count(&file),
        )
    });
    let ret = match fd_table.alloc_fd_at(newfd, file, is_cloexec) {
        Ok(fd) => fd as isize,
        Err(e) => -(e as isize),
    };
    drop(fd_table);
    if ret >= 0 {
        if let Some((description, refs)) = replaced_flock {
            if refs <= 1 {
                release_flock_description(description);
            }
        }
    }
    ret
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

    // Linux: readlinkat(fd, "", buf, size) operates on the symlink fd itself, unconditionally
    if path.is_empty() {
        let inode = match resolve_start_inode(dirfd) {
            Ok(inode) => inode,
            Err(e) => return e,
        };
        let md = match inode.metadata() {
            Ok(md) => md,
            Err(_) => return EINVAL,
        };
        if md.file_type != vfs::FileType::SymLink {
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
        let real_path = match String::from_utf8(link_buf) {
            Ok(s) => alloc::string::String::from(s.trim_end_matches('\0')),
            Err(_) => return EINVAL,
        };
        let len = real_path.len().min(bufsiz);
        let bytes = real_path.as_bytes();
        let mut user_buf = match UserBufferWriter::new(token, buf, len) {
            Ok(writer) => writer,
            Err(_) => return EFAULT,
        };
        if user_buf.write_from(&bytes[..len]).is_err() {
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
        let start = match resolve_start_inode(dirfd) { Ok(s) => s, Err(e) => return e, };

        let (uid, gid) = open_subject_ids();
        let perm_result = check_parent_search_access(&start, &path, uid, gid);
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
                warn!("[sys_readlinkat] metadata() failed: path={}, err={:?}", path, e);
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
    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }
    let flags = match FstatatFlags::from_bits(flags) {
        Some(flags) => flags,
        None => {
            warn!("[sys_fstatat] unknown flags");
            return EINVAL;
        }
    };
    if path.is_empty() && !flags.contains(FstatatFlags::AT_EMPTY_PATH) {
        return ENOENT;
    }

    info!(
        "[sys_fstatat] dirfd: {}, path: {:?}, flags: {:?}",
        dirfd as isize, path, flags,
    );

    // AT_EMPTY_PATH + empty path: stat the dirfd itself, skip path resolution
    if path.is_empty() && flags.contains(FstatatFlags::AT_EMPTY_PATH) {
        let inode = match resolve_start_inode(dirfd) {
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
        return SUCCESS;
    }

    let no_follow = flags.contains(FstatatFlags::AT_SYMLINK_NOFOLLOW);
    let start = match resolve_start_inode(dirfd) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };

    // Check search permission on all parent directories (EACCES)
    let (uid, gid) = open_subject_ids();
    let perm_result = check_parent_search_access(&start, &path, uid, gid);
    if perm_result != SUCCESS {
        return perm_result;
    }

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
    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }
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

    // AT_EMPTY_PATH: stat the dirfd itself (glibc dynamic linker on la64)
    if path.is_empty() {
        if !flags.contains(FstatatFlags::AT_EMPTY_PATH) {
            return ENOENT;
        }
        let start = match resolve_start_inode(dirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        };
        let statx = match start.metadata() {
            Ok(meta) => metadata_to_statx(&meta, mask),
            Err(e) => return -(e as isize),
        };
        if UserPtrMut::new(buf as *mut Statx).write(token, &statx).is_err() {
            return EFAULT;
        }
        return SUCCESS;
    }

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
        AT_FDCWD => task.process.fs().lock().working_inode.clone(),
        fd => {
            let files_ref = task.process.files();
        let fd_table = files_ref.lock();
            match fd_table.get_file(fd) {
                Ok(file) => file,
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
fn superblock_to_statfs(sb: &SuperBlock) -> Statfs {
    Statfs {
        f_type: sb.f_type as usize,
        f_bsize: sb.f_bsize as usize,
        f_blocks: sb.f_blocks,
        f_bfree: sb.f_bfree,
        f_bavail: sb.f_bavail,
        f_files: sb.f_files,
        f_ffree: sb.f_ffree,
        f_fsid: sb.f_fsid,
        f_namelen: sb.f_namelen as usize,
        f_frsize: sb.f_frsize as usize,
        f_flag: sb.flags as usize,
        f_spare: [0; 4],
    }
}

fn write_statfs(buf: *mut Statfs, statfs: &Statfs) -> isize {
    let token = current_task().unwrap().get_user_token();
    if UserPtrMut::new(buf).write(token, statfs).is_err() {
        log::error!("[sys_statfs] Failed to copy to {:?}", buf);
        return EFAULT;
    };
    SUCCESS
}

/// sys_statfs — get filesystem statistics for a mounted path
pub fn sys_statfs(pathname: *const u8, buf: *mut Statfs) -> isize {
    let token = current_user_token();
    let path = match user_cstring(token, pathname) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if path.is_empty() {
        return ENOENT;
    }
    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }
    let start = current_task()
        .map(|t| t.process.fs().lock().working_inode.inode.clone())
        .unwrap_or_else(|| crate::fs::vfs_root().mountpoint_root_inode());
    let inode = match crate::fs::vfs_lookup(&start, &path, true) {
        Ok(inode) => inode,
        Err(e) => return e,
    };
    let fs = inode.fs();
    let sb = match fs.statfs(&inode) {
        Ok(sb) => sb,
        Err(e) => return -(e as isize),
    };
    let statfs = superblock_to_statfs(&sb);
    write_statfs(buf, &statfs)
}

pub fn sys_fstatfs(fd: usize, buf: *mut Statfs) -> isize {
    let Some(task) = current_task() else {
        return ESRCH;
    };
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let inode = match fd_table.get_file(fd) {
        Ok(file) => file.inode.clone(),
        Err(e) => return -(e as isize),
    };
    drop(fd_table);

    let fs = inode.fs();
    let sb = match fs.statfs(&inode) {
        Ok(sb) => sb,
        Err(e) => return -(e as isize),
    };
    let statfs = superblock_to_statfs(&sb);
    write_statfs(buf, &statfs)
}

pub fn sys_fsync(fd: usize) -> isize {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    if !matches!(file.file_type(), FileType::File | FileType::Dir | FileType::BlockDevice) {
        return EINVAL;
    }
    drop(fd_table);
    match file.inode.sync() {
        Ok(()) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

pub fn sys_fdatasync(fd: usize) -> isize {
    let task = current_task().unwrap();

    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    drop(fd_table);

    if !matches!(file.file_type(), FileType::File | FileType::Dir | FileType::BlockDevice) {
        return EINVAL;
    }

    match file.inode.datasync() {
        Ok(()) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

pub fn sys_sync() -> isize {
    crate::fs::flush_all_page_caches();
    // Collect live ext4 instances, then flush metadata cache without holding the registry lock
    let mut guard = crate::fs::ext4::ext4fs::EXT4_REGISTRY.lock();
    let live: alloc::vec::Vec<_> = guard.iter().filter_map(|w| w.upgrade()).collect();
    guard.retain(|w| w.strong_count() > 0);
    drop(guard);
    for fs in &live {
        fs.flush_metadata_cache();
    }
    SUCCESS
}

pub fn sys_syncfs(fd: usize) -> isize {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(_e) => return EBADF,
    };
    drop(fd_table);

    // Flush all page caches (global, but correct for single-fs system)
    crate::fs::flush_all_page_caches();

    // Flush ext4 metadata caches for the filesystem containing this fd.
    // Must unwrap MountFSInode to reach the real Ext4FileSystem.
    let inode = vfs::MountFSInode::unwrap_inode(&file.inode);
    let fs = inode.fs();
    if let Some(ext4) = fs.as_any_ref().downcast_ref::<crate::fs::ext4::ext4fs::Ext4FileSystem>() {
        ext4.flush_metadata_cache();
    }

    SUCCESS
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
        let (uid, gid) = open_subject_ids();
        let perm_err = check_parent_search_access(&start, &path_str, uid, gid);
        if perm_err != SUCCESS {
            return perm_err;
        }
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
    // Check read-only filesystem (must precede EPERM per Linux semantics:
    // EROFS takes priority over EPERM)
    if let Some(mnt) = inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
        if mnt.mount_fs.mount_flags().contains(vfs::MountFlags::RDONLY) {
            return EROFS;
        }
    }
    let file_type = meta.mode & vfs::InodeMode::S_IFMT;
    meta.mode = file_type | (new_mode & vfs::InodeMode::S_IALLUGO);
    // Permission check: owner or root (DAC)
    let (caller_uid, _) = open_subject_ids();
    if caller_uid != 0 && caller_uid != meta.uid {
        return EPERM;
    }
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
    // Check read-only filesystem (must precede EPERM per Linux semantics:
    // EROFS takes priority over EPERM)
    if let Some(mnt) = file.inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
        if mnt.mount_fs.mount_flags().contains(vfs::MountFlags::RDONLY) {
            return EROFS;
        }
    }
    let file_type = meta.mode & vfs::InodeMode::S_IFMT;
    meta.mode = file_type | (new_mode & vfs::InodeMode::S_IALLUGO);
    // Permission check: owner or root (DAC)
    let (caller_uid, _) = open_subject_ids();
    if caller_uid != 0 && caller_uid != meta.uid {
        return EPERM;
    }
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

pub fn sys_fchown(fd: usize, owner: u32, group: u32) -> isize {
    const CHOWN_ID_NO_CHANGE: u32 = u32::MAX;

    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };

    let mut meta = match file.inode.metadata() {
        Ok(meta) => meta,
        Err(e) => return -(e as isize),
    };

    // Check read-only filesystem (must precede EPERM per Linux semantics:
    // EROFS takes priority over EPERM)
    if let Some(mnt) = file.inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
        if mnt.mount_fs.mount_flags().contains(vfs::MountFlags::RDONLY) {
            return EROFS;
        }
    }

    // Permission check: root-only (simplified DAC, no CAP_CHOWN)
    let (caller_uid, _) = open_subject_ids();
    if caller_uid != 0 {
        return EPERM;
    }

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

    match file.inode.set_metadata(&meta) {
        Ok(()) => SUCCESS,
        Err(e) => -(e as isize),
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
            crate::fs::current_root_inode()
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

    // Check read-only filesystem (must precede EPERM per Linux semantics:
    // EROFS takes priority over EPERM)
    if let Some(mnt) = inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
        if mnt.mount_fs.mount_flags().contains(vfs::MountFlags::RDONLY) {
            return EROFS;
        }
    }

    // Permission check: root-only (simplified DAC, no CAP_CHOWN)
    let (caller_uid, _) = open_subject_ids();
    if caller_uid != 0 {
        return EPERM;
    }

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

pub fn sys_mknodat(dirfd: usize, path: *const u8, mode: u32, dev: usize) -> isize {
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
        m if m == vfs::InodeMode::S_IFREG || m.is_empty() => FileType::File,
        m if m == vfs::InodeMode::S_IFDIR => return EINVAL,
        _ => return EINVAL,
    };
    let perm = apply_current_umask(vfs::InodeMode::from_bits_truncate(mode));
    // Only pass device number for CHR/BLK; FIFO/socket use 0
    let rdev = if file_type == FileType::CharDevice || file_type == FileType::BlockDevice {
        dev
    } else {
        0
    };
    match parent.create_with_data(&leaf, file_type, perm, rdev) {
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
        Ok(inode) => {
            // ENOTDIR: chdir target must be a directory
            let md = match inode.metadata() {
                Ok(md) => md,
                Err(e) => return -(e as isize),
            };
            if md.file_type != vfs::FileType::Dir {
                warn!("[sys_chdir] not a directory: {:?}", md.file_type);
                return ENOTDIR;
            }
            match vfs::File::new(inode, vfs::FileFlags::O_RDONLY) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            }
        }
        Err(errno) => return errno,
    };
    let working_path = target
        .inode
        .absolute_path()
        .unwrap_or_else(|_| normalize_cwd(&old_path, &path));
    let fs_ref = task.process.fs();
    let mut lock = fs_ref.lock();
    lock.working_inode = target;
    lock.working_path = working_path;
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
    let meta = match inode.metadata() {
        Ok(meta) => meta,
        Err(e) => return -(e as isize),
    };
    let (uid, gid) = open_subject_ids();
    if !has_search_access(&meta, uid, gid) {
        return EACCES;
    }
    let working_path = inode.absolute_path().ok();
    let file = match vfs::File::new(inode, vfs::FileFlags::O_RDONLY) {
        Ok(f) => f,
        Err(e) => return -(e as isize),
    };
    let fs_ref = task.process.fs();
    let mut lock = fs_ref.lock();
    lock.working_inode = file;
    if let Some(path) = working_path {
        lock.working_path = path;
    }
    SUCCESS
}

/// Change root directory for the calling process.
/// Only root (uid == 0) may call this. The target must be a directory.
pub fn sys_chroot(path: *const u8) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path = match user_cstring(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    info!("[sys_chroot] path: {}", path);
    if path.is_empty() {
        return ENOENT;
    }
    if let Err(errno) = validate_path_len(&path) {
        return errno;
    }

    // only root may chroot
    let (uid, _) = open_subject_ids();
    if uid != 0 {
        return EPERM;
    }

    // clone cwd while not holding fs lock
    let cwd_inode = {
        let fs_ref = task.process.fs();
        let lock = fs_ref.lock();
        lock.working_inode.inode.clone()
    };

    let target = match vfs_lookup(&cwd_inode, &path, true) {
        Ok(inode) => {
            let md = match inode.metadata() {
                Ok(md) => md,
                Err(e) => return -(e as isize),
            };
            if md.file_type != vfs::FileType::Dir {
                return ENOTDIR;
            }
            match vfs::File::new(inode, vfs::FileFlags::O_RDONLY) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            }
        }
        Err(errno) => return errno,
    };

    let target_inode = target.inode.clone();
    let fs_ref = task.process.fs();
    let mut lock = fs_ref.lock();
    lock.working_inode = target;
    lock.working_path = alloc::string::String::from("/");
    lock.root_inode = Some(target_inode);
    SUCCESS
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
    if let Some(result) = open_proc_self_fd(&path, flags) {
        let new_file = match result {
            Ok(file) => file,
            Err(errno) => return errno,
        };
        let files_ref = task.process.files();
        let mut fd_table = files_ref.lock();
        return match fd_table.alloc_fd(new_file, flags.contains(OpenFlags::O_CLOEXEC)) {
            Ok(fd) => fd as isize,
            Err(e) => -(e as isize),
        };
    }
    let create_mode = apply_current_umask(vfs::InodeMode::from_bits_truncate(mode_bits));
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

pub fn sys_memfd_create(name: *const u8, flags: u32) -> isize {
    if let Err(err) = validate_memfd_flags(flags) {
        return -(err as isize);
    }

    let task = current_task().unwrap();
    let token = task.get_user_token();
    let name = match user_cstring(token, name) {
        Ok(name) => name,
        Err(errno) => return errno,
    };
    if name.len() > MEMFD_NAME_MAX {
        return EINVAL;
    }

    let open_flags = OpenFlags::O_CREAT | OpenFlags::O_EXCL | OpenFlags::O_RDWR;
    let create_mode = vfs::InodeMode::from_bits_truncate(0o600);
    let mut last_errno = EEXIST;
    let file = {
        let tid = task.gettid();
        let mut created = None;
        for _ in 0..8 {
            let id = MEMFD_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = alloc::format!("/dev/shm/.memfd-{}-{}", tid, id);
            match open_file_at(AT_FDCWD, &path, open_flags, create_mode) {
                Ok(file) => {
                    created = Some(file);
                    break;
                }
                Err(errno) if errno == EEXIST => {
                    last_errno = errno;
                }
                Err(errno) => return errno,
            }
        }
        match created {
            Some(file) => file,
            None => return last_errno,
        }
    };

    let initial_seals = if (flags & MFD_ALLOW_SEALING) != 0 {
        0
    } else {
        vfs::F_SEAL_SEAL
    };
    file.set_memfd_seals(Arc::new(AtomicUsize::new(initial_seals)));

    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();
    match fd_table.alloc_fd(file, (flags & MFD_CLOEXEC) != 0) {
        Ok(fd) => fd as isize,
        Err(e) => -(e as isize),
    }
}

pub fn sys_renameat2(
    olddirfd: usize,
    oldpath: *const u8,
    newdirfd: usize,
    newpath: *const u8,
    flags: u32,
) -> isize {
    use crate::fs::vfs::RENAME_NOREPLACE;
    // RENAME_SUPPORTED_FLAGS: only NOREPLACE for now
    const RENAME_SUPPORTED_FLAGS: u32 = RENAME_NOREPLACE;

    // reject unsupported flags
    if flags & !RENAME_SUPPORTED_FLAGS != 0 {
        return -(SyscallErr::EINVAL as isize);
    }

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
        "[sys_renameat2] old: dirfd={} path={}, new: dirfd={} path={}, flags={:#x}",
        olddirfd as isize, oldpath_str, newdirfd as isize, newpath_str, flags
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

    // VFS 层 RENAME_NOREPLACE 预检（目标存在即返回 EEXIST）
    if flags & RENAME_NOREPLACE != 0 {
        match new_parent.find(&new_leaf) {
            Ok(_) => return -(SyscallErr::EEXIST as isize),
            Err(SyscallErr::ENOENT) => {} // 目标不存在，继续
            Err(e) => return -(e as isize),
        }
    }

    // sticky bit check on source directory: only file owner, dir owner, or root may rename
    let old_parent_meta = match old_parent.metadata() {
        Ok(m) => m,
        Err(e) => return -(e as isize),
    };
    if old_parent_meta.mode.contains(vfs::InodeMode::S_ISVTX) {
        let (uid, _) = open_subject_ids();
        if uid != 0 && uid != old_parent_meta.uid {
            if let Ok(file_inode) = old_parent.find(&old_leaf) {
                if let Ok(file_meta) = file_inode.metadata() {
                    if uid != file_meta.uid {
                        return -(SyscallErr::EACCES as isize);
                    }
                }
            }
        }
    }

    match old_parent.rename(&old_leaf, &new_parent, &new_leaf, flags) {
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
        // Let inode try first (PTY uses internal buffer size)
        match file.inode.ioctl(cmd, arg, file.private_data()) {
            Ok(n) => return n as isize,
            Err(SyscallErr::ENOSYS) => { /* fall through */ }
            Err(e) => return -(e as isize),
        }
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
        Err(SyscallErr::ENOSYS) => ENOTTY,
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
    let dir_mode = apply_current_umask(vfs::InodeMode::from_bits_truncate(mode));
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

    if flags.contains(UnlinkatFlags::AT_REMOVEDIR) {
        let trimmed = path.trim_end_matches('/');
        if trimmed == "." || trimmed.ends_with("/.") {
            return EINVAL;
        }
    }

    let start = match resolve_start_inode(dirfd) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };
    let (uid, gid) = open_subject_ids();
    let parent_result = check_parent_search_access(&start, &path, uid, gid);
    if parent_result != SUCCESS {
        return parent_result;
    }
    let (parent, leaf) = match vfs_lookup_parent_for_start(&start, &path) {
        Ok(result) => result,
        Err(errno) => return errno,
    };
    if let Err(errno) = check_parent_write_search_access(&parent, uid, gid) {
        return errno;
    }
    // sticky bit: only file owner, dir owner, or root may delete from sticky dir
    let parent_meta = match parent.metadata() {
        Ok(m) => m,
        Err(e) => return -(e as isize),
    };
    if parent_meta.mode.contains(vfs::InodeMode::S_ISVTX) && uid != 0 && uid != parent_meta.uid
    {
        if let Ok(file_inode) = parent.find(&leaf) {
            if let Ok(file_meta) = file_inode.metadata() {
                if uid != file_meta.uid {
                    return EACCES;
                }
            }
        }
    }
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
    // Permission check: umount requires root (CAP_SYS_ADMIN)
    let task = current_task().unwrap();
    if task.acquire_inner_lock().euid != 0 {
        return EPERM;
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
    if flags.contains(UmountFlags::MNT_DETACH) {
        let target_mnt = match resolve_umount_target(&inode) {
            Ok(mnt) => mnt,
            Err(errno) => return errno,
        };
        return match target_mnt.detach_recursive() {
            Ok(()) => SUCCESS,
            Err(e) => -(e as isize),
        };
    }
    match inode.umount() {
        Ok(_) => SUCCESS,
        Err(e) => {
            error!("[sys_umount2] inode.umount() failed for '{}': errno={}", lookup_path, e as isize);
            -(e as isize)
        }
    }
}

/// Extract the target MountFS from a MountFSInode for umount/detach operations.
/// Returns the child MountFS if the inode is a mountpoint, or the MountFS itself
/// if the inode is a mountpoint root.
fn resolve_umount_target(inode: &Arc<dyn vfs::IndexNode>) -> Result<Arc<vfs::MountFS>, isize> {
    let mnt_inode = match inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
        Some(m) => m,
        None => return Err(EINVAL),
    };
    if mnt_inode.is_mountpoint_root() {
        return Ok(mnt_inode.mount_fs.clone());
    }
    let inode_id = match mnt_inode.inner_inode.metadata() {
        Ok(md) => md.inode_id,
        Err(e) => return Err(-(e as isize)),
    };
    mnt_inode.mount_fs.mountpoints.lock()
        .get(&inode_id)
        .cloned()
        .ok_or(EINVAL)
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
) -> Result<Arc<vfs::MountFS>, isize> {
    let source_path = if source.is_null() {
        return Err(EINVAL);
    } else {
        match user_cstring(token, source) {
            Ok(s) => s,
            Err(errno) => return Err(errno),
        }
    };

    let source_inode = match vfs_lookup(lookup_inode, &source_path, true) {
        Ok(inode) => inode,
        Err(errno) => {
            error!("[do_bind_mount] vfs_lookup source '{}' failed: {}", source_path, errno);
            return Err(errno);
        }
    };

    let source_mfs_inode = match source_inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
        Some(mfs) => mfs,
        None => return Err(EINVAL),
    };
    let source_mount_fs = source_mfs_inode.mount_fs.clone();
    let source_inner: Arc<dyn vfs::IndexNode> = source_mfs_inode.inner_inode.clone();

    // Reject bind mount from unbindable source
    if source_mount_fs.propagation().is_unbindable() {
        warn!("[do_bind_mount] source mount '{}' is unbindable, refusing bind", source_path);
        return Err(EINVAL);
    }

    // Collect recursive bind snapshot BEFORE creating base mount, so the
    // new mnt_fs doesn't pollute the source mount tree during snapshotting.
    // Skip unbindable submounts — they must not be replicated.
    let rbind_snapshot: Option<Vec<RbindEntry>> =
        if mountflags.contains(MountFlags::MS_REC) {
            let mut snapshot = collect_rbind_snapshot(source_mount_fs.clone(), source_inner.clone());
            snapshot.retain(|e| !e.child_mfs.propagation().is_unbindable());
            Some(snapshot)
        } else {
            None
        };

    let target_mfs_inode = match target_inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
        Some(mfs) => mfs,
        None => return Err(EINVAL),
    };

    let mnt_flags = vfs::MountFlags::from_bits_truncate(mountflags.bits() as u32);
    // Use mount_subtree_inner(false) to skip automatic propagation.
    // We'll set up the final propagation group and propagate manually,
    // ensuring peers get the correct group membership.
    let mnt_fs = match target_mfs_inode.mount_subtree_inner(
        source_mount_fs.inner_filesystem(),
        source_inner,
        mnt_flags,
        Some(alloc::string::String::from(lookup_path)),
        false,
    ) {
        Ok(fs) => fs,
        Err(e) => return Err(-(e as isize)),
    };
    mnt_fs.set_mount_source(Some(source_path));

    // Set up propagation matching the source mount.
    // The mount_subtree_inner(false) already inherited the parent's group ID
    // (if parent shared) but did NOT register in PEER_GROUPS.
    // Unconditionally unregister from parent's group, then apply source's propagation.
    let inherited_gid = mnt_fs.propagation().peer_group_id();
    if inherited_gid != 0 {
        vfs::propagation::unregister_peer_mount(&mnt_fs);
    }
    let source_prop = source_mount_fs.propagation();
    let source_is_shared = source_prop.is_shared();
    let source_gid = source_prop.peer_group_id();

    // Compute desired (peer_gid, master_gid) from source + inherited context
    let desired_peer: Option<u32> = if source_is_shared {
        Some(source_gid)
    } else if inherited_gid != 0 {
        // Private source under shared target parent → join target's child group
        Some(inherited_gid)
    } else {
        None
    };
    let desired_master: Option<u32> = if source_prop.is_slave() {
        let mgid = source_prop.master_group_id();
        if mgid != 0 { Some(mgid) } else { None }
    } else {
        None
    };

    vfs::propagation::configure_propagation_no_register(&mnt_fs, desired_peer, desired_master);

    // Propagate mount event. The propagation source is the mount that HOLDS
    // the mountpoint. When the target is itself a mount root, the new mount
    // is created inside that existing mount (chain: parent → old → new),
    // so the propagation source must be the existing mount (not its parent).
    // The walk-up heuristic below was incorrect — it switched the source to
    // the grandparent, losing the correct mountpoint inode.
    let (target_parent_mfs, target_ino) =
        if target_mfs_inode.is_mountpoint_root() {
            // target is an existing mount root → propagate FROM it
            let child_mfs = target_mfs_inode.mount_fs.clone();
            let md = match target_inode.metadata() {
                Ok(md) => md,
                Err(_) => {
                    vfs::propagation::unregister_peer_mount(&mnt_fs);
                    vfs::propagation::unregister_slave_mount(&mnt_fs);
                    mnt_fs.propagation().set_peer_group_id(0);
                    mnt_fs.propagation().set_master_group_id(0);
                    return Err(-(SyscallErr::EIO as isize));
                }
            };
            (child_mfs, md.inode_id)
        } else {
            let md = match target_inode.metadata() {
                Ok(md) => md,
                Err(_) => {
                    vfs::propagation::unregister_peer_mount(&mnt_fs);
                    vfs::propagation::unregister_slave_mount(&mnt_fs);
                    mnt_fs.propagation().set_peer_group_id(0);
                    mnt_fs.propagation().set_master_group_id(0);
                    return Err(-(SyscallErr::EIO as isize));
                }
            };
            (target_mfs_inode.mount_fs.clone(), md.inode_id)
        };
    let child_name = lookup_path
        .rsplit('/')
        .next()
        .unwrap_or("");
    vfs::propagation::propagate_mount(&target_parent_mfs, target_ino, &mnt_fs, child_name);

    // NOW register in peer/slave group AFTER propagation (prevents self-peer loop)
    vfs::propagation::register_current_propagation(&mnt_fs);

    if let Some(snapshot) = rbind_snapshot {
        if let Err(e) = apply_rbind_snapshot(
            &snapshot,
            source_mount_fs,
            mnt_fs.clone(),
            lookup_path,
        ) {
            let _ = mnt_fs.umount();
            return Err(-(e as isize));
        }
    }

    Ok(mnt_fs)
}

/// Apply an explicit propagation override to a mount and its subtree.
/// Used for combined flags like `mount --bind --make-slave`.
///
/// The mount has already been created by `do_bind_mount()` (which applied
/// source-based propagation, peer registration, and rbind snapshot).
/// This function overrides the final propagation type on top.
fn apply_propagation_change(
    mnt_fs: &Arc<vfs::MountFS>,
    prop_type: vfs::propagation::PropagationType,
    recursive: bool,
) {
    // DragonOS: delegate to set_propagation_type for correct idempotent
    // group handling (Shared→Shared no-op, SharedSlave→Slave keeps master).
    vfs::propagation::set_propagation_type(mnt_fs, prop_type);
    if recursive {
        set_propagation_recursive(mnt_fs, prop_type);
    }
}

/// Entry in rbind snapshot: records everything needed to recreate a submount
/// in the target tree without relying on name-based find().
struct RbindEntry {
    child_mfs: Arc<vfs::MountFS>,
    source_parent_mfs: Arc<vfs::MountFS>,
    child_name: alloc::string::String,
    mountpoint_id: usize,
}

/// Collect all submounts under `source_subtree_root` within `source_mfs` tree.
///
/// Returns Vec of (child_mfs, parent_mfs, relative_name) — BFS order,
/// no mutations to the mount tree.
fn collect_rbind_snapshot(
    source_mfs: Arc<vfs::MountFS>,
    source_subtree_root: Arc<dyn vfs::IndexNode>,
) -> Vec<RbindEntry> {
    const MAX_DEPTH: usize = 256;

    let mut queue: VecDeque<(Arc<vfs::MountFS>, Arc<dyn vfs::IndexNode>)> = VecDeque::new();
    let mut result: Vec<RbindEntry> = Vec::new();
    let mut seen: Vec<usize> = Vec::new();
    let root_ptr = Arc::as_ptr(&source_mfs) as usize;
    seen.push(root_ptr);

    // Seed: submounts directly reachable from source_subtree_root
    {
        let mps = source_mfs.mountpoints.lock();
        for (&ino, child_mfs) in mps.iter() {
            let ptr = Arc::as_ptr(child_mfs) as usize;
            if seen.contains(&ptr) { continue; }
            // Find the name of this mountpoint under subtree_root
            if let Ok(dirents) = source_subtree_root.list_dirents() {
                if let Some((name, _, _)) = dirents.iter().find(|(_, i, _)| *i == ino) {
                    seen.push(ptr);
                    queue.push_back((child_mfs.clone(), child_mfs.mountpoint_root_inode()));
                    result.push(RbindEntry {
                        child_mfs: child_mfs.clone(),
                        source_parent_mfs: source_mfs.clone(),
                        child_name: name.clone(),
                        mountpoint_id: ino,
                    });
                }
            }
        }
    }

    // BFS: for each child, collect its submounts (with cycle detection + depth limit)
    while let Some((child_mfs, child_root)) = queue.pop_front() {
        if seen.len() > MAX_DEPTH {
            log::error!("[collect_rbind_snapshot] max depth {} exceeded", MAX_DEPTH);
            break;
        }
        let mps = child_mfs.mountpoints.lock();
        for (&grand_ino, grandchild) in mps.iter() {
            let ptr = Arc::as_ptr(grandchild) as usize;
            if seen.contains(&ptr) { continue; }
            seen.push(ptr);
            if let Some(ref child_mfs_inode) = child_root.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
                if let Ok(dirents) = child_mfs_inode.inner_inode.list_dirents() {
                    if let Some((name, _, _)) = dirents.iter().find(|(_, i, _)| *i == grand_ino) {
                        result.push(RbindEntry {
                            child_mfs: grandchild.clone(),
                            source_parent_mfs: child_mfs.clone(),
                            child_name: name.clone(),
                            mountpoint_id: grand_ino,
                        });
                        queue.push_back((grandchild.clone(), grandchild.mountpoint_root_inode()));
                    }
                }
            }
        }
    }
    result
}

/// Recursively apply a propagation type change to all child mounts.
///
/// Uses snapshot-first BFS to avoid mutating the mount tree while iterating.
fn set_propagation_recursive(
    root: &Arc<vfs::MountFS>,
    prop_type: vfs::propagation::PropagationType,
) {
    // Snapshot all child mounts (BFS order)
    let mut children: Vec<Arc<vfs::MountFS>> = Vec::new();
    let mut queue: Vec<Arc<vfs::MountFS>> = {
        let mps = root.mountpoints.lock();
        mps.values().cloned().collect()
    };
    while let Some(child) = queue.pop() {
        children.push(child.clone());
        let mps = child.mountpoints.lock();
        for grandchild in mps.values() {
            queue.push(grandchild.clone());
        }
    }

    // Apply propagation change to each child (no locks held during iteration)
    for child in &children {
        let master_gid = if prop_type == vfs::propagation::PropagationType::Slave {
            let parent_prop = child.propagation();
            parent_prop.peer_group_id()
        } else {
            0
        };

        if prop_type == vfs::propagation::PropagationType::Slave && master_gid != 0 {
            child.propagation().set_master_group_id(master_gid);
        }
        vfs::propagation::set_propagation_type(child, prop_type);
        // registration handled inside set_propagation_type now
    }
}

/// Apply previously collected rbind snapshot to the target mount tree.
///
/// Uses source→target parent mapping (by Arc pointer) to locate the correct
/// target parent for each submount. All-or-nothing: any failure rolls back
/// all created mounts.
fn apply_rbind_snapshot(
    snapshot: &[RbindEntry],
    source_mfs: Arc<vfs::MountFS>,
    target_mfs: Arc<vfs::MountFS>,
    _target_base_path: &str,
) -> Result<(), SyscallErr> {
    let mut mnt_map: BTreeMap<usize, Arc<vfs::MountFS>> = BTreeMap::new();
    mnt_map.insert(Arc::as_ptr(&source_mfs) as usize, target_mfs.clone());

    let mut created: Vec<Arc<vfs::MountFS>> = Vec::new();

    for entry in snapshot {
        let source_parent_ptr = Arc::as_ptr(&entry.source_parent_mfs) as usize;
        let target_parent = match mnt_map.get(&source_parent_ptr) {
            Some(p) => p.clone(),
            None => {
                rollback_mounts(&created);
                return Err(SyscallErr::EINVAL);
            }
        };

        // DragonOS: use source child's self_mountpoint inner_inode to
        // construct backref in target tree — no find(child_name) needed.
        let covered_inode = entry.child_mfs.self_mountpoint()
            .map(|mp| mp.inner_inode.clone())
            .unwrap_or_else(|| entry.child_mfs.root_inner_inode());
        let target_mfs_inode = vfs::MountFSInode::new(covered_inode, target_parent.clone());

        let mount_path = match target_parent.mount_path() {
            Some(ref p) => alloc::format!("{}/{}", p, entry.child_name),
            None => alloc::format!("/{}", entry.child_name),
        };

        match target_mfs_inode.mount_subtree_inner(
            entry.child_mfs.inner_filesystem(),
            entry.child_mfs.root_inner_inode(),
            vfs::MountFlags::empty(),
            Some(mount_path),
            false,
        ) {
            Ok(new_mnt) => {
                let child_prop = entry.child_mfs.propagation();
                let child_peer = if child_prop.is_shared() {
                    Some(child_prop.peer_group_id())
                } else {
                    None
                };
                let child_master = if child_prop.is_slave() {
                    Some(child_prop.master_group_id())
                } else {
                    None
                };
                vfs::propagation::install_propagation(&new_mnt, child_peer, child_master);
                if child_prop.is_unbindable() {
                    new_mnt.propagation().set_prop_type_value(vfs::propagation::PropagationType::Unbindable);
                }
                if let Some(src) = entry.child_mfs.mount_source() {
                    new_mnt.set_mount_source(Some(src));
                }
                let child_ptr = Arc::as_ptr(&entry.child_mfs) as usize;
                mnt_map.insert(child_ptr, new_mnt.clone());
                created.push(new_mnt);
            }
            Err(e) => {
                rollback_mounts(&created);
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Check whether any mount in the subtree rooted at `root_mfs` has
/// unbindable propagation. Uses BFS with snapshot to avoid deadlocks.
/// Conservative: returns true on overflow / errors.
fn subtree_has_unbindable(root_mfs: &Arc<vfs::MountFS>) -> bool {
    const MAX_NODES: usize = 256;
    if root_mfs.propagation().is_unbindable() {
        return true;
    }
    let mut queue: Vec<Arc<vfs::MountFS>> = {
        let mps = root_mfs.mountpoints.lock();
        mps.values().cloned().collect()
    };
    let mut checked: usize = 0;
    while let Some(child) = queue.pop() {
        if child.propagation().is_unbindable() {
            return true;
        }
        checked += 1;
        if checked > MAX_NODES {
            return true;
        }
        let mps = child.mountpoints.lock();
        for grandchild in mps.values() {
            queue.push(grandchild.clone());
        }
    }
    false
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
    // Permission check: mount requires root (CAP_SYS_ADMIN)
    let task = current_task().unwrap();
    let is_root = task.acquire_inner_lock().euid == 0;
    if !is_root {
        return EPERM;
    }
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

    let propagation_type_flags = MountFlags::MS_SHARED
        | MountFlags::MS_PRIVATE | MountFlags::MS_SLAVE | MountFlags::MS_UNBINDABLE;
    let prop_type_flag = mountflags & propagation_type_flags;

    // Propagation-type-change commands (e.g., mount --make-shared /mnt)
    // MS_REC is allowed as modifier, but only when there is exactly one
    // propagation-type flag AND no MS_MOVE/MS_REMOUNT.
    // MS_BIND + single propagation flag is allowed (bind, then override).
    let bind_prop_override: Option<vfs::propagation::PropagationType> = if mountflags.intersects(MountFlags::MS_BIND) && !prop_type_flag.is_empty() {
        if prop_type_flag.bits().count_ones() != 1 {
            return EINVAL;
        }
        if prop_type_flag.contains(MountFlags::MS_SHARED) {
            Some(vfs::propagation::PropagationType::Shared)
        } else if prop_type_flag.contains(MountFlags::MS_PRIVATE) {
            Some(vfs::propagation::PropagationType::Private)
        } else if prop_type_flag.contains(MountFlags::MS_SLAVE) {
            Some(vfs::propagation::PropagationType::Slave)
        } else {
            Some(vfs::propagation::PropagationType::Unbindable)
        }
    } else {
        None
    };
    let bind_prop_override_recursive = if bind_prop_override.is_some() {
        mountflags.contains(MountFlags::MS_REC)
    } else {
        false
    };

    if !prop_type_flag.is_empty() {
        if mountflags.intersects(MountFlags::MS_MOVE | MountFlags::MS_REMOUNT)
            || (prop_type_flag.bits().count_ones() != 1 && bind_prop_override.is_none())
        {
            return EINVAL;
        }
        // Pure propagation-type-change (no BIND): apply to existing mount
        if bind_prop_override.is_none() {
            let is_recursive = mountflags.contains(MountFlags::MS_REC);
            let target_mnt_inode = match target_inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
                Some(m) => m,
                None => return EINVAL,
            };
            let prop_type = if prop_type_flag.contains(MountFlags::MS_SHARED) {
                vfs::propagation::PropagationType::Shared
            } else if prop_type_flag.contains(MountFlags::MS_PRIVATE) {
                vfs::propagation::PropagationType::Private
            } else if prop_type_flag.contains(MountFlags::MS_SLAVE) {
                vfs::propagation::PropagationType::Slave
            } else {
                vfs::propagation::PropagationType::Unbindable
            };
            let mnt = target_mnt_inode.mount_fs.clone();
            if prop_type == vfs::propagation::PropagationType::Slave {
                let parent_prop = target_mnt_inode.mount_fs.propagation();
                let master_gid = parent_prop.peer_group_id();
                mnt.propagation().set_master_group_id(master_gid);
            }
            vfs::propagation::set_propagation_type(&mnt, prop_type);
            // registration now handled inside set_propagation_type
            if is_recursive {
                set_propagation_recursive(&mnt, prop_type);
            }
            return SUCCESS;
        }
    }

    if mountflags.intersects(MountFlags::MS_BIND) {
        let mnt_fs = match do_bind_mount(source, token, &lookup_inode, &lookup_path, target_inode, mountflags) {
            Ok(fs) => fs,
            Err(errno) => return errno,
        };
        // Apply explicit propagation override if specified (e.g., --bind --make-slave)
        if let Some(prop_type) = bind_prop_override {
            apply_propagation_change(&mnt_fs, prop_type, bind_prop_override_recursive);
        }
        return SUCCESS;
    }

    if mountflags.intersects(MountFlags::MS_MOVE) {
        let source_path = match user_cstring(token, source) {
            Ok(s) => s,
            Err(errno) => return errno,
        };
        let (src_lookup_inode, src_lookup_path) = {
            let task = current_task().unwrap();
            let fs_ref = task.process.fs();
            let fs = fs_ref.lock();
            if source_path.starts_with('/') {
                let root: Arc<dyn vfs::IndexNode> = crate::fs::vfs_root().mountpoint_root_inode();
                (root, source_path)
            } else {
                let cwd: Arc<dyn vfs::IndexNode> = fs.working_inode.inode.clone();
                let path = alloc::format!("{}/{}", fs.working_path, source_path);
                (cwd, path)
            }
        };
        let src_inode = match vfs_lookup(&src_lookup_inode, &src_lookup_path, false) {
            Ok(inode) => inode,
            Err(errno) => {
                error!("[sys_mount] MS_MOVE source lookup failed: {}", errno);
                return errno;
            }
        };
        let src_mnt = match src_inode
            .as_any_ref()
            .downcast_ref::<vfs::MountFSInode>()
            .map(|m| m.mount_fs.clone())
        {
            Some(m) => m,
            None => return EINVAL,
        };

        let old_mp = match src_mnt.self_mountpoint() {
            Some(mp) => mp,
            None => return EINVAL,
        };
        let old_mp_id = match old_mp.inner_inode.metadata() {
            Ok(md) => md.inode_id,
            Err(e) => return -(e as isize),
        };
        let old_parent_mnt = old_mp.mount_fs.clone();

        let target_mnt_inode = match target_inode
            .as_any_ref()
            .downcast_ref::<vfs::MountFSInode>()
        {
            Some(m) => m,
            None => return EINVAL,
        };
        let new_parent_mnt = target_mnt_inode.mount_fs.clone();

        // Reject MS_MOVE from a shared parent: Linux forbids detaching
        // a mount from a shared tree without move-propagation support.
        if old_parent_mnt.propagation().is_shared() {
            return EINVAL;
        }

        // Reject MS_MOVE to a shared parent when the subtree contains
        // unbindable mounts: they cannot be propagated to peers.
        if new_parent_mnt.propagation().is_shared() && subtree_has_unbindable(&src_mnt) {
            return EINVAL;
        }

        // Prevent moving a mount under its own subtree (would create a cycle).
        // Walk parent chain from target: if any ancestor is src_mnt, reject.
        {
            let mut cur = Arc::clone(&new_parent_mnt);
            let mut depth: u32 = 0;
            loop {
                if Arc::ptr_eq(&cur, &src_mnt) {
                    return EINVAL;
                }
                depth += 1;
                if depth > 64 {
                    return EINVAL;
                }
                // Walk up via self_mountpoint
                let next = match cur.self_mountpoint() {
                    Some(mp) => mp.mount_fs.clone(),
                    None => break,
                };
                if Arc::ptr_eq(&next, &cur) {
                    break;
                }
                cur = next;
            }
        }

        // Save old state for rollback if new-parent add fails
        let old_path = src_mnt.mount_path();
        let old_backref = old_mp.clone();

        old_parent_mnt.remove_mount(old_mp_id);

        vfs::mount::MOUNT_LIST.remove_fs(&src_mnt);

        if let Err(e) = new_parent_mnt.add_mount(inode_id, src_mnt.clone()) {
            // Rollback: restore old parent (best-effort, must never panic)
            log::error!(
                "[sys_mount] MS_MOVE add_mount to '{}' failed (errno={}); restoring old parent",
                lookup_path, e as isize,
            );
            if let Err(rollback_err) = old_parent_mnt.add_mount(old_mp_id, src_mnt.clone()) {
                log::error!(
                    "[sys_mount] MS_MOVE rollback failed: add_mount back to old parent errno={}",
                    rollback_err as isize
                );
            } else {
                if let Some(ref old_path) = old_path {
                    vfs::mount::MOUNT_LIST.insert(old_path.as_str(), src_mnt.clone(), Some(old_mp_id));
                }
                src_mnt.set_self_mountpoint(Some(old_backref));
                src_mnt.set_mount_path(old_path);
            }
            return -(e as isize);
        }

        // Success: update to new parent
        let new_backref =
            vfs::MountFSInode::new(target_mnt_inode.inner_inode.clone(), new_parent_mnt.clone());
        src_mnt.set_self_mountpoint(Some(new_backref));

        let old_prefix = old_path.clone();
        let new_prefix = lookup_path.clone();
        src_mnt.set_mount_path(Some(new_prefix.clone()));
        vfs::mount::MOUNT_LIST.insert(new_prefix.as_str(), src_mnt.clone(), Some(inode_id));

        // MS_MOVE must also update mount_path of all descendants.
        // Without this, child mounts retain old paths (e.g., "parent2/a")
        // making them unreachable via umount and causing cleanup loops.
        {
            let mut queue: Vec<Arc<vfs::MountFS>> = {
                let mps = src_mnt.mountpoints.lock();
                mps.values().cloned().collect()
            };
            let mut seen: Vec<usize> = alloc::vec![Arc::as_ptr(&src_mnt) as usize];
            while let Some(child) = queue.pop() {
                let ptr = Arc::as_ptr(&child) as usize;
                if seen.contains(&ptr) || seen.len() > 64 {
                    continue;
                }
                seen.push(ptr);
                if let Some(ref old_child_path) = old_prefix {
                    if let Some(ref cur_path) = child.mount_path() {
                        if let Some(suffix) = cur_path.strip_prefix(old_child_path.as_str()) {
                            let new_child_path = if suffix.is_empty() {
                                new_prefix.clone()
                            } else if suffix.starts_with('/') {
                                alloc::format!("{}{}", new_prefix, suffix)
                            } else {
                                alloc::format!("{}/{}", new_prefix, suffix)
                            };
                            vfs::mount::MOUNT_LIST.remove(cur_path.as_str());
                            vfs::mount::MOUNT_LIST.insert(
                                new_child_path.as_str(), child.clone(), None,
                            );
                            child.set_mount_path(Some(new_child_path));
                        }
                    }
                }
                {
                    let mps = child.mountpoints.lock();
                    for gc in mps.values() {
                        queue.push(gc.clone());
                    }
                }
            }
        }

        // Propagate moved mount tree to new parent's peers.
        // DragonOS: mount into shared parent makes the moved root shared
        // in the parent's peer group. Ensure src_mnt has a non-zero peer
        // group before propagation so clones are created as shared.
        if new_parent_mnt.propagation().is_shared() {
            let src_peer = src_mnt.propagation().peer_group_id();
            if src_peer == 0 {
                vfs::propagation::set_propagation_type(
                    &src_mnt,
                    vfs::propagation::PropagationType::Shared,
                );
            }
            let snapshot = collect_rbind_snapshot(
                src_mnt.clone(),
                src_mnt.mountpoint_root_inode(),
            );
            let child_name = new_prefix.rsplit('/').next().unwrap_or("");
            vfs::propagation::propagate_mount(
                &new_parent_mnt, inode_id, &src_mnt, child_name,
            );
            if !snapshot.is_empty() {
                for peer in vfs::propagation::get_peers(&new_parent_mnt) {
                    let peer_clone = {
                        let mps = peer.mountpoints.lock();
                        mps.get(&inode_id).cloned()
                    };
                    if let Some(clone) = peer_clone {
                        let _ = apply_rbind_snapshot(
                            &snapshot, src_mnt.clone(), clone, &new_prefix,
                        );
                    }
                }
            }
        }

        return SUCCESS;
    }

    if mountflags.intersects(MountFlags::MS_REMOUNT) {
        let mnt_fs = target_inode
            .as_any_ref()
            .downcast_ref::<vfs::MountFSInode>()
            .map(|m| m.mount_fs.clone())
            .unwrap_or_else(|| crate::fs::vfs_root().clone());
        let remount_flags = vfs::MountFlags::from_bits_truncate(
            (mountflags.bits() & !MountFlags::MS_REMOUNT.bits()) as u32,
        );
        mnt_fs.set_mount_flags(remount_flags);
        return SUCCESS;
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

    // Use mount_subtree_inner to go through the shared-parent propagation
    // path. The raw MountFS::new() + add_mount() path would bypass child
    // peer group allocation and mount event propagation.
    let target_mfs_inode = match target_inode.as_any_ref().downcast_ref::<vfs::MountFSInode>() {
        Some(m) => m,
        None => return EINVAL,
    };

    let new_fs: Arc<dyn vfs::FileSystem> = match filesystemtype.as_str() {
        "tmpfs" => crate::fs::tmpfs::TmpFS::new_with_options(4096 * 4096), // ~16MB default
        "sysfs" => {
            let s = crate::fs::sysfs::SysFS::new();
            crate::fs::sysfs::files::register_all(s.root())
                .expect("sysfs: failed to register root entries");
            s
        }
        "proc" => {
            let p = crate::fs::procfs::ProcFS::new();
            crate::fs::procfs::files::register_all(p.root())
                .expect("procfs: failed to register root entries");
            p
        }
        _ => {
            match filesystemtype.as_str() {
                "ext2" | "ext3" | "ext4" | "vfat" | "fat32" => {
                    // 1. Resolve source device path → BlockDevice
                    let dev_inode = match vfs_lookup(&lookup_inode, &source, false) {
                        Ok(i) => i,
                        Err(errno) => return errno,
                    };
                    // Unwrap through MountFS if the inode is a mount-point wrapper
                    let dev_inode = match dev_inode
                        .as_any_ref()
                        .downcast_ref::<vfs::MountFSInode>()
                    {
                        Some(mfsi) => mfsi.inner_inode.clone(),
                        None => dev_inode,
                    };
                    let bdi = match dev_inode.as_any_ref()
                        .downcast_ref::<crate::fs::dev::block::BlockDevInode>()
                    {
                        Some(b) => b,
                        None => return -(SyscallErr::ENOTBLK as isize),
                    };
                    let blk_dev = &bdi.inner;

                    // 2. Detect actual FS type from superblock
                    let detected = crate::fs::detect_fs(blk_dev);

                    // 3. Validate FS type matches user request
                    let is_ext = matches!(filesystemtype.as_str(), "ext2" | "ext3" | "ext4");
                    match (&detected, is_ext) {
                        (crate::fs::FS_Type::Ext4, true) => {}
                        (crate::fs::FS_Type::Fat32, false) => {}
                        _ => return -(SyscallErr::EINVAL as isize),
                    }

                    // 4. Open the filesystem
                    let new_fs: Arc<dyn vfs::FileSystem> = match detected {
                        crate::fs::FS_Type::Ext4 => {
                            crate::fs::ext4::ext4fs::Ext4FileSystem::open_ext4rs(blk_dev.clone())
                        }
                        crate::fs::FS_Type::Fat32 => {
                            crate::fs::fat32::EasyFileSystem::open(blk_dev.clone())
                        }
                        _ => return -(SyscallErr::EINVAL as isize),
                    };

                    // 5. Insert into mount tree
                    let root_inode = new_fs.root_inode();
                    let mnt_flags = vfs::MountFlags::from_bits_truncate(mountflags.bits() as u32);
                    let mnt = match target_mfs_inode.mount_subtree_inner(
                        new_fs, root_inode, mnt_flags, Some(lookup_path.clone()), true,
                    ) {
                        Ok(m) => m,
                        Err(e) => return -(e as isize),
                    };
                    let _ = mnt;
                    return SUCCESS;
                }
                "exfat" | "btrfs" | "xfs" | "ntfs" => {
                    return -(SyscallErr::ENODEV as isize)
                }
                _ => return -(SyscallErr::ENODEV as isize),
            }
        }
    };
    let root_inode = new_fs.root_inode();
    let mnt_flags = vfs::MountFlags::from_bits_truncate(mountflags.bits() as u32);

    let mnt = match target_mfs_inode.mount_subtree_inner(
        new_fs, root_inode, mnt_flags, Some(lookup_path.clone()), true,
    ) {
        Ok(m) => m,
        Err(e) => return -(e as isize),
    };

    // Dynamic pseudo-fs need dentry cache disabled so hooks fire on every access
    match filesystemtype.as_str() {
        "sysfs" | "proc" => mnt.no_dentry_cache.store(true, core::sync::atomic::Ordering::Relaxed),
        _ => {}
    }

    SUCCESS
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
            if let Err(e) = _file.inode.set_metadata(&metadata) {
                return -(e as isize);
            }
        }
    }
    SUCCESS
}


fn record_flock_close(
    releases: &mut Vec<(usize, usize, usize)>,
    file: &Arc<vfs::File>,
) {
    let description = file.description_id();
    let ref_count = Arc::strong_count(file);
    if let Some((_, closed, refs)) = releases
        .iter_mut()
        .find(|(existing, _, _)| *existing == description)
    {
        *closed += 1;
        *refs = ref_count;
    } else {
        releases.push((description, 1, ref_count));
    }
}

fn release_closed_flock_descriptions(releases: Vec<(usize, usize, usize)>) {
    let mut locks = FLOCK_LOCKS.lock();
    for (description, closed, refs) in releases {
        if closed >= refs {
            locks.retain(|lock| lock.owner_description != description);
        }
    }
}

fn release_flock_description(description: usize) {
    FLOCK_LOCKS
        .lock()
        .retain(|lock| lock.owner_description != description);
}

pub fn release_flock_for_file_if_last(file: &Arc<vfs::File>) {
    let mut releases = Vec::new();
    record_flock_close(&mut releases, file);
    release_closed_flock_descriptions(releases);
}

// ── POSIX lock thin wrappers ──────────────────────────────────────────

fn fcntl_getlk(file: &vfs::File, arg: usize, _owner_pid: usize) -> isize {
    let token = current_user_token();
    let mut flock = match UserPtrMut::<PosixFlock>::from_addr(arg).read(token) {
        Ok(f) => f,
        Err(_) => return EFAULT,
    };
    let task = current_task().unwrap();
    let files = task.process.files();
    let ft = files.lock();
    let owner_id = ft.lock_owner_id();
    let owner_pid = task.pid() as i32;
    drop(ft);
    let owner = LockOwner::Posix { owner_id, owner_pid };
    match posix_lock_get(file, owner, &mut flock) {
        Ok(()) => {
            let _ = UserPtrMut::<PosixFlock>::from_addr(arg).write(token, &flock);
            SUCCESS
        }
        Err(e) => -(e as isize),
    }
}

fn fcntl_getlk_ofd(file: &vfs::File, arg: usize) -> isize {
    let token = current_user_token();
    let mut flock = match UserPtrMut::<PosixFlock>::from_addr(arg).read(token) {
        Ok(f) => f,
        Err(_) => return EFAULT,
    };
    let owner = LockOwner::Ofd { open_file_id: file.open_file_id() };
    match posix_lock_get(file, owner, &mut flock) {
        Ok(()) => {
            let _ = UserPtrMut::<PosixFlock>::from_addr(arg).write(token, &flock);
            SUCCESS
        }
        Err(e) => -(e as isize),
    }
}

fn fcntl_setlk(file: &vfs::File, arg: usize, _owner_pid: usize, wait: bool) -> isize {
    let token = current_user_token();
    let flock = match UserPtr::<PosixFlock>::from_addr(arg).read(token) {
        Ok(f) => f,
        Err(_) => return EFAULT,
    };
    let task = current_task().unwrap();
    let files = task.process.files();
    let ft = files.lock();
    let owner_id = ft.lock_owner_id();
    drop(ft);
    let owner_pid = task.pid() as i32;
    let owner = LockOwner::Posix { owner_id, owner_pid };
    match posix_lock_set(file, owner, &flock, wait) {
        Ok(()) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

fn fcntl_setlk_ofd(file: &vfs::File, arg: usize, wait: bool) -> isize {
    let token = current_user_token();
    let flock = match UserPtr::<PosixFlock>::from_addr(arg).read(token) {
        Ok(f) => f,
        Err(_) => return EFAULT,
    };
    // Linux requires l_pid == 0 for OFD locks
    if flock.l_pid != 0 {
        return -(SyscallErr::EINVAL as isize);
    }
    let owner = LockOwner::Ofd { open_file_id: file.open_file_id() };
    match posix_lock_set(file, owner, &flock, wait) {
        Ok(()) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

pub fn release_fcntl_locks_for_pid(_pid: usize) {
    // Per-file release handled by drop_fd; full shard scan deferred to Phase 4.
}

fn release_fcntl_locks_for_pid_key(_pid: usize, _key: LockKey) {
    // Per-file release handled by drop_fd.
}

pub fn close_cloexec_and_release_fcntl_locks(pid: usize, fd_table: &mut vfs::FdTable) {
    let mut flock_releases = Vec::new();
    for fd in 0..fd_table.len() {
        if fd_table.get_cloexec(fd) {
            if let Ok(file) = fd_table.get_file(fd) {
                let owner_id = fd_table.lock_owner_id();
                release_posix_for_owner(&file, owner_id);
                record_flock_close(&mut flock_releases, &file);
            }
        }
    }
    fd_table.close_cloexec();
    release_closed_flock_descriptions(flock_releases);
}

pub fn sys_fcntl(fd: usize, cmd: u32, arg: usize) -> isize {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();

    info!(
        "[sys_fcntl] fd: {}, cmd: {:?}, arg: {:X}",
        fd,
        FcntlCommand::try_from_primitive(cmd).ok(),
        arg
    );

    let command = match FcntlCommand::try_from_primitive(cmd) { Ok(c) => c, Err(_) => return -(SyscallErr::EINVAL as isize), };
    match command {
        FcntlCommand::DupFd | FcntlCommand::DupFdCloexec => {
            let cloexec = matches!(command, FcntlCommand::DupFdCloexec);
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            };

            match fd_table.alloc_fd_from(arg, file, cloexec) {
                Ok(fd) => fd as isize,
                Err(e) => -(e as isize),
            }
        }
        FcntlCommand::GetFd => {
            // Check that fd is valid first
            match fd_table.get_file(fd) { Ok(_) => {}, Err(e) => return -(e as isize), };
            fd_table.get_cloexec(fd) as isize
        }
        FcntlCommand::SetFd => {
            match fd_table.set_cloexec(fd, (arg & vfs::FD_CLOEXEC) != 0) { Ok(_) => {}, Err(e) => return -(e as isize), };
            if (arg & !vfs::FD_CLOEXEC) != 0 {
                warn!("[fcntl] Unsupported flag exists: {:X}", arg);
            }
            SUCCESS
        }
        FcntlCommand::SetFlags => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            };

            // Preserve old access mode, only update SETFL-allowed status bits
            let old_flags = file.flags();
            let old_async = old_flags.contains(vfs::FileFlags::O_ASYNC);
            let old_access = old_flags.access_flags().bits();
            const ACCMODE_MASK: u32 = 0o3;
            let arg_without_accmode = (arg as u32) & !ACCMODE_MASK;
            let new_flags = vfs::FileFlags::from_bits_truncate(arg_without_accmode | old_access);
            match file.set_flags(new_flags) {
                Ok(()) => {
                    let new_async = new_flags.contains(vfs::FileFlags::O_ASYNC);
                    if new_async != old_async {
                        let _ = vfs::fasync::set_file_fasync(&file, fd as i32, new_async);
                    }
                    SUCCESS
                }
                Err(e) => -(e as isize),
            }
        }
        FcntlCommand::GetFlags => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            };
            let bits = file.flags().bits();
            ((bits & 0o3) | (bits & vfs::STATUS_MASK)) as isize
        }
        FcntlCommand::GetLock => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            };
            let owner_pid = task.pid();
            drop(fd_table);
            fcntl_getlk(&file, arg, owner_pid)
        }
        FcntlCommand::OfdGetLock => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file.clone(),
                Err(e) => return -(e as isize),
            };
            drop(fd_table);
            fcntl_getlk_ofd(&file, arg)
        }
        FcntlCommand::SetLock
        | FcntlCommand::SetLockWait => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            };
            let owner_pid = task.pid();
            let wait = matches!(command, FcntlCommand::SetLockWait);
            drop(fd_table);
            fcntl_setlk(&file, arg, owner_pid, wait)
        }
        FcntlCommand::OfdSetLock
        | FcntlCommand::OfdSetLockWait => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file.clone(),
                Err(e) => return -(e as isize),
            };
            let wait = matches!(command, FcntlCommand::OfdSetLockWait);
            drop(fd_table);
            fcntl_setlk_ofd(&file, arg, wait)
        }
        FcntlCommand::SetPipeSize | FcntlCommand::GetPipeSize => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            };
            let pipe = match file.inode_as_any_ref().downcast_ref::<Pipe>() {
                Some(pipe) => pipe,
                None => return EINVAL,
            };
            match command {
                FcntlCommand::GetPipeSize => pipe.pipe_capacity() as isize,
                FcntlCommand::SetPipeSize => match pipe.set_pipe_capacity_compat(arg) {
                    Ok(size) => size as isize,
                    Err(e) => -(e as isize),
                },
                _ => unreachable!(),
            }
        }
        FcntlCommand::AddSeals => {
            const VALID_SEALS: usize = vfs::F_SEAL_SEAL
                | vfs::F_SEAL_SHRINK
                | vfs::F_SEAL_GROW
                | vfs::F_SEAL_WRITE
                | vfs::F_SEAL_FUTURE_WRITE;

            if (arg & !VALID_SEALS) != 0 {
                return EINVAL;
            }
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            };
            let seals = match file.memfd_seals() {
                Some(seals) => seals,
                None => return EINVAL,
            };
            if file.writable().is_err() {
                return EPERM;
            }
            let old = seals.load(Ordering::SeqCst);
            if (old & vfs::F_SEAL_SEAL) != 0 {
                return EPERM;
            }
            if (arg & vfs::F_SEAL_WRITE) != 0 {
                let inode = vfs::MountFSInode::unwrap_inode(&file.inode);
                let vm_ref = task.process.vm();
                let memory_set = vm_ref.lock();
                if memory_set.has_shared_writable_mapping(&inode) {
                    return EBUSY;
                }
            }
            seals.store(old | arg, Ordering::SeqCst);
            SUCCESS
        }
        FcntlCommand::GetSeals => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            };
            match file.memfd_seal_bits() {
                Some(seals) => seals as isize,
                None => EINVAL,
            }
        }
        FcntlCommand::SetOwn => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            let v = arg as i32;
            if v > 0 {
                file.set_owner_target(vfs::FileOwnerTarget::Pid(v as usize), v);
            } else if v < 0 {
                file.set_owner_target(vfs::FileOwnerTarget::Pgrp((-v) as usize), v);
            } else {
                file.set_owner_target(vfs::FileOwnerTarget::None, 0);
            }
            SUCCESS
        }
        FcntlCommand::GetOwn => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            file.owner_raw() as isize
        }
        FcntlCommand::SetSig => {
            if arg > 64 {
                return EINVAL;
            }
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            file.set_owner_signum(arg as i32);
            SUCCESS
        }
        FcntlCommand::GetSig => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            file.owner_signum() as isize
        }
        FcntlCommand::SetOwnEx => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            let token = current_user_token();
            let oe: vfs::FOwnerEx = match UserPtr::<vfs::FOwnerEx>::from_addr(arg)
                .read(token)
            {
                Ok(v) => v,
                Err(e) => return e,
            };
            match oe.type_ {
                vfs::F_OWNER_TID => match find_task_by_tid(oe.pid as usize) {
                    Some(t) => file.set_owner_target(vfs::FileOwnerTarget::Tid(t.tid.0), oe.pid),
                    None => return -(SyscallErr::ESRCH as isize),
                },
                vfs::F_OWNER_PID => {
                    file.set_owner_target(vfs::FileOwnerTarget::Pid(oe.pid as usize), oe.pid);
                }
                vfs::F_OWNER_PGRP => {
                    file.set_owner_target(vfs::FileOwnerTarget::Pgrp(oe.pid as usize), oe.pid)
                }
                _ => return EINVAL,
            }
            SUCCESS
        }
        FcntlCommand::GetOwnEx => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            let token = current_user_token();
            let s = file.owner_snapshot();
            let t = match &s.target {
                vfs::FileOwnerTarget::None | vfs::FileOwnerTarget::Pid(_) => vfs::F_OWNER_PID,
                vfs::FileOwnerTarget::Pgrp(_) => vfs::F_OWNER_PGRP,
                vfs::FileOwnerTarget::Tid(_) => vfs::F_OWNER_TID,
            };
            let pid = file.owner_raw();
            let _ = UserPtrMut::<vfs::FOwnerEx>::from_addr(arg)
                .write(token, &vfs::FOwnerEx { type_: t, pid });
            SUCCESS
        }
        FcntlCommand::GetOwnerUids => ENOSYS,
        FcntlCommand::SetLease => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            let t = arg as i16;
            use crate::fs::vfs::fcntl::{F_RDLCK, F_WRLCK, F_UNLCK};
            match t {
                F_RDLCK => {
                    if !file.flags().is_readable() {
                        return -(SyscallErr::EAGAIN as isize);
                    }
                    if !file.flags().is_read_only()
                        || is_writable_inode_busy(&file.inode)
                    {
                        return -(SyscallErr::EAGAIN as isize);
                    }
                    *file.lease.lock() = Some(F_RDLCK);
                    SUCCESS
                }
                F_WRLCK => -(SyscallErr::EAGAIN as isize),
                F_UNLCK => {
                    *file.lease.lock() = None;
                    SUCCESS
                }
                _ => EINVAL,
            }
        }
        FcntlCommand::GetLease => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            let lease_val = *file.lease.lock();
            lease_val.unwrap_or(F_UNLCK) as isize
        }
        FcntlCommand::Notify => ENOSYS,
        FcntlCommand::CreatedQuery => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            if file.created_by_open() { 1 } else { 0 }
        }
        FcntlCommand::CancelLock => ENOSYS,
        FcntlCommand::GetRwHint | FcntlCommand::GetFileRwHint => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            let token = current_user_token();
            let v = *file.file_rw_hint.lock();
            match UserPtrMut::<u64>::from_addr(arg).write(token, &v) {
                Ok(()) => SUCCESS,
                Err(e) => e,
            }
        }
        FcntlCommand::SetRwHint | FcntlCommand::SetFileRwHint => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            let token = current_user_token();
            match UserPtr::<u64>::from_addr(arg).read(token) {
                Ok(v) => {
                    *file.file_rw_hint.lock() = v;
                    SUCCESS
                }
                Err(e) => e,
            }
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
pub fn sys_umask(mask: u32) -> isize {
    info!("[sys_umask] mask: {:o}", mask);
    let task = current_task().unwrap();
    let fs_ref = task.process.fs();
    let mut fs = fs_ref.lock();
    let old_mask = fs.umask & 0o777;
    fs.umask = mask & 0o777;
    old_mask as isize
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
        crate::fs::current_root_inode()
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
        crate::fs::current_root_inode()
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

pub fn sys_fadvise64(fd: usize, offset: usize, len: usize, advice: i32) -> isize {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };

    if offset_is_negative(offset) || offset_is_negative(len) {
        return EINVAL;
    }
    if !(0..=5).contains(&advice) {
        return EINVAL;
    }
    if is_stream_file(&file) {
        return ESPIPE;
    }

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
        if let Err(errno) = check_memfd_truncate_seals(&*file, length as usize) {
            return errno;
        }
        file.inode.clone()
    };
    // RLIMIT_FSIZE check
    let fsize_limit = {
        let inner = task.acquire_inner_lock();
        inner.fsize_limit_cur
    };
    if (length as usize) > fsize_limit {
        return EFBIG;
    }
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
    // Check parent directory search permission before lookup (correct errno order)
    let (uid, gid) = open_subject_ids();
    let start = if path.starts_with('/') {
        crate::fs::current_root_inode()
    } else {
        cwd_inode.clone()
    };
    let parent_result = check_parent_search_access(&start, &path, uid, gid);
    if parent_result != SUCCESS {
        return parent_result;
    }
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
    // Check RLIMIT_FSIZE
    let fsize_limit = {
        let inner = task.acquire_inner_lock();
        inner.fsize_limit_cur
    };
    if (length as usize) > fsize_limit {
        return EFBIG;
    }
    // Check write permission on the file itself
    if uid != 0 && (permission_class_bits(&md, uid, gid) & 0o2) == 0 {
        return EACCES;
    }
    match inode.resize(length as usize) {
        Ok(()) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

pub fn sys_fallocate(fd: usize, mode: u32, offset: isize, len: isize) -> isize {
    const FALLOC_FL_KEEP_SIZE: u32 = 0x01;
    const FALLOC_FL_PUNCH_HOLE: u32 = 0x02;
    const FALLOC_PUNCH_HOLE_KEEP_SIZE: u32 = FALLOC_FL_KEEP_SIZE | FALLOC_FL_PUNCH_HOLE;

    if offset < 0 || len <= 0 {
        return EINVAL;
    }

    let end = match offset.checked_add(len) {
        Some(end) => end,
        None => return EINVAL,
    };

    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let (file, is_regular) = {
        let fd_table = files_ref.lock();
        let file = match fd_table.get_file(fd) {
            Ok(file) => file,
            Err(e) => return -(e as isize),
        };
        let is_regular = file.file_type() == FileType::File;
        (file, is_regular)
    };
    drop(files_ref);

    // ENODEV if not a regular file
    if !is_regular {
        return ENODEV;
    }

    info!(
        "[sys_fallocate] fd: {}, mode: {:#x}, offset: {}, len: {}, end: {}",
        fd, mode, offset, len, end
    );

    // Supported modes: 0 (allocate), FALLOC_FL_KEEP_SIZE (allocate keep size),
    // FALLOC_PUNCH_HOLE|KEEP_SIZE (punch hole, no-op)
    if mode == FALLOC_PUNCH_HOLE_KEEP_SIZE {
        let seals = file.memfd_seal_bits().unwrap_or(0);
        if (seals & vfs::F_SEAL_WRITE) != 0 {
            return EOPNOTSUPP;
        }
        return SUCCESS;
    }
    if mode != 0 && mode != FALLOC_FL_KEEP_SIZE {
        warn!("[sys_fallocate] unsupported mode: {:#x}", mode);
        return EOPNOTSUPP;
    }

    if mode == 0 {
        if file.get_size() >= end as usize {
            return SUCCESS;
        }
        if let Some(seals) = file.memfd_seal_bits() {
            if (seals & vfs::F_SEAL_GROW) != 0 {
                return EPERM;
            }
        }
        match file.truncate_size(end as usize) {
            Ok(()) => SUCCESS,
            Err(e) => -(e as isize),
        }
    } else {
        // mode == FALLOC_FL_KEEP_SIZE: allocate without extending file size.
        // No actual block preallocation support, just return success.
        SUCCESS
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

    if let Err(errno) = validate_path_len(&target_str) {
        return errno;
    }
    if let Err(errno) = validate_path_len(&linkpath_str) {
        return errno;
    }

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

    let (uid, gid) = open_subject_ids();
    let search_result = check_parent_search_access(&start, &linkpath_str, uid, gid);
    if search_result != SUCCESS {
        return search_result;
    }

    let components = crate::fs::parse_path(&linkpath_str);
    let leaf = match components.last() {
        Some(n) => n.clone(),
        None => return ENOENT,
    };

    let parent_dir = if components.len() == 1 {
        if linkpath_str.starts_with('/') {
            crate::fs::current_root_inode()
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

    if let Err(errno) = check_parent_write_search_access(&parent_dir, uid, gid) {
        return errno;
    }

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
    flags: u32,
) -> isize {
    if flags & !(AT_SYMLINK_FOLLOW | AT_EMPTY_PATH) != 0 {
        return EINVAL;
    }

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

    // Linux: when path is absolute, dirfd is ignored — skip fd resolution
    let old_start = if oldpath_str.starts_with('/') {
        crate::fs::current_root_inode()
    } else {
        match resolve_start_inode(olddirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
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
        return EPERM;
    }

    // 解析新路径：获取父目录 + 叶子名
    // Linux: when path is absolute, dirfd is ignored — skip fd resolution
    let new_start = if newpath_str.starts_with('/') {
        crate::fs::current_root_inode()
    } else {
        match resolve_start_inode(newdirfd) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        }
    };

    // Resolve target parent directory and leaf name
    let components = crate::fs::parse_path(&newpath_str);
    let leaf = if let Some(n) = components.last() {
        n.clone()
    } else {
        return ENOENT;
    };

    let parent_dir = if components.len() == 1 {
        if newpath_str.starts_with('/') {
            crate::fs::current_root_inode()
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

    // Check if leaf already exists in parent directory (mandated by Linux link(2): EEXIST)
    // Use list_dirents() instead of find() — find can miss entries in some VFS edge cases
    if let Ok(entries) = parent_dir.list_dirents() {
        for (name, _ino, _ftype) in &entries {
            if name == &leaf {
                return EEXIST;
            }
        }
    }

    match parent_dir.link(&leaf, &existing) {
        Ok(_) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

// ── xattr (Extended Attributes) ───────────────────────────────────────

const XATTR_USER_PREFIX: &str = "user.";

fn validate_xattr_name(name: &str) -> Result<&str, isize> {
    if !name.starts_with("user.") {
        return Err(ENOTSUP);
    }
    if name.len() <= XATTR_USER_PREFIX.len() {
        return Err(EINVAL);
    }
    if name.len() > 255 {
        return Err(ERANGE);
    }
    Ok(name)
}

fn validate_xattr_flags(flags: u32) -> Result<(), isize> {
    const XATTR_CREATE: u32 = 1;
    const XATTR_REPLACE: u32 = 2;
    match flags {
        0 | XATTR_CREATE | XATTR_REPLACE => Ok(()),
        _ => Err(EINVAL),
    }
}

fn resolve_path_inode(path: &str, follow_final: bool) -> Result<Arc<dyn vfs::IndexNode>, isize> {
    if let Err(errno) = validate_path_len(path) {
        return Err(errno);
    }
    let start = match resolve_start_inode(AT_FDCWD) {
        Ok(inode) => inode,
        Err(errno) => return Err(errno),
    };
    let (uid, gid) = open_subject_ids();
    let perm_err = check_parent_search_access(&start, path, uid, gid);
    if perm_err != SUCCESS {
        return Err(perm_err);
    }
    let inode = crate::fs::vfs_lookup(&start, path, follow_final)?;

    // xattrs are only valid on regular files and directories
    if let Ok(md) = inode.metadata() {
        if md.file_type != FileType::File && md.file_type != FileType::Dir {
            return Err(EOPNOTSUPP);
        }
    }

    Ok(inode)
}

fn fd_to_inode(fd: usize) -> Result<Arc<dyn vfs::IndexNode>, isize> {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = fd_table.get_file(fd).map_err(|e| -(e as isize))?;
    let inode = file.inode.clone();
    drop(fd_table);

    // xattrs are only valid on regular files and directories
    if file.file_type() != FileType::File && file.file_type() != FileType::Dir {
        return Err(EOPNOTSUPP);
    }

    Ok(inode)
}

pub fn sys_setxattr(
    path: *const u8,
    name: *const u8,
    value: *const u8,
    size: usize,
    flags: u32,
) -> isize {
    let token = current_user_token();
    let path_str = match user_cstring(token, path) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let name_str = match user_cstring(token, name) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let validated = match validate_xattr_name(&name_str) {
        Ok(n) => n,
        Err(e) => return e,
    };
    if let Err(e) = validate_xattr_flags(flags) {
        return e;
    }
    let value_slice = if size > 0 {
        let reader = match UserBufferReader::new(token, value, size) {
            Ok(r) => r,
            Err(e) => return e,
        };
        match reader.read_to_vec(crate::hal::MAX_RW_COUNT) {
            Ok(v) => v,
            Err(e) => return e,
        }
    } else {
        alloc::vec![]
    };

    let inode = match resolve_path_inode(&path_str, true) {
        Ok(i) => i,
        Err(e) => return e,
    };

    match inode.setxattr(validated, &value_slice, flags) {
        Ok(_) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

pub fn sys_lsetxattr(
    path: *const u8,
    name: *const u8,
    value: *const u8,
    size: usize,
    flags: u32,
) -> isize {
    let token = current_user_token();
    let path_str = match user_cstring(token, path) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let name_str = match user_cstring(token, name) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let validated = match validate_xattr_name(&name_str) {
        Ok(n) => n,
        Err(e) => return e,
    };
    if let Err(e) = validate_xattr_flags(flags) {
        return e;
    }
    let value_slice = if size > 0 {
        let reader = match UserBufferReader::new(token, value, size) {
            Ok(r) => r,
            Err(e) => return e,
        };
        match reader.read_to_vec(crate::hal::MAX_RW_COUNT) {
            Ok(v) => v,
            Err(e) => return e,
        }
    } else {
        alloc::vec![]
    };

    let inode = match resolve_path_inode(&path_str, false) {
        Ok(i) => i,
        Err(e) => return e,
    };

    match inode.setxattr(validated, &value_slice, flags) {
        Ok(_) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

pub fn sys_fsetxattr(fd: usize, name: *const u8, value: *const u8, size: usize, flags: u32) -> isize {
    let token = current_user_token();
    let name_str = match user_cstring(token, name) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let validated = match validate_xattr_name(&name_str) {
        Ok(n) => n,
        Err(e) => return e,
    };
    if let Err(e) = validate_xattr_flags(flags) {
        return e;
    }
    let value_slice = if size > 0 {
        let reader = match UserBufferReader::new(token, value, size) {
            Ok(r) => r,
            Err(e) => return e,
        };
        match reader.read_to_vec(crate::hal::MAX_RW_COUNT) {
            Ok(v) => v,
            Err(e) => return e,
        }
    } else {
        alloc::vec![]
    };
    let inode = match fd_to_inode(fd) {
        Ok(i) => i,
        Err(e) => return e,
    };

    match inode.setxattr(validated, &value_slice, flags) {
        Ok(_) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

pub fn sys_getxattr(
    path: *const u8,
    name: *const u8,
    value: *mut u8,
    size: usize,
) -> isize {
    let token = current_user_token();
    let path_str = match user_cstring(token, path) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let name_str = match user_cstring(token, name) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let validated = match validate_xattr_name(&name_str) {
        Ok(n) => n,
        Err(e) => return e,
    };

    let inode = match resolve_path_inode(&path_str, true) {
        Ok(i) => i,
        Err(e) => return e,
    };

    if size == 0 {
        let mut dummy = [];
        match inode.getxattr(validated, &mut dummy) {
            Ok(len) => return len as isize,
            Err(e) => return -(e as isize),
        }
    }

    let buf_size = size.min(crate::hal::MAX_RW_COUNT);
    let mut kernel_buf = alloc::vec![0u8; buf_size];
    match inode.getxattr(validated, &mut kernel_buf) {
        Ok(len) => {
            let mut writer = match UserBufferWriter::new(token, value, len) {
                Ok(w) => w,
                Err(e) => return e,
            };
            match writer.write_from(&kernel_buf[..len]) {
                Ok(_) => len as isize,
                Err(e) => e,
            }
        }
        Err(e) => -(e as isize),
    }
}

pub fn sys_lgetxattr(
    path: *const u8,
    name: *const u8,
    value: *mut u8,
    size: usize,
) -> isize {
    let token = current_user_token();
    let path_str = match user_cstring(token, path) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let name_str = match user_cstring(token, name) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let validated = match validate_xattr_name(&name_str) {
        Ok(n) => n,
        Err(e) => return e,
    };

    let inode = match resolve_path_inode(&path_str, false) {
        Ok(i) => i,
        Err(e) => return e,
    };

    if size == 0 {
        let mut dummy = [];
        match inode.getxattr(validated, &mut dummy) {
            Ok(len) => return len as isize,
            Err(e) => return -(e as isize),
        }
    }

    let buf_size = size.min(crate::hal::MAX_RW_COUNT);
    let mut kernel_buf = alloc::vec![0u8; buf_size];
    match inode.getxattr(validated, &mut kernel_buf) {
        Ok(len) => {
            let mut writer = match UserBufferWriter::new(token, value, len) {
                Ok(w) => w,
                Err(e) => return e,
            };
            match writer.write_from(&kernel_buf[..len]) {
                Ok(_) => len as isize,
                Err(e) => e,
            }
        }
        Err(e) => -(e as isize),
    }
}

pub fn sys_fgetxattr(fd: usize, name: *const u8, value: *mut u8, size: usize) -> isize {
    let token = current_user_token();
    let name_str = match user_cstring(token, name) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let validated = match validate_xattr_name(&name_str) {
        Ok(n) => n,
        Err(e) => return e,
    };

    let inode = match fd_to_inode(fd) {
        Ok(i) => i,
        Err(e) => return e,
    };

    if size == 0 {
        let mut dummy = [];
        match inode.getxattr(validated, &mut dummy) {
            Ok(len) => return len as isize,
            Err(e) => return -(e as isize),
        }
    }

    let buf_size = size.min(crate::hal::MAX_RW_COUNT);
    let mut kernel_buf = alloc::vec![0u8; buf_size];
    match inode.getxattr(validated, &mut kernel_buf) {
        Ok(len) => {
            let mut writer = match UserBufferWriter::new(token, value, len) {
                Ok(w) => w,
                Err(e) => return e,
            };
            match writer.write_from(&kernel_buf[..len]) {
                Ok(_) => len as isize,
                Err(e) => e,
            }
        }
        Err(e) => -(e as isize),
    }
}

pub fn sys_listxattr(path: *const u8, list: *mut u8, size: usize) -> isize {
    let token = current_user_token();
    let path_str = match user_cstring(token, path) {
        Ok(s) => s,
        Err(e) => return e,
    };

    let inode = match resolve_path_inode(&path_str, true) {
        Ok(i) => i,
        Err(e) => return e,
    };

    if size == 0 {
        let mut dummy = [];
        match inode.listxattr(&mut dummy) {
            Ok(len) => return len as isize,
            Err(e) => return -(e as isize),
        }
    }

    let buf_size = size.min(crate::hal::MAX_RW_COUNT);
    let mut kernel_buf = alloc::vec![0u8; buf_size];
    match inode.listxattr(&mut kernel_buf) {
        Ok(len) => {
            let mut writer = match UserBufferWriter::new(token, list, len) {
                Ok(w) => w,
                Err(e) => return e,
            };
            match writer.write_from(&kernel_buf[..len]) {
                Ok(_) => len as isize,
                Err(e) => e,
            }
        }
        Err(e) => -(e as isize),
    }
}

pub fn sys_llistxattr(path: *const u8, list: *mut u8, size: usize) -> isize {
    let token = current_user_token();
    let path_str = match user_cstring(token, path) {
        Ok(s) => s,
        Err(e) => return e,
    };

    let inode = match resolve_path_inode(&path_str, false) {
        Ok(i) => i,
        Err(e) => return e,
    };

    if size == 0 {
        let mut dummy = [];
        match inode.listxattr(&mut dummy) {
            Ok(len) => return len as isize,
            Err(e) => return -(e as isize),
        }
    }

    let buf_size = size.min(crate::hal::MAX_RW_COUNT);
    let mut kernel_buf = alloc::vec![0u8; buf_size];
    match inode.listxattr(&mut kernel_buf) {
        Ok(len) => {
            let mut writer = match UserBufferWriter::new(token, list, len) {
                Ok(w) => w,
                Err(e) => return e,
            };
            match writer.write_from(&kernel_buf[..len]) {
                Ok(_) => len as isize,
                Err(e) => e,
            }
        }
        Err(e) => -(e as isize),
    }
}

pub fn sys_flistxattr(fd: usize, list: *mut u8, size: usize) -> isize {
    let token = current_user_token();
    let inode = match fd_to_inode(fd) {
        Ok(i) => i,
        Err(e) => return e,
    };

    if size == 0 {
        let mut dummy = [];
        match inode.listxattr(&mut dummy) {
            Ok(len) => return len as isize,
            Err(e) => return -(e as isize),
        }
    }

    let buf_size = size.min(crate::hal::MAX_RW_COUNT);
    let mut kernel_buf = alloc::vec![0u8; buf_size];
    match inode.listxattr(&mut kernel_buf) {
        Ok(len) => {
            let mut writer = match UserBufferWriter::new(token, list, len) {
                Ok(w) => w,
                Err(e) => return e,
            };
            match writer.write_from(&kernel_buf[..len]) {
                Ok(_) => len as isize,
                Err(e) => e,
            }
        }
        Err(e) => -(e as isize),
    }
}

pub fn sys_removexattr(path: *const u8, name: *const u8) -> isize {
    let token = current_user_token();
    let path_str = match user_cstring(token, path) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let name_str = match user_cstring(token, name) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let validated = match validate_xattr_name(&name_str) {
        Ok(n) => n,
        Err(e) => return e,
    };

    let inode = match resolve_path_inode(&path_str, true) {
        Ok(i) => i,
        Err(e) => return e,
    };

    match inode.removexattr(validated) {
        Ok(_) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

pub fn sys_lremovexattr(path: *const u8, name: *const u8) -> isize {
    let token = current_user_token();
    let path_str = match user_cstring(token, path) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let name_str = match user_cstring(token, name) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let validated = match validate_xattr_name(&name_str) {
        Ok(n) => n,
        Err(e) => return e,
    };

    let inode = match resolve_path_inode(&path_str, false) {
        Ok(i) => i,
        Err(e) => return e,
    };

    match inode.removexattr(validated) {
        Ok(_) => SUCCESS,
        Err(e) => -(e as isize),
    }
}

pub fn sys_fremovexattr(fd: usize, name: *const u8) -> isize {
    let token = current_user_token();
    let name_str = match user_cstring(token, name) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let validated = match validate_xattr_name(&name_str) {
        Ok(n) => n,
        Err(e) => return e,
    };

    let inode = match fd_to_inode(fd) {
        Ok(i) => i,
        Err(e) => return e,
    };

    match inode.removexattr(validated) {
        Ok(_) => SUCCESS,
        Err(e) => -(e as isize),
    }
}
