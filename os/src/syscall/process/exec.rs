use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::fs::{vfs, vfs_lookup};
use crate::mm::{UserCString, UserPtr};
use crate::show_frame_consumption;
use crate::syscall::errno::*;
use crate::task::{current_task, exit_current_and_run_next};
use log::{debug, info};

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
        return Err(EACCES);
    }
    let exec_bits = vfs::InodeMode::S_IXUSR | vfs::InodeMode::S_IXGRP | vfs::InodeMode::S_IXOTH;
    if !metadata.mode.intersects(exec_bits) {
        return Err(EACCES);
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
            argv_vec.push(match UserCString::new(arg_ptr).read(token) {
                Ok(arg) => arg,
                Err(errno) => return errno,
            });
            unsafe {
                argv = argv.add(1);
            }
        }
    }
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
            envp_vec.push(match UserCString::new(env_ptr).read(token) {
                Ok(env) => env,
                Err(errno) => return errno,
            });
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

    let open_exec = |path: &str| -> Result<vfs::File, isize> {
        validate_exec_path_len(path)?;
        let inode = vfs_lookup(&cwd_inode, path, true)?;
        let file = vfs::File::new(inode, vfs::FileFlags::O_RDONLY).map_err(|e| -(e as isize))?;
        check_exec_metadata(&file)?;
        Ok(file)
    };

    match open_exec(&path) {
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
                let shell_file = match parse_shebang(&file) {
                    Ok(Some((interp, shebang_arg))) => {
                        match open_exec(&interp) {
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
                                match open_exec("/bin/bash") {
                                    Ok(f) => {
                                        if argv_vec.try_reserve(1).is_err() { return ENOMEM; }
                                        argv_vec.insert(0, path.clone());
                                        f
                                    }
                                    Err(e) => return e,
                                }
                            }
                        }
                    }
                    _ => {
                        match open_exec("/bin/bash") {
                            Ok(f) => {
                                if argv_vec.try_reserve(1).is_err() { return ENOMEM; }
                                argv_vec.insert(0, path.clone());
                                f
                            }
                            Err(e) => return e,
                        }
                    }
                };
                shell_file
            } else {
                return ENOEXEC;
            };

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
