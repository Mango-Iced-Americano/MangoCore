use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::config::{PAGE_SIZE, USER_STACK_INIT_SIZE};
use crate::fs::{vfs, vfs_lookup};
use crate::mm::{UserCString, UserPtr, USER_STACK_ABI_ALIGN};
use crate::show_frame_consumption;
use crate::syscall::errno::*;
use crate::task::{
    current_task, current_user_token, exit_current_and_run_next, is_writable_inode_busy,
    AuxvEntry,
};

const MAX_EXEC_ARG_ENV_BYTES: usize = USER_STACK_INIT_SIZE / 2;
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
) -> Result<Arc<vfs::File>, isize> {
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

fn open_exec_file(
    cwd_inode: &Arc<dyn vfs::IndexNode>,
    path: &str,
    follow_final: bool,
) -> Result<Arc<vfs::File>, isize> {
    validate_exec_path_len(path)?;
    let inode = vfs_lookup(cwd_inode, path, follow_final)?;
    if !follow_final
        && inode.metadata().map_err(|e| -(e as isize))?.file_type == vfs::FileType::SymLink
    {
        return Err(ELOOP);
    }
    let file = vfs::File::new(inode, vfs::FileFlags::O_RDONLY).map_err(|e| -(e as isize))?;
    check_exec_metadata(&file)?;
    Ok(file)
}

fn is_compat_shell_path(path: &str) -> bool {
    path == "/bin/sh" || path == "/bin/bash"
}

fn open_exec(cwd_inode: &Arc<dyn vfs::IndexNode>, path: &str) -> Result<Arc<vfs::File>, isize> {
    open_exec_with_follow(cwd_inode, path, true)
}

fn open_exec_with_follow(
    cwd_inode: &Arc<dyn vfs::IndexNode>,
    path: &str,
    follow_final: bool,
) -> Result<Arc<vfs::File>, isize> {
    match open_exec_file(cwd_inode, path, follow_final) {
        Ok(file) => Ok(file),
        Err(errno) if follow_final && is_compat_shell_path(path) => {
            open_exec_file(cwd_inode, "/bash", true).or(Err(errno))
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
    debug_assert!(USER_STACK_ABI_ALIGN.is_power_of_two());
    bytes = bytes.checked_add(USER_STACK_ABI_ALIGN - 1).ok_or(E2BIG)? & !(USER_STACK_ABI_ALIGN - 1);
    checked_add_exec_bytes(&mut bytes, 2 * word)?; // AT_RANDOM bytes
    let auxv_bytes = EXEC_AUXV_ENTRY_COUNT
        .checked_mul(core::mem::size_of::<AuxvEntry>())
        .ok_or(E2BIG)?;
    let pointer_words = argv_vec
        .len()
        .checked_add(1)
        .and_then(|words| words.checked_add(envp_vec.len().checked_add(1)?))
        .and_then(|words| words.checked_add(1)) // argc
        .ok_or(E2BIG)?;
    let table_bytes = pointer_words
        .checked_mul(word)
        .and_then(|pointer_bytes| pointer_bytes.checked_add(auxv_bytes))
        .ok_or(E2BIG)?;
    let padding = (USER_STACK_ABI_ALIGN - (table_bytes & (USER_STACK_ABI_ALIGN - 1)))
        & (USER_STACK_ABI_ALIGN - 1);
    checked_add_exec_bytes(&mut bytes, padding)?;
    checked_add_exec_bytes(&mut bytes, table_bytes)?;
    if bytes > USER_STACK_INIT_SIZE.saturating_sub(PAGE_SIZE) {
        return Err(E2BIG);
    }
    Ok(())
}

fn read_exec_vectors(
    token: usize,
    mut argv: *const *const u8,
    mut envp: *const *const u8,
) -> Result<(Vec<String>, Vec<String>), isize> {
    let mut argv_vec: Vec<String> = Vec::new();
    if argv_vec.try_reserve(16).is_err() {
        return Err(ENOMEM);
    }

    let mut envp_vec: Vec<String> = Vec::new();
    if envp_vec.try_reserve(16).is_err() {
        return Err(ENOMEM);
    }

    let mut arg_env_bytes = 0usize;
    if !argv.is_null() {
        loop {
            let arg_ptr = UserPtr::new(argv).read(token)?;
            if arg_ptr.is_null() {
                break;
            }
            if argv_vec.try_reserve(1).is_err() {
                return Err(ENOMEM);
            }
            let arg = UserCString::new(arg_ptr).read(token)?;
            account_exec_string_bytes(&mut arg_env_bytes, &arg)?;
            argv_vec.push(arg);
            unsafe {
                argv = argv.add(1);
            }
        }
    }
    if argv_vec.is_empty() {
        if argv_vec.try_reserve(1).is_err() {
            return Err(ENOMEM);
        }
        argv_vec.push(String::new());
        account_exec_string_bytes(&mut arg_env_bytes, "")?;
    }

    if !envp.is_null() {
        loop {
            let env_ptr = UserPtr::new(envp).read(token)?;
            if env_ptr.is_null() {
                break;
            }
            if envp_vec.try_reserve(1).is_err() {
                return Err(ENOMEM);
            }
            let env = UserCString::new(env_ptr).read(token)?;
            account_exec_string_bytes(&mut arg_env_bytes, &env)?;
            envp_vec.push(env);
            unsafe {
                envp = envp.add(1);
            }
        }
    }

    Ok((argv_vec, envp_vec))
}

fn make_abs_exec_path(path: &str, base_path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else if path.is_empty() {
        base_path.to_string()
    } else if base_path == "/" {
        alloc::format!("/{}", path)
    } else {
        alloc::format!("{}/{}", base_path, path)
    }
}

fn exec_opened_file(
    cwd_inode: &Arc<dyn vfs::IndexNode>,
    path: &str,
    abs_path: String,
    file: Arc<vfs::File>,
    mut argv_vec: Vec<String>,
    envp_vec: Vec<String>,
) -> isize {
    if file.get_size() < 4 {
        return ENOEXEC;
    }

    let mut magic_number = [0u8; 4];
    let _ = file.pread(0, &mut magic_number);
    let elf = if &magic_number == b"\x7fELF" {
        file
    } else if &magic_number[..2] == b"#!" {
        if let Ok(Some((interp, shebang_arg))) = parse_shebang(&file) {
            match open_exec(cwd_inode, &interp).and_then(|f| {
                if !f.is_dir() && is_valid_elf(&f) {
                    Ok(f)
                } else {
                    Err(ENOEXEC)
                }
            }) {
                Ok(f) => {
                    let mut script_argv = Vec::new();
                    if script_argv
                        .try_reserve(3usize.saturating_add(argv_vec.len()))
                        .is_err()
                    {
                        return ENOMEM;
                    }
                    script_argv.push(interp);
                    if let Some(arg) = shebang_arg {
                        script_argv.push(arg);
                    }
                    script_argv.push(path.to_string());
                    for arg in argv_vec.iter().skip(1) {
                        script_argv.push(arg.clone());
                    }
                    argv_vec = script_argv;
                    f
                }
                Err(_) => match try_open_shell_fallback(cwd_inode, &mut argv_vec, path) {
                    Ok(f) => f,
                    Err(e) => return e,
                },
            }
        } else {
            match try_open_shell_fallback(cwd_inode, &mut argv_vec, path) {
                Ok(f) => f,
                Err(e) => return e,
            }
        }
    } else {
        return ENOEXEC;
    };

    if let Err(errno) = validate_exec_stack_usage(&argv_vec, &envp_vec) {
        return errno;
    }

    if elf.is_dir() {
        return EISDIR;
    }

    let task = current_task().unwrap();
    show_frame_consumption! {
        "load_elf";
        // Try the PageCache-backed demand-paged loader first; fall back to the
        // eager kmap loader if the filesystem doesn't provide a PageCache.
        crate::task::perf::record_exec_direct();
        let result = task.load_elf_direct(elf.clone(), &argv_vec, &envp_vec);
        let result = match result {
            Err(ENOSYS) => {
                crate::task::perf::record_exec_direct_enosys();
                crate::task::perf::record_exec_fallback();
                task.load_elf(elf, &argv_vec, &envp_vec)
            }
            other => other,
        };
        if let Err(_errno) = result {
            // exit 切回 idle 后不会返回本 syscall 栈帧，不能把本地 Arc 留在栈上。
            drop(task);
            exit_current_and_run_next(127);
        };
    }
    task.process.mark_execed();
    task.set_exec_comm(&abs_path);
    task.process.set_exec_identity(abs_path, &argv_vec);
    task.process.complete_vfork();
    SUCCESS
}

fn clone_fd_file(fd: usize) -> Result<Arc<vfs::File>, isize> {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let fd_table = files_ref.lock();
    let file = fd_table.get_file(fd).map_err(|e| -(e as isize))?;
    Ok(file)
}

fn reopen_exec_fd(file: &vfs::File) -> Result<Arc<vfs::File>, isize> {
    let exec_file =
        vfs::File::new(file.inode.clone(), vfs::FileFlags::O_RDONLY).map_err(|e| -(e as isize))?;
    check_exec_metadata(&exec_file)?;
    Ok(exec_file)
}

fn resolve_exec_start_inode(dirfd: usize, path: &str) -> Result<Arc<dyn vfs::IndexNode>, isize> {
    let task = current_task().unwrap();
    if path.starts_with('/') || dirfd == crate::syscall::fs::AT_FDCWD {
        return Ok(task.process.fs().lock().working_inode.inode.clone());
    }

    let file = clone_fd_file(dirfd)?;
    let metadata = file.metadata().map_err(|e| -(e as isize))?;
    if metadata.file_type != vfs::FileType::Dir {
        return Err(ENOTDIR);
    }
    Ok(file.inode.clone())
}

pub fn sys_execve(pathname: *const u8, argv: *const *const u8, envp: *const *const u8) -> isize {
    let task = current_task().unwrap();
    let token = current_user_token();
    let fs_ref = task.process.fs();
    let path = match UserCString::new(pathname).read(token) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    if let Err(errno) = validate_exec_path_len(&path) {
        return errno;
    }
    let (argv_vec, envp_vec) = match read_exec_vectors(token, argv, envp) {
        Ok(v) => v,
        Err(errno) => return errno,
    };
    let (working_inode, working_path) = {
        let lock = fs_ref.lock();
        (lock.working_inode.clone(), lock.working_path.clone())
    };
    let cwd_inode: Arc<dyn vfs::IndexNode> = working_inode.inode.clone();
    let abs_path = make_abs_exec_path(&path, &working_path);

    match open_exec(&cwd_inode, &path) {
        Ok(file) => exec_opened_file(&cwd_inode, &path, abs_path, file, argv_vec, envp_vec),
        Err(errno) => errno,
    }
}

pub fn sys_execveat(
    dirfd: usize,
    pathname: *const u8,
    argv: *const *const u8,
    envp: *const *const u8,
    flags: u32,
) -> isize {
    const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
    const AT_EMPTY_PATH: u32 = 0x1000;
    const VALID_FLAGS: u32 = AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH;

    let task = current_task().unwrap();
    let token = current_user_token();
    let fs_ref = task.process.fs();
    let path = match UserCString::new(pathname).read(token) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    if (flags & !VALID_FLAGS) != 0 {
        return EINVAL;
    }
    if let Err(errno) = validate_exec_path_len(&path) {
        return errno;
    }
    let (argv_vec, envp_vec) = match read_exec_vectors(token, argv, envp) {
        Ok(v) => v,
        Err(errno) => return errno,
    };

    if path.is_empty() {
        if (flags & AT_EMPTY_PATH) == 0 {
            return ENOENT;
        }
        if dirfd == crate::syscall::fs::AT_FDCWD {
            return ENOENT;
        }
        let fd_file = match clone_fd_file(dirfd) {
            Ok(file) => file,
            Err(errno) => return errno,
        };
        let file = match reopen_exec_fd(&fd_file) {
            Ok(file) => file,
            Err(errno) => return errno,
        };
        let abs_path = fd_file
            .inode
            .absolute_path()
            .unwrap_or_else(|_| alloc::format!("/dev/fd/{}", dirfd));
        return exec_opened_file(&fd_file.inode, &path, abs_path, file, argv_vec, envp_vec);
    }

    let start_inode = match resolve_exec_start_inode(dirfd, &path) {
        Ok(inode) => inode,
        Err(errno) => return errno,
    };
    let follow_final = (flags & AT_SYMLINK_NOFOLLOW) == 0;
    let base_path = start_inode
        .absolute_path()
        .unwrap_or_else(|_| fs_ref.lock().working_path.clone());
    let abs_path = make_abs_exec_path(&path, &base_path);
    match open_exec_with_follow(&start_inode, &path, follow_final) {
        Ok(file) => exec_opened_file(&start_inode, &path, abs_path, file, argv_vec, envp_vec),
        Err(errno) => errno,
    }
}
