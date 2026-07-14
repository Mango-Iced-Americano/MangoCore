use super::common::*;

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
