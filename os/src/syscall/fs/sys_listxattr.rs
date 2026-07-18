use super::common::*;

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
