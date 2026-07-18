use super::common::*;

pub fn sys_fgetxattr(fd: usize, name: *const u8, value: *mut u8, size: usize) -> isize {
    // fd_to_inode first: O_PATH fd returns EBADF before xattr name validation
    // (Linux ordering: EBADF takes priority over EOPNOTSUPP/ENOTSUP)
    let inode = match fd_to_inode(fd) {
        Ok(i) => i,
        Err(e) => return e,
    };

    let token = current_user_token();
    let name_str = match user_cstring(token, name) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let validated = match validate_xattr_name(&name_str) {
        Ok(n) => n,
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
