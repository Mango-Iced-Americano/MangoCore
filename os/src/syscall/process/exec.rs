use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::config::{PAGE_SIZE, USER_STACK_SIZE};
use crate::fs::{vfs, vfs_lookup};
use crate::mm::{UserCString, UserPtr};
use crate::show_frame_consumption;
use crate::syscall::errno::*;
use crate::task::{current_task, exit_current_and_run_next, is_writable_inode_busy, AuxvEntry};
use log::{debug, info};

const MAX_EXEC_ARG_ENV_BYTES: usize = USER_STACK_SIZE / 2;
const EXEC_AUXV_ENTRY_COUNT: usize = 17;

/// 验证文件是否为有效 ELF（前4字节为 \x7fELF 魔数）
fn is_valid_elf(file: &vfs::File) -> bool {
    if file.get_size() < 4 {
        return false;
    }
    let mut magic = [0u8; 4];
    match file.pread(0, &mut magic) {
        Ok(n) if n >= 4 => &magic == b"\x7fELF",
        _ => false,
    }
}

/// shebang 解释器无效时尝试常见 shell：/bin/sh → /bin/bash
/// 成功时更新 argv_vec（插入脚本路径为 argv[0]），返回打开的 shell File
fn try_open_shell_fallback(
    cwd_inode: &Arc<dyn vfs::IndexNode>,
    argv_vec: &mut Vec<String>,
    script_path: &str,
) -> Result<vfs::File, isize> {
    for shell in &["/bin/sh", "/bin/bash"] {
        let file = match open_exec(cwd_inode, shell) {
            Ok(f) if is_valid_elf(&f) => f,
            _ => continue,
        };
        if argv_vec.try_reserve(1).is_err() {
            return Err(ENOMEM);
        }
        argv_vec.insert(0, script_path.to_string());
        return Ok(file);
    }
    Err(ENOEXEC)
}

fn parse_shebang(file: &vfs::File) -> Result<Option<(String, Option<String>)>, isize> {
    let mut header = [0u8; 128];
    let n = file.pread(0, &mut header).map_err(|e| -(e as isize))?;
    if n < 2 || header[0] != b'#' || header[1] != b'!' {
        return Ok(None);
    }

    let line_end = header[..n].iter().position(|&c| c == b'\n').unwrap_or(n);
    let line = core::str::from_utf8(&header[2..line_end]).map_err(|_| ENOEXEC)?;
    let line = line.trim();
    if line.is_empty() {
        return Err(ENOEXEC);
    }

    let mut parts = line.splitn(2, |c: char| c == ' ' || c == '\t');
    let interpreter = parts.next().unwrap().to_string();
    let arg = parts
        .next()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    Ok(Some((interpreter, arg)))
}

