use super::common::*;

const FIFREEZE: u32 = 0xc004_5877;
const FITHAW: u32 = 0xc004_5878;
const FIFREEZE_LEGACY: u32 = 0x5878;
const FITHAW_LEGACY: u32 = 0x5879;
const FS_IOC_GETFLAGS: u32 = 0x8008_6601;
const FS_IOC_SETFLAGS: u32 = 0x4008_6602;
const FS_APPEND_FL: u32 = 0x0000_0020;
const FS_IMMUTABLE_FL: u32 = 0x0000_0010;

pub fn sys_ioctl(fd: usize, cmd: u32, arg: usize) -> isize {
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = match fd_table.get_file(fd) {
        Ok(file) => file,
        Err(e) => return -(e as isize),
    };
    if is_path_fd(&file) {
        return EBADF;
    }

    if matches!(cmd, FIFREEZE | FITHAW | FIFREEZE_LEGACY | FITHAW_LEGACY)
        && file.file_type() == crate::fs::vfs::FileType::File
    {
        return 0;
    }

    if file.file_type() == crate::fs::vfs::FileType::File {
        match cmd {
            FS_IOC_GETFLAGS => {
                let metadata = match file.metadata() {
                    Ok(metadata) => metadata,
                    Err(error) => return -(error as isize),
                };
                let mut flags = 0u32;
                if metadata
                    .flags
                    .contains(crate::fs::vfs::InodeFlags::S_APPEND)
                {
                    flags |= FS_APPEND_FL;
                }
                if metadata
                    .flags
                    .contains(crate::fs::vfs::InodeFlags::S_IMMUTABLE)
                {
                    flags |= FS_IMMUTABLE_FL;
                }
                return match crate::mm::translated_refmut(token, arg as *mut u32) {
                    Ok(output) => {
                        *output = flags;
                        0
                    }
                    Err(_) => EFAULT,
                };
            }
            FS_IOC_SETFLAGS => {
                let flags = match crate::mm::translated_ref(token, arg as *const u32) {
                    Ok(input) => *input,
                    Err(_) => return EFAULT,
                };
                let mut metadata = match file.metadata() {
                    Ok(metadata) => metadata,
                    Err(error) => return -(error as isize),
                };
                metadata.flags.set(
                    crate::fs::vfs::InodeFlags::S_APPEND,
                    flags & FS_APPEND_FL != 0,
                );
                metadata.flags.set(
                    crate::fs::vfs::InodeFlags::S_IMMUTABLE,
                    flags & FS_IMMUTABLE_FL != 0,
                );
                return match file.inode.set_metadata(&metadata) {
                    Ok(()) => 0,
                    Err(error) => -(error as isize),
                };
            }
            _ => {}
        }
    }

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

    if cmd == FIONBIO {
        let arg_ptr = arg as *mut i32;
        if arg_ptr.is_null() {
            return EFAULT;
        }
        let value = match crate::mm::translated_ref(token, arg_ptr) {
            Ok(r) => *r,
            Err(_) => return EFAULT,
        };
        file.set_nonblock(value != 0);
        return 0;
    }

    match file.inode.ioctl(cmd, arg, file.private_data()) {
        Ok(n) => n as isize,
        Err(SyscallErr::ENOSYS) => ENOTTY,
        Err(e) => -(e as isize),
    }
}
