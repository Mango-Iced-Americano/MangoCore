use super::common::*;

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
