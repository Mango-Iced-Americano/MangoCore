use super::errno::*;
use crate::fs::iov::IOVec;
use crate::fs::poll::{ppoll, pselect, FdSet, PollFd};
use crate::fs::vfs::{self, FileFlags, FileType, SeekFrom};
use crate::fs::*;
use crate::hal::BLOCK_SZ;
use crate::mm::{
    copy_from_user, copy_from_user_array, copy_to_user, copy_to_user_array, copy_to_user_string,
    translated_byte_buffer, translated_byte_buffer_append_to_existing_vec, translated_ref,
    translated_refmut, translated_str, try_get_from_user, MapPermission, UserAccess, UserBuffer,
    VirtAddr,
};
use crate::syscall::utils::wait_io_core;
use crate::task::{current_task, current_user_token, signal, WaitQueue};
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::mem::size_of;
use core::panic;
use log::{debug, info, trace, warn};
use num_enum::FromPrimitive;
use smoltcp::socket;

// 防止用户传入过大参数导致内核 OOM 或者长时间阻塞
const MAX_SYSCALL_BUFFER_SIZE: usize = 2 * 1024 * 1024; // 限制为 2 MiB

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
        AT_FDCWD => task.fs.lock().working_inode.inode.clone(),
        fd => {
            let fd_table = task.files.lock();
            fd_table.get_file(fd).map_err(|e| -(e as isize))?.inode.clone()
        }
    })
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
    let fd_table = task.files.lock();
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
    // turn a pointer in user space into a pointer in kernel space if it is not null
    let off_in = if off_in.is_null() {
        off_in
    } else {
        match translated_refmut(token, off_in) {
            Ok(offset) => {
                if (*offset as isize) < 0 {
                    return EINVAL;
                };
                offset as *mut usize
            }
            Err(errno) => return errno,
        }
    };
    let off_out = if off_out.is_null() {
        off_out
    } else {
        match translated_refmut(token, off_out) {
            Ok(offset) => {
                if (*offset as isize) < 0 {
                    return EINVAL;
                };
                offset as *mut usize
            }
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
                let read_size = unsafe {
                    if let Some(ref mut off_ptr) = off_in.as_mut() {
                        let off_val = **off_ptr;
                        let n = match in_file.inode.read_at(
                            off_val,
                            buffer.len(),
                            buffer.as_mut_slice(),
                            in_file.private_data(),
                        ) {
                            Ok(n) => n,
                            Err(e) => return -(e as isize),
                        };
                        **off_ptr += n;
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

        let write_size = unsafe {
            if let Some(ref mut off_ptr) = off_out.as_mut() {
                let off_val = **off_ptr;
                let n = match out_file.inode.write_at(
                    off_val,
                    write_buffer.len(),
                    write_buffer,
                    out_file.private_data(),
                ) {
                    Ok(n) => n,
                    Err(e) => return -(e as isize),
                };
                **off_ptr += n;
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
    info!("[sys_sendfile] send bytes: {}", send_size);
    send_size as isize
}

/// # Warning
/// `fs` & `files` is locked in this function
fn __openat(dirfd: usize, path: &str) -> Result<vfs::File, isize> {
    let start = resolve_start_inode(dirfd)?;
    let target = crate::fs::vfs_lookup(&start, path, true)?;
    vfs::File::new(target, vfs::FileFlags::O_RDONLY).map_err(|e| -(e as isize))
}

pub fn sys_getcwd(buf: usize, size: usize) -> isize {
    let task = current_task().unwrap();
    if !task
        .vm
        .lock()
        .contains_valid_buffer(buf, size, MapPermission::W)
    {
        // buf points to a bad address.
        return EFAULT;
    }
    if size == 0 && buf != 0 {
        // The size argument is zero and buf is not a NULL pointer.
        return EINVAL;
    }
    let working_dir = match task.fs.lock().working_inode.get_cwd() {
        Some(s) => s,
        None => {
            log::error!("[sys_getcwd] failed to resolve cwd absolute path");
            return ENOENT;
        }
    };
    if working_dir.len() >= size {
        // The size argument is less than the length of the absolute pathname of the working directory,
        // including the terminating null byte.
        return ERANGE;
    }
    let token = task.get_user_token();
    let write_len = working_dir.len() + 1;
    let mut user_buf = UserBuffer::new({
        match translated_byte_buffer(token, buf as *const u8, write_len, UserAccess::Write) {
            Ok(buffer) => buffer,
            Err(errno) => return errno,
        }
    });
    user_buf.write(working_dir.as_bytes());
    user_buf.write_at(working_dir.len(), b"\0");
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
    let fd_table = task.files.lock();
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
        let fd_table = task.files.lock();
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
        let user_buf = match translated_byte_buffer(token, buf as *const u8, count, UserAccess::Write) {
            Ok(buffer) => UserBuffer::new(buffer),
            Err(errno) => return errno as isize,
        };
        match file.read_user(None, user_buf) {
            Ok(n) => n as isize,
            Err(e) => -(e as isize),
        }
    } else if let Some(wq) = file.inode.read_wait_queue() {
        match WaitQueue::wait_until_interruptible(wq, || {
            let user_buf = match translated_byte_buffer(token, buf as *const u8, count, UserAccess::Write) {
                Ok(buffer) => UserBuffer::new(buffer),
                Err(errno) => return Some(errno as isize),
            };
            match file.read_user(None, user_buf) {
                Ok(n) => Some(n as isize),
                Err(SyscallErr::EAGAIN) => None,
                Err(e) => Some(-(e as isize)),
            }
        }) {
            Ok(n) => n,
            Err(n) => n,
        }
    } else {
        wait_io_core(
            || {
                let user_buf = match translated_byte_buffer(token, buf as *const u8, count, UserAccess::Write) {
                    Ok(buffer) => UserBuffer::new(buffer),
                    Err(errno) => return errno as isize,
                };
                match file.read_user(None, user_buf) {
                    Ok(n) => n as isize,
                    Err(e) => -(e as isize),
                }
            },
            is_nonblock,
        )
    }
}

pub fn sys_write(fd: usize, buf: usize, count: usize) -> isize {
    let count = count.min(MAX_SYSCALL_BUFFER_SIZE);
    let task = current_task().unwrap();
    let file = {
        let fd_table = task.files.lock();
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
        let user_buf = match translated_byte_buffer(token, buf as *const u8, count, UserAccess::Read) {
            Ok(buffer) => UserBuffer::new(buffer),
            Err(errno) => return errno as isize,
        };
        match file.write_user(None, user_buf) {
            Ok(n) => n as isize,
            Err(e) => -(e as isize),
        }
    } else if let Some(wq) = file.inode.write_wait_queue() {
        match WaitQueue::wait_until_interruptible(wq, || {
            let user_buf = match translated_byte_buffer(token, buf as *const u8, count, UserAccess::Read) {
                Ok(buffer) => UserBuffer::new(buffer),
                Err(errno) => return Some(errno as isize),
            };
            match file.write_user(None, user_buf) {
                Ok(n) => Some(n as isize),
                Err(SyscallErr::EAGAIN) => None,
                Err(e) => Some(-(e as isize)),
            }
        }) {
            Ok(n) => n,
            Err(n) => n,
        }
    } else {
        wait_io_core(
            || {
                let user_buf = match translated_byte_buffer(token, buf as *const u8, count, UserAccess::Read) {
                    Ok(buffer) => UserBuffer::new(buffer),
                    Err(errno) => return errno as isize,
                };
                match file.write_user(None, user_buf) {
                    Ok(n) => n as isize,
                    Err(e) => -(e as isize),
                }
            },
            is_nonblock,
        )
    }
}

pub fn sys_pread(fd: usize, buf: usize, count: usize, offset: usize) -> isize {
    let count = count.min(MAX_SYSCALL_BUFFER_SIZE);
    let task = current_task().unwrap();
    let fd_table = task.files.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    // fd is not open for reading
    if file.readable().is_err() {
        return EBADF;
    }
    let token = task.get_user_token();
    match file.read_user(
        Some(offset),
        UserBuffer::new({
            match translated_byte_buffer(token, buf as *const u8, count, UserAccess::Write) {
                Ok(buffer) => buffer,
                Err(errno) => return errno,
            }
        }),
    ) {
        Ok(n) => n as isize,
        Err(e) => -(e as isize),
    }
}

pub fn sys_pwrite(fd: usize, buf: usize, count: usize, offset: usize) -> isize {
    let count = count.min(MAX_SYSCALL_BUFFER_SIZE);
    let task = current_task().unwrap();
    let fd_table = task.files.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    // fd is not open for writing
    if file.writable().is_err() {
        return EBADF;
    }
    let token = task.get_user_token();
    match file.write_user(
        Some(offset),
        UserBuffer::new({
            match translated_byte_buffer(token, buf as *const u8, count, UserAccess::Read) {
                Ok(buffer) => buffer,
                Err(errno) => return errno,
            }
        }),
    ) {
        Ok(n) => n as isize,
        Err(e) => -(e as isize),
    }
}

pub fn sys_readv(fd: usize, iov: usize, iovcnt: usize) -> isize {
    if iovcnt > 1024 {
        return EINVAL;
    }
    let task = current_task().unwrap();
    let fd_table = task.files.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    // fd is not open for reading
    if file.readable().is_err() {
        return EBADF;
    }
    let token = task.get_user_token();
    let mut iovecs = Vec::<IOVec>::with_capacity(iovcnt);
    if copy_from_user_array(token, iov as *const IOVec, iovecs.as_mut_ptr(), iovcnt).is_err() {
        // See read(2), which the ERRORS section of readv is written in addition to.
        log::error!("[readv] Failed to copy from {:?}", iov);
        return EFAULT;
    };
    unsafe { iovecs.set_len(iovcnt) };
    if validate_iovec_total_len(&iovecs).is_err() {
        return EINVAL;
    }
    match file.read_user(
        None,
        UserBuffer::new({
            let mut vec = Vec::with_capacity(32);
            let mut total_len = 0;
            for iovec in iovecs.iter() {
                let mut iov_len = iovec.iov_len;
                if total_len + iov_len > MAX_SYSCALL_BUFFER_SIZE {
                    iov_len = MAX_SYSCALL_BUFFER_SIZE - total_len;
                }
                if iov_len == 0 {
                    continue;
                }
                match translated_byte_buffer_append_to_existing_vec(
                    &mut vec,
                    token,
                    iovec.iov_base,
                    iov_len,
                    UserAccess::Write,
                ) {
                    Ok(_) => {
                        total_len += iov_len;
                        continue;
                    }
                    Err(errno) => return errno,
                }
            }
            vec
        }),
    ) {
        Ok(n) => n as isize,
        Err(e) => -(e as isize),
    }
}

pub fn sys_writev(fd: usize, iov: usize, iovcnt: usize) -> isize {
    if iovcnt > 1024 {
        return EINVAL;
    }
    let task = current_task().unwrap();
    let fd_table = task.files.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    // fd is not open for writing
    if file.writable().is_err() {
        return EBADF;
    }
    let token = task.get_user_token();
    let mut iovecs = Vec::<IOVec>::with_capacity(iovcnt);
    if copy_from_user_array(token, iov as *const IOVec, iovecs.as_mut_ptr(), iovcnt).is_err() {
        log::error!("[writev] Failed to copy from {:?}", iov);
        return EFAULT;
    };
    unsafe { iovecs.set_len(iovcnt) };
    if validate_iovec_total_len(&iovecs).is_err() {
        return EINVAL;
    }
    match file.write_user(
        None,
        UserBuffer::new({
            let mut vec = Vec::with_capacity(32);
            let mut total_len = 0;
            for iovec in iovecs.iter() {
                let mut iov_len = iovec.iov_len;
                if total_len + iov_len > MAX_SYSCALL_BUFFER_SIZE {
                    iov_len = MAX_SYSCALL_BUFFER_SIZE - total_len;
                }
                if iov_len == 0 {
                    continue;
                }
                match translated_byte_buffer_append_to_existing_vec(
                    &mut vec,
                    token,
                    iovec.iov_base,
                    iov_len,
                    UserAccess::Read,
                ) {
                    Ok(_) => {
                        total_len += iov_len;
                        continue;
                    }
                    Err(errno) => return errno,
                }
            }
            vec
        }),
    ) {
        Ok(n) => n as isize,
        Err(e) => -(e as isize),
    }
}

fn validate_iovec_total_len(iovecs: &[IOVec]) -> Result<(), ()> {
    let mut total_len = 0usize;
    for iovec in iovecs {
        // 先按 Linux 语义查长度溢出
        total_len = total_len
            .checked_add(iovec.iov_len)
            .filter(|len| *len <= isize::MAX as usize)
            .ok_or(())?;
    }
    Ok(())
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
    let fd_table = task.files.lock();
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
    // turn a pointer in user space into a pointer in kernel space if it is not null
    let offset = if offset.is_null() {
        offset
    } else {
        match translated_refmut(token, offset) {
            Ok(offset) => offset as *mut usize,
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
                let read_size = unsafe {
                    if let Some(ref mut off) = offset.as_mut() {
                        let off_val = **off;
                        let n = match in_file.inode.read_at(
                            off_val,
                            buffer.len(),
                            buffer.as_mut_slice(),
                            in_file.private_data(),
                        ) {
                            Ok(n) => n,
                            Err(e) => return -(e as isize),
                        };
                        **off += n;
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

        let fallback = |redundant_bytes: usize| unsafe {
            let offset = offset.as_mut();
            match offset {
                Some(offset) => {
                    *offset -= redundant_bytes;
                }
                None => match in_file.lseek(SeekFrom::SeekCurrent(-(redundant_bytes as i64))) {
                    Ok(_) => {}
                    Err(errno) => panic!("failed! errno {:?}", errno),
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
    info!("[sys_sendfile] send bytes: {}", send_size);
    send_size as isize
}

pub fn sys_close(fd: usize) -> isize {
    info!("[sys_close] fd: {}", fd);
    let task = current_task().unwrap();
    let mut fd_table = task.files.lock();
    match fd_table.drop_fd(fd) {
        Ok(_) => SUCCESS,
        Err(e) => return -(e as isize),
    }
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
    let mut fd_table = task.files.lock();
    let (pipe_read, pipe_write) = make_pipe();
    let cloexec = flags.contains(OpenFlags::O_CLOEXEC);
    let vf_read = vfs::File::new_without_open(
        pipe_read, vfs::FileFlags::O_RDONLY, vfs::FileType::Pipe,
    );
    let read_fd = match fd_table.alloc_fd(vf_read, cloexec) {
        Ok(fd) => fd,
        Err(e) => return -(e as isize),
    };
    let vf_write = vfs::File::new_without_open(
        pipe_write, vfs::FileFlags::O_WRONLY, vfs::FileType::Pipe,
    );
    let write_fd = match fd_table.alloc_fd(vf_write, cloexec) {
        Ok(fd) => fd,
        Err(e) => return -(e as isize),
    };

    let token = task.get_user_token();
    if copy_to_user_array(
        token,
        [read_fd as u32, write_fd as u32].as_ptr(),
        pipefd as *mut u32,
        2,
    )
    .is_err()
    {
        log::error!("[sys_pipe2] Failed to copy to {:?}", pipefd);
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
        AT_FDCWD => match task.fs.lock().working_inode.try_clone() { Some(f) => f, None => return EBADF, },
        fd => {
            let fd_table = task.files.lock();
            match fd_table.get_file(fd) {
                Ok(file) => match file.try_clone() { Some(f) => f, None => return EBADF, },
                Err(e) => return -(e as isize),
            }
        }
    };
    // 获取目录项向量 — count 是字节数，需要转换为最大条目数
    let max_entries = count / size_of::<Dirent>();
    let dirent_vec = match file.get_dirent(max_entries) {
        Ok(vec) => vec,
        Err(errno) => return errno,
    };
    // 将结果复制到用户态的数组中
    if copy_to_user_array(
        token,
        dirent_vec.as_ptr(),
        dirp as *mut Dirent,
        dirent_vec.len(),
    )
    .is_err()
    {
        log::error!("[sys_getdents64] Failed to copy to {:?}", dirp);
        return EFAULT;
    };
    info!("[sys_getdents64] fd: {}, count: {}", fd, count);
    dirent_vec.len() as isize * size_of::<Dirent>() as isize
}

pub fn sys_dup(oldfd: usize) -> isize {
    let task = current_task().unwrap();
    let mut fd_table = task.files.lock();
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
    if oldfd == newfd {
        return oldfd as isize;
    }
    let task = current_task().unwrap();

    let ret = {
        let mut fd_table = task.files.lock();
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
        "[sys_dup3] oldfd: {}, newfd: {}, flags: {:?}",
        oldfd,
        newfd,
        OpenFlags::from_bits(flags)
    );
    if oldfd == newfd {
        return EINVAL;
    }
    let is_cloexec = match OpenFlags::from_bits(flags) {
        Some(OpenFlags::O_CLOEXEC) => true,
        // `O_RDONLY == 0`, so it means *NO* cloexec in fact
        Some(OpenFlags::O_RDONLY) => false,
        // flags contain an invalid value
        Some(flags) => {
            warn!("[sys_dup3] invalid flags: {:?}", flags);
            return EINVAL;
        }
        None => {
            warn!("[sys_dup3] unknown flags");
            return EINVAL;
        }
    };
    let task = current_task().unwrap();
    let mut fd_table = task.files.lock();

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
    let path = match translated_str(token, pathname) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    let real_path = if path.as_str() == "/proc/self/exe" {
        let exe_path = task.exe_path.lock().clone();
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
    for i in 0..len {
        let ptr = buf as usize + i;
        if crate::mm::copy_to_user(token, &bytes[i], ptr as *mut u8).is_err() {
            log::error!("[sys_readlinkat] Failed to copy to {:?}", buf);
            return EFAULT;
        }
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
    let path = match translated_str(token, path) {
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
        let ft = inode.metadata().map(|m| m.file_type).unwrap_or(vfs::FileType::File);
        let vf = vfs::File::new_without_open(inode, vfs::FileFlags::O_RDONLY, ft);
        let stat = vf.get_stat_old();
        if copy_to_user(token, &stat, buf as *mut Stat).is_err() {
            return EFAULT;
        }
        SUCCESS
    } else {
        let dir_file = vfs::File::new_without_open(start, vfs::FileFlags::O_RDONLY, vfs::FileType::Dir);
        match dir_file.open_path(&path, OpenFlags::O_RDONLY) {
            Ok(new_file) => {
                if copy_to_user(token, &new_file.get_stat_old(), buf as *mut Stat).is_err() {
                    log::error!("[sys_fstatat] Failed to copy to {:?}", buf);
                    return EFAULT;
                };
                SUCCESS
            }
            Err(errno) => errno,
        }
    }
}

/// warning: 此函数没有完全实现，没有实现根据mask来填充statx的值，并且没有直接维护statx结构体，通过stat结构体间接实现
pub fn sys_statx(dirfd: usize, path: *const u8, flags: u32, mask: u32, buf: *mut u8) -> isize {
    let token = current_user_token();
    let path = match translated_str(token, path) {
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

    fn stat_to_statx(stat: &Stat, mask: u32) -> Statx {
        Statx::new(
            mask,
            stat.get_nlink(),
            stat.get_mode() as u16,
            stat.get_ino() as u64,
            stat.get_size() as u64,
            stat.get_atime() as i64,
            stat.get_ctime() as i64,
            stat.get_mtime() as i64,
            (stat.get_rdev() & 0xffff_00) >> 8 as u32,
            (stat.get_rdev() & 0xff) as u32,
            (stat.get_dev() & 0xffff_00) >> 8 as u32,
            (stat.get_dev() & 0xff) as u32,
        )
    }

    let no_follow = flags.contains(FstatatFlags::AT_SYMLINK_NOFOLLOW);
    if no_follow {
        // AT_SYMLINK_NOFOLLOW: 使用新 VFS 路径解析
        let inode = match vfs_lookup(&start, &path, false) {
            Ok(inode) => inode,
            Err(errno) => return errno,
        };
        let ft = inode.metadata().map(|m| m.file_type).unwrap_or(vfs::FileType::File);
        let vf = vfs::File::new_without_open(inode, vfs::FileFlags::O_RDONLY, ft);
        let statx = stat_to_statx(&vf.get_stat_old(), mask);
        if copy_to_user(token, &statx, buf as *mut Statx).is_err() {
            return EFAULT;
        }
        SUCCESS
    } else {
        let dir_file = vfs::File::new_without_open(start, vfs::FileFlags::O_RDONLY, vfs::FileType::Dir);
        match dir_file.open_path(&path, OpenFlags::O_RDONLY) {
            Ok(new_file) => {
                let statx = stat_to_statx(&new_file.get_stat_old(), mask);
                if copy_to_user(token, &statx, buf as *mut Statx).is_err()
                {
                    log::error!("[sys_statx] Failed to copy to {:?}", buf);
                    return EFAULT;
                };
                log::debug!("[sys_statx] statx:\n{:?}", statx);
                SUCCESS
            }
            Err(errno) => errno,
        }
    }
}

pub fn sys_fstat(fd: usize, statbuf: *mut u8) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();

    info!("[sys_fstat] fd: {}", fd);
    let file = match fd {
        AT_FDCWD => match task.fs.lock().working_inode.try_clone() { Some(f) => f, None => return EBADF, },
        fd => {
            let fd_table = task.files.lock();
            match fd_table.get_file(fd) {
                Ok(file) => match file.try_clone() { Some(f) => f, None => return EBADF, },
                Err(e) => return -(e as isize),
            }
        }
    };
    if copy_to_user(token, &file.get_stat_old(), statbuf as *mut Stat).is_err() {
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
    if copy_to_user(token, statfs.as_ref(), buf).is_err() {
        log::error!("[sys_statfs] Failed to copy to {:?}", buf);
        return EFAULT;
    };
    SUCCESS
}

pub fn sys_fsync(fd: usize) -> isize {
    let task = current_task().unwrap();

    info!("[sys_fsync] fd: {}", fd);
    let fd_table = task.files.lock();
    match fd_table.get_file(fd) {
        Ok(_) => SUCCESS,
        Err(e) => return -(e as isize),
    }
}

pub fn sys_fchmodat() -> isize {
    // baseline 未完成这个函数
    println!("[kernel in sys_fchmodat] chmod is not supported for now!\n");
    0
}

pub fn sys_fchownat() -> isize {
    // 内核暂无权限系统，假装返回成功
    0
}

pub fn sys_chdir(path: *const u8) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path = match translated_str(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    info!("[sys_chdir] path: {}", path);

    let mut lock = task.fs.lock();

    match lock.working_inode.cd(&path) {
        Ok(new_working_inode) => {
            lock.working_inode = new_working_inode;
            SUCCESS
        }
        Err(errno) => errno,
    }
}

pub fn sys_openat(dirfd: usize, path: *const u8, flags: u32, mode: u32) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let path = match translated_str(token, path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    let flags = match OpenFlags::from_bits(flags) {
        Some(flags) => flags,
        None => {
            warn!("[sys_openat] unknown flags");
            return EINVAL;
        }
    };
    let mode = StatMode::from_bits(mode);
    info!(
        "[sys_openat] dirfd: {}, path: {}, flags: {:?}, mode: {:?}",
        dirfd as isize, path, flags, mode
    );
    let start = match resolve_start_inode(dirfd) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };

    let dir_file = vfs::File::new_without_open(start, vfs::FileFlags::O_RDONLY, vfs::FileType::Dir);

    let new_file = match dir_file.open_path(&path, flags) {
        Ok(file) => file,
        Err(errno) => return errno,
    };

    let mut fd_table = task.files.lock();
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
    let oldpath_str = match translated_str(token, oldpath) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    let newpath_str = match translated_str(token, newpath) {
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

pub fn sys_ioctl(fd: usize, cmd: u32, arg: usize) -> isize {
    let task = current_task().unwrap();
    let fd_table = task.files.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    file.ioctl_old(cmd, arg)
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
    let path = match translated_str(token, path) {
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
    let dir_file = vfs::File::new_without_open(start, vfs::FileFlags::O_RDONLY, vfs::FileType::Dir);
    match dir_file.mkdir_path(&path) {
        Ok(_) => SUCCESS,
        Err(errno) => errno,
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
    let path = match translated_str(token, path) {
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

    let start = match resolve_start_inode(dirfd) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };
    let dir_file = vfs::File::new_without_open(start, vfs::FileFlags::O_RDONLY, vfs::FileType::Dir);
    match dir_file.delete_path(&path, flags.contains(UnlinkatFlags::AT_REMOVEDIR)) {
        Ok(_) => SUCCESS,
        Err(errno) => errno,
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
    let target = match translated_str(token, target) {
        Ok(target) => target,
        Err(errno) => return errno,
    };
    let flags = match UmountFlags::from_bits(flags) {
        Some(flags) => flags,
        None => return EINVAL,
    };
    info!("[sys_umount2] target: {}, flags: {:?}", target, flags);
    warn!("[sys_umount2] fake implementation!");
    SUCCESS
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

pub fn sys_mount(
    source: *const u8,
    target: *const u8,
    filesystemtype: *const u8,
    mountflags: usize,
    data: *const u8,
) -> isize {
    if source.is_null() || target.is_null() || filesystemtype.is_null() {
        return EINVAL;
    }
    let token = current_user_token();
    let source = match translated_str(token, source) {
        Ok(source) => source,
        Err(errno) => return errno,
    };
    let target = match translated_str(token, target) {
        Ok(target) => target,
        Err(errno) => return errno,
    };
    let filesystemtype = match translated_str(token, filesystemtype) {
        Ok(filesystemtype) => filesystemtype,
        Err(errno) => return errno,
    };
    // infallible
    let mountflags = MountFlags::from_bits(mountflags).unwrap();
    info!(
        "[sys_mount] source: {}, target: {}, filesystemtype: {}, mountflags: {:?}, data: {:?}",
        source, target, filesystemtype, mountflags, data
    );
    warn!("[sys_mount] fake implementation!");
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
        match translated_str(token, pathname) {
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

    let now = TimeSpec::now();
    let timespec = &mut [now; 2];
    let mut atime = Some(now.tv_sec);
    let mut mtime = Some(now.tv_sec);
    if !times.is_null() {
        if copy_from_user(token, times, timespec).is_err() {
            log::error!("[sys_utimensat] Failed to copy from {:?}", times);
            return EFAULT;
        };
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

    _file.set_timestamp_old(None, atime, mtime).unwrap();
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
    let mut fd_table = task.files.lock();

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

            // Find the lowest-numbered available fd greater than or equal to arg
            let mut new_fd = arg;
            while new_fd < fd_table.len() {
                if fd_table.get_file(new_fd).is_err() {
                    break;
                }
                new_fd += 1;
            }

            match fd_table.alloc_fd_at(new_fd, file, cloexec) {
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
            // Access control is not fully implemented
            let mut res = OpenFlags::O_RDWR.bits() as isize;
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
        match translated_ref(token, sigmask_args as *const usize) {
            Ok(ss_ptr) => {
                let ptr = *ss_ptr;
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
    let mut kread_fds = match try_get_from_user(token, read_fds) {
        Ok(fds) => fds,
        Err(errno) => return errno,
    };
    let mut kwrite_fds = match try_get_from_user(token, write_fds) {
        Ok(fds) => fds,
        Err(errno) => return errno,
    };
    let mut kexception_fds = match try_get_from_user(token, exception_fds) {
        Ok(fds) => fds,
        Err(errno) => return errno,
    };
    let ktimeout = match try_get_from_user(token, timeout) {
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
    /*
    WARNING! The EFAULT errno is NOT mentioned in man for Linux.
    However, it is mentioned in BSD man, so we keep it anyway.
     */
    if let Some(kread_fds) = &kread_fds {
        trace!("[pselect] read_fds: {:?}", kread_fds);
        if copy_to_user(token, kread_fds, read_fds).is_err() {
            log::error!("[sys_pselect] Error copying to read_fds {:?}", read_fds);
            ret = EFAULT;
        };
    }
    if let Some(kwrite_fds) = &kwrite_fds {
        trace!("[pselect] write_fds: {:?}", kwrite_fds);
        if copy_to_user(token, kwrite_fds, write_fds).is_err() {
            log::error!("[sys_pselect] Error copying to write_fds {:?}", write_fds);
            ret = EFAULT;
        };
    }
    if let Some(kexception_fds) = &kexception_fds {
        trace!("[pselect] exception_fds: {:?}", kexception_fds);
        if copy_to_user(token, kexception_fds, exception_fds).is_err() {
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

pub fn sys_faccessat2(dirfd: usize, pathname: *const u8, mode: u32, flags: u32) -> isize {
    let token = current_user_token();
    let pathname = match translated_str(token, pathname) {
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

    // Do not check user's authority, because user group is not implemented yet.
    // All existing files can be accessed.
    match __openat(dirfd, pathname.as_str()) {
        Ok(_) => SUCCESS,
        Err(errno) => errno,
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
    let task = current_task().unwrap();
    if !task
        .vm
        .lock()
        .contains_valid_buffer(addr, length, MapPermission::empty())
    {
        return ENOMEM;
    }
    info!(
        "[sys_msync] addr: {:X}, length: {:X}, flags: {:?}",
        addr, flags, flags
    );
    SUCCESS
}

pub fn sys_ftruncate(fd: usize, length: isize) -> isize {
    let task = current_task().unwrap();
    let fd_table = task.files.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    match file.truncate_size(length as usize) {
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
    let fd_table = task.files.lock();
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

    let target_str = match translated_str(token, target) {
        Ok(s) => s,
        Err(e) => return e,
    };

    let linkpath_str = match translated_str(token, linkpath) {
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

    let oldpath_str = match translated_str(token, oldpath) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let newpath_str = match translated_str(token, newpath) {
        Ok(s) => s,
        Err(e) => return e,
    };

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