fn validate_exec_path_len(path: &str) -> Result<(), isize> {
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

fn check_exec_metadata(file: &vfs::File) -> Result<(), isize> {
    let metadata = file.metadata().map_err(|e| -(e as isize))?;
    if metadata.file_type != vfs::FileType::File {
        if metadata.file_type == vfs::FileType::Dir {
            return Err(EISDIR);
        }
        return Err(EACCES);
    }
    if !has_exec_access(&metadata) {
        return Err(EACCES);
    }
    if is_writable_inode_busy(&file.inode) {
        return Err(ETXTBSY);
    }
    Ok(())
}

fn has_exec_access(metadata: &vfs::Metadata) -> bool {
    let mode = metadata.mode.bits() & 0o777;
    let exec_any = (mode & 0o111) != 0;
    let task = current_task().unwrap();
    let inner = task.acquire_inner_lock();

    if inner.fsuid == 0 {
        return exec_any;
    }
    let allowed = if inner.fsuid == metadata.uid {
        (mode >> 6) & 0o7
    } else if inner.fsgid == metadata.gid || inner.groups.iter().any(|&gid| gid == metadata.gid) {
        (mode >> 3) & 0o7
    } else {
        mode & 0o7
    };
    (allowed & 0o1) != 0
}

fn open_exec_file(cwd_inode: &Arc<dyn vfs::IndexNode>, path: &str) -> Result<vfs::File, isize> {
    validate_exec_path_len(path)?;
    let inode = vfs_lookup(cwd_inode, path, true)?;
    let file = vfs::File::new(inode, vfs::FileFlags::O_RDONLY).map_err(|e| -(e as isize))?;
    check_exec_metadata(&file)?;
    Ok(file)
}

fn is_compat_shell_path(path: &str) -> bool {
    path == "/bin/sh" || path == "/bin/bash"
}

fn open_exec(cwd_inode: &Arc<dyn vfs::IndexNode>, path: &str) -> Result<vfs::File, isize> {
    match open_exec_file(cwd_inode, path) {
        Ok(file) => Ok(file),
        Err(errno) if is_compat_shell_path(path) => {
            open_exec_file(cwd_inode, "/bash").or(Err(errno))
        }
        Err(errno) => Err(errno),
    }
}

fn checked_add_exec_bytes(total: &mut usize, add: usize) -> Result<(), isize> {
    *total = total.checked_add(add).ok_or(E2BIG)?;
    Ok(())
}

fn account_exec_string_bytes(total: &mut usize, value: &str) -> Result<(), isize> {
    let size = value.len().checked_add(1).ok_or(E2BIG)?;
    checked_add_exec_bytes(total, size)?;
    if *total > MAX_EXEC_ARG_ENV_BYTES {
        return Err(E2BIG);
    }
    Ok(())
}

fn validate_exec_stack_usage(argv_vec: &[String], envp_vec: &[String]) -> Result<(), isize> {
    let word = core::mem::size_of::<usize>();
    let mut bytes = 2usize.checked_mul(word).ok_or(E2BIG)?;
    for value in argv_vec.iter().chain(envp_vec.iter()) {
        checked_add_exec_bytes(&mut bytes, value.len().checked_add(1).ok_or(E2BIG)?)?;
    }
    bytes = (bytes + word - 1) & !(word - 1);
    checked_add_exec_bytes(&mut bytes, 2 * word)?; // AT_RANDOM bytes
    checked_add_exec_bytes(&mut bytes, word)?; // padding
    checked_add_exec_bytes(
        &mut bytes,
        EXEC_AUXV_ENTRY_COUNT
            .checked_mul(core::mem::size_of::<AuxvEntry>())
            .ok_or(E2BIG)?,
    )?;
    checked_add_exec_bytes(
        &mut bytes,
        argv_vec
            .len()
            .checked_add(1)
            .and_then(|n| n.checked_mul(word))
            .ok_or(E2BIG)?,
    )?;
    checked_add_exec_bytes(
        &mut bytes,
        envp_vec
            .len()
            .checked_add(1)
            .and_then(|n| n.checked_mul(word))
            .ok_or(E2BIG)?,
    )?;
    checked_add_exec_bytes(&mut bytes, word)?; // argc
    if bytes > USER_STACK_SIZE.saturating_sub(PAGE_SIZE) {
        return Err(E2BIG);
    }
    Ok(())
}

pub fn sys_execve(
    pathname: *const u8,
    mut argv: *const *const u8,
    mut envp: *const *const u8,
) -> isize {
    // 获取当前进程
    let task = current_task().unwrap();
    // 获取当前进程的用户态内存访问权限
    let token = task.get_user_token();
    // 获取可执行文件的路径
    let path = match UserCString::new(pathname).read(token) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    if let Err(errno) = validate_exec_path_len(&path) {
        return errno;
    }
    // 解析参数列表
    let mut argv_vec: Vec<String> = Vec::new();
    if argv_vec.try_reserve(16).is_err() {
        return ENOMEM;
    }
    // 解析环境变量列表
    let mut envp_vec: Vec<String> = Vec::new();
    if envp_vec.try_reserve(16).is_err() {
        return ENOMEM;
    }
    if !argv.is_null() {
        let mut arg_env_bytes = 0usize;
        loop {
            let arg_ptr = match UserPtr::new(argv).read(token) {
                Ok(argv) => argv,
                Err(errno) => return errno,
            };
            if arg_ptr.is_null() {
                break;
            }
            if argv_vec.try_reserve(1).is_err() {
                return ENOMEM;
            }
            let arg = match UserCString::new(arg_ptr).read(token) {
                Ok(arg) => arg,
                Err(errno) => return errno,
            };
            if let Err(errno) = account_exec_string_bytes(&mut arg_env_bytes, &arg) {
                return errno;
            }
            argv_vec.push(arg);
            unsafe {
                argv = argv.add(1);
            }
        }
    }
    if argv_vec.is_empty() {
        if argv_vec.try_reserve(1).is_err() {
            return ENOMEM;
        }
        argv_vec.push(String::new());
    }
    let mut arg_env_bytes = argv_vec
        .iter()
        .map(|arg| arg.len().saturating_add(1))
        .sum::<usize>();
    if !envp.is_null() {
        loop {
            let env_ptr = match UserPtr::new(envp).read(token) {
                Ok(envp) => envp,
                Err(errno) => return errno,
            };
            if env_ptr.is_null() {
                break;
            }
            if envp_vec.try_reserve(1).is_err() {
                return ENOMEM;
            }
            let env = match UserCString::new(env_ptr).read(token) {
                Ok(env) => env,
                Err(errno) => return errno,
            };
            if let Err(errno) = account_exec_string_bytes(&mut arg_env_bytes, &env) {
                return errno;
            }
            envp_vec.push(env);
            unsafe {
                envp = envp.add(1);
            }
        }
    }
    debug!(
        "[exec] argv: {:?} /* {} vars */, envp: {:?} /* {} vars */",
        argv_vec,
        argv_vec.len(),
        envp_vec,
        envp_vec.len()
    );
    // 获取当前工作目录的文件描述符
    let (working_inode, working_path) = {
        let fs_ref = task.process.fs();
        let lock = fs_ref.lock();
        (lock.working_inode.clone(), lock.working_path.clone())
    };
    let cwd_inode: Arc<dyn vfs::IndexNode> = working_inode.inode.clone();

    match open_exec(&cwd_inode, &path) {
        // 检查打开的文件
        Ok(file) => {
            // 若文件大小小于4，则返回ENOEXEC
            // 即非可执行文件
            if file.get_size() < 4 {
                return ENOEXEC;
            }
            // 看前四个字节是否是可执行文件魔数
            let mut magic_number = [0u8; 4];
            // this operation may be expensive... I'm not sure
            let _ = file.pread(0, &mut magic_number);
            let elf = if &magic_number == b"\x7fELF" {
                file
            } else if &magic_number[..2] == b"#!" {
                let shell_file = if let Ok(Some((interp, shebang_arg))) = parse_shebang(&file) {
                    // 尝试打开并验证 shebang 解释器
                    match open_exec(&cwd_inode, &interp).and_then(|f| {
                        if !f.is_dir() && is_valid_elf(&f) {
                            Ok(f)
                        } else {
                            Err(ENOEXEC)
                        }
                    }) {
                        Ok(f) => {
                            let mut script_argv = Vec::new();
                            script_argv.push(interp);
                            if let Some(arg) = shebang_arg { script_argv.push(arg); }
                            script_argv.push(path.clone());
                            for arg in argv_vec.iter().skip(1) {
                                script_argv.push(arg.clone());
                            }
                            argv_vec = script_argv;
                            f
                        }
                        Err(_) => {
                            // 解释器无效，回退到常见 shell
                            match try_open_shell_fallback(&cwd_inode, &mut argv_vec, &path) {
                                Ok(f) => f,
                                Err(e) => return e,
                            }
                        }
                    }
                } else {
                    // shebang 解析失败，尝试常见 shell 回退
                    match try_open_shell_fallback(&cwd_inode, &mut argv_vec, &path) {
                        Ok(f) => f,
                        Err(e) => return e,
                    }
                };
                shell_file
            } else {
                return ENOEXEC;
            };
            if let Err(errno) = validate_exec_stack_usage(&argv_vec, &envp_vec) {
                return errno;
            }

            // 检查不是目录 — Linux execve(2) 对目录返回 EISDIR
            if elf.is_dir() {
                return EISDIR;
            }

            let task = current_task().unwrap();
            // 确保 exe_path 是绝对路径（glibc _dl_get_origin 要求以 '/' 开头）
            let abs_path = if path.starts_with('/') {
                path.clone()
            } else {
                let cwd = working_path.clone();
                if cwd == "/" {
                    alloc::format!("/{}", path)
                } else {
                    alloc::format!("{}/{}", cwd, path)
                }
            };
            show_frame_consumption! {
                "load_elf";
                if let Err(errno) = task.load_elf(elf, &argv_vec, &envp_vec) {
                    exit_current_and_run_next(127);
                };
            }
            task.process.mark_execed();
            task.process.set_exe_path(abs_path);
            task.process.complete_vfork();
            // should return 0 in success
            SUCCESS
        }
        Err(errno) => {
            info!(
                "[sys_execve] open_path(\"{}\") failed: errno={}",
                path, errno
            );
            errno
        }
    }
}
