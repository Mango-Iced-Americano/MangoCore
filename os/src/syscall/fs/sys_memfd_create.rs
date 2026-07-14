use super::common::*;

pub fn sys_memfd_create(name: *const u8, flags: u32) -> isize {
    if let Err(err) = validate_memfd_flags(flags) {
        return -(err as isize);
    }

    let task = current_task().unwrap();
    let token = task.get_user_token();
    let name = match user_cstring(token, name) {
        Ok(name) => name,
        Err(errno) => return errno,
    };
    if name.len() > MEMFD_NAME_MAX {
        return EINVAL;
    }

    let open_flags = OpenFlags::O_CREAT | OpenFlags::O_EXCL | OpenFlags::O_RDWR;
    let create_mode = vfs::InodeMode::from_bits_truncate(0o600);
    let mut last_errno = EEXIST;
    let file = {
        let tid = task.gettid();
        let mut created = None;
        for _ in 0..8 {
            let id = MEMFD_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = alloc::format!("/dev/shm/.memfd-{}-{}", tid, id);
            match open_file_at(AT_FDCWD, &path, open_flags, create_mode) {
                Ok(file) => {
                    created = Some(file);
                    break;
                }
                Err(errno) if errno == EEXIST => {
                    last_errno = errno;
                }
                Err(errno) => return errno,
            }
        }
        match created {
            Some(file) => file,
            None => return last_errno,
        }
    };

    let initial_seals = if (flags & MFD_ALLOW_SEALING) != 0 {
        0
    } else {
        vfs::F_SEAL_SEAL
    };
    file.set_memfd_seals(Arc::new(AtomicUsize::new(initial_seals)));

    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();
    match fd_table.alloc_fd(file, (flags & MFD_CLOEXEC) != 0) {
        Ok(fd) => fd as isize,
        Err(e) => -(e as isize),
    }
}
