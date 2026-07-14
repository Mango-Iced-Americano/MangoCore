use super::common::*;

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
