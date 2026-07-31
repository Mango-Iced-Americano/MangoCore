use super::common::*;

pub fn sys_fcntl(fd: usize, cmd: u32, arg: usize) -> isize {
    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();

    info!(
        "[sys_fcntl] fd: {}, cmd: {:?}, arg: {:X}",
        fd,
        FcntlCommand::try_from_primitive(cmd).ok(),
        arg
    );

    let command = match FcntlCommand::try_from_primitive(cmd) { Ok(c) => c, Err(_) => return -(SyscallErr::EINVAL as isize), };
    match command {
        FcntlCommand::DupFd | FcntlCommand::DupFdCloexec => {
            let cloexec = matches!(command, FcntlCommand::DupFdCloexec);
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            };

            match fd_table.alloc_fd_from(arg, file, cloexec) {
                Ok(fd) => fd as isize,
                Err(e) => -(e as isize),
            }
        }
        FcntlCommand::GetFd => {
            // Check that fd is valid first
            match fd_table.get_file(fd) { Ok(_) => {}, Err(e) => return -(e as isize), };
            fd_table.get_cloexec(fd) as isize
        }
        FcntlCommand::SetFd => {
            match fd_table.set_cloexec(fd, (arg & vfs::FD_CLOEXEC) != 0) { Ok(_) => {}, Err(e) => return -(e as isize), };
            if (arg & !vfs::FD_CLOEXEC) != 0 {
                warn!("[fcntl] Unsupported flag exists: {:X}", arg);
            }
            SUCCESS
        }
        FcntlCommand::SetFlags => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            };

            // O_PATH fds do not support F_SETFL (Linux semantics: EBADF).
            if is_path_fd(&file) {
                return EBADF;
            }

            // Preserve old access mode, only update SETFL-allowed status bits
            let old_flags = file.flags();
            let old_async = old_flags.contains(vfs::FileFlags::O_ASYNC);
            let old_access = old_flags.access_flags().bits();
            const ACCMODE_MASK: u32 = 0o3;
            let arg_without_accmode = (arg as u32) & !ACCMODE_MASK;
            let new_flags = vfs::FileFlags::from_bits_truncate(arg_without_accmode | old_access);
            match file.set_flags(new_flags) {
                Ok(()) => {
                    let new_async = new_flags.contains(vfs::FileFlags::O_ASYNC);
                    if new_async != old_async {
                        let _ = vfs::fasync::set_file_fasync(&file, fd as i32, new_async);
                    }
                    SUCCESS
                }
                Err(e) => -(e as isize),
            }
        }
        FcntlCommand::GetFlags => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            };
            let bits = file.flags().bits();
            ((bits & 0o3) | (bits & vfs::STATUS_MASK)) as isize
        }
        FcntlCommand::GetLock => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            };
            let owner_pid = task.pid();
            drop(fd_table);
            fcntl_getlk(&file, arg, owner_pid)
        }
        FcntlCommand::OfdGetLock => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file.clone(),
                Err(e) => return -(e as isize),
            };
            drop(fd_table);
            fcntl_getlk_ofd(&file, arg)
        }
        FcntlCommand::SetLock
        | FcntlCommand::SetLockWait => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            };
            let owner_pid = task.pid();
            let wait = matches!(command, FcntlCommand::SetLockWait);
            drop(fd_table);
            fcntl_setlk(&file, arg, owner_pid, wait)
        }
        FcntlCommand::OfdSetLock
        | FcntlCommand::OfdSetLockWait => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file.clone(),
                Err(e) => return -(e as isize),
            };
            let wait = matches!(command, FcntlCommand::OfdSetLockWait);
            drop(fd_table);
            fcntl_setlk_ofd(&file, arg, wait)
        }
        FcntlCommand::SetPipeSize | FcntlCommand::GetPipeSize => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            };
            let pipe = match file.inode_as_any_ref().downcast_ref::<Pipe>() {
                Some(pipe) => pipe,
                None => return EINVAL,
            };
            match command {
                FcntlCommand::GetPipeSize => pipe.pipe_capacity() as isize,
                FcntlCommand::SetPipeSize => match pipe.set_pipe_capacity_compat(arg) {
                    Ok(size) => size as isize,
                    Err(e) => -(e as isize),
                },
                _ => unreachable!(),
            }
        }
        FcntlCommand::AddSeals => {
            const VALID_SEALS: usize = vfs::F_SEAL_SEAL
                | vfs::F_SEAL_SHRINK
                | vfs::F_SEAL_GROW
                | vfs::F_SEAL_WRITE
                | vfs::F_SEAL_FUTURE_WRITE;

            if (arg & !VALID_SEALS) != 0 {
                return EINVAL;
            }
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            };
            let seals = match file.memfd_seals() {
                Some(seals) => seals,
                None => return EINVAL,
            };
            if file.writable().is_err() {
                return EPERM;
            }
            let old = seals.load(Ordering::SeqCst);
            if (old & vfs::F_SEAL_SEAL) != 0 {
                return EPERM;
            }
            if (arg & vfs::F_SEAL_WRITE) != 0 {
                let inode = vfs::MountFSInode::unwrap_inode(&file.inode);
                let vm_ref = task.process.vm();
                if vm_ref.read(|memory_set| memory_set.has_shared_writable_mapping(&inode)) {
                    return EBUSY;
                }
            }
            seals.store(old | arg, Ordering::SeqCst);
            SUCCESS
        }
        FcntlCommand::GetSeals => {
            let file = match fd_table.get_file(fd) {
                Ok(file) => file,
                Err(e) => return -(e as isize),
            };
            match file.memfd_seal_bits() {
                Some(seals) => seals as isize,
                None => EINVAL,
            }
        }
        FcntlCommand::SetOwn => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            let v = arg as i32;
            if v > 0 {
                file.set_owner_target(vfs::FileOwnerTarget::Pid(v as usize), v);
            } else if v < 0 {
                file.set_owner_target(vfs::FileOwnerTarget::Pgrp((-v) as usize), v);
            } else {
                file.set_owner_target(vfs::FileOwnerTarget::None, 0);
            }
            SUCCESS
        }
        FcntlCommand::GetOwn => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            file.owner_raw() as isize
        }
        FcntlCommand::SetSig => {
            if arg > 64 {
                return EINVAL;
            }
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            file.set_owner_signum(arg as i32);
            SUCCESS
        }
        FcntlCommand::GetSig => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            file.owner_signum() as isize
        }
        FcntlCommand::SetOwnEx => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            let token = current_user_token();
            let oe: vfs::FOwnerEx = match UserPtr::<vfs::FOwnerEx>::from_addr(arg)
                .read(token)
            {
                Ok(v) => v,
                Err(e) => return e,
            };
            match oe.type_ {
                vfs::F_OWNER_TID => match find_task_by_tid(oe.pid as usize) {
                    Some(t) => file.set_owner_target(vfs::FileOwnerTarget::Tid(t.gettid()), oe.pid),
                    None => return -(SyscallErr::ESRCH as isize),
                },
                vfs::F_OWNER_PID => {
                    file.set_owner_target(vfs::FileOwnerTarget::Pid(oe.pid as usize), oe.pid);
                }
                vfs::F_OWNER_PGRP => {
                    file.set_owner_target(vfs::FileOwnerTarget::Pgrp(oe.pid as usize), oe.pid)
                }
                _ => return EINVAL,
            }
            SUCCESS
        }
        FcntlCommand::GetOwnEx => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            let token = current_user_token();
            let s = file.owner_snapshot();
            let t = match &s.target {
                vfs::FileOwnerTarget::None | vfs::FileOwnerTarget::Pid(_) => vfs::F_OWNER_PID,
                vfs::FileOwnerTarget::Pgrp(_) => vfs::F_OWNER_PGRP,
                vfs::FileOwnerTarget::Tid(_) => vfs::F_OWNER_TID,
            };
            let pid = file.owner_raw();
            if UserPtrMut::<vfs::FOwnerEx>::from_addr(arg)
                .write(token, &vfs::FOwnerEx { type_: t, pid })
                .is_err()
            {
                return EFAULT;
            }
            SUCCESS
        }
        FcntlCommand::GetOwnerUids => ENOSYS,
        FcntlCommand::SetLease => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            let t = arg as i16;
            use crate::fs::vfs::fcntl::{F_RDLCK, F_WRLCK, F_UNLCK};
            match t {
                F_RDLCK => {
                    if !file.flags().is_readable() {
                        return -(SyscallErr::EAGAIN as isize);
                    }
                    if !file.flags().is_read_only()
                        || is_writable_inode_busy(&file.inode)
                    {
                        return -(SyscallErr::EAGAIN as isize);
                    }
                    *file.lease.lock() = Some(F_RDLCK);
                    SUCCESS
                }
                F_WRLCK => -(SyscallErr::EAGAIN as isize),
                F_UNLCK => {
                    *file.lease.lock() = None;
                    SUCCESS
                }
                _ => EINVAL,
            }
        }
        FcntlCommand::GetLease => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            let lease_val = *file.lease.lock();
            lease_val.unwrap_or(F_UNLCK) as isize
        }
        FcntlCommand::Notify => ENOSYS,
        FcntlCommand::CreatedQuery => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            if file.created_by_open() { 1 } else { 0 }
        }
        FcntlCommand::CancelLock => ENOSYS,
        FcntlCommand::GetRwHint | FcntlCommand::GetFileRwHint => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            let token = current_user_token();
            let v = *file.file_rw_hint.lock();
            match UserPtrMut::<u64>::from_addr(arg).write(token, &v) {
                Ok(()) => SUCCESS,
                Err(e) => e,
            }
        }
        FcntlCommand::SetRwHint | FcntlCommand::SetFileRwHint => {
            let file = match fd_table.get_file(fd) {
                Ok(f) => f,
                Err(e) => return -(e as isize),
            };
            let token = current_user_token();
            match UserPtr::<u64>::from_addr(arg).read(token) {
                Ok(v) => {
                    *file.file_rw_hint.lock() = v;
                    SUCCESS
                }
                Err(e) => e,
            }
        }
        command => {
            warn!("[fcntl] Unsupported command: {:?}", command);
            -(SyscallErr::EINVAL as isize)
        }
    }
}
