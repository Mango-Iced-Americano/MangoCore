use super::common::*;

pub fn sys_pselect(
    nfds: usize,
    read_fds: *mut FdSet,
    write_fds: *mut FdSet,
    exception_fds: *mut FdSet,
    timeout: *mut TimeSpec,
    sigmask_args: usize,
) -> isize {
    if (nfds as isize) < 0 {
        return EINVAL;
    }

    // pselect6 syscall (SYS_PSELECT6=72) passes sigmask via a {ss, ss_len} structure:
    //   struct { const sigset_t *ss; size_t ss_len; };
    // args[5] points to this structure in user space, NOT directly to a sigset_t.
    // The musl wrapper builds it as: long data[2] = { (long)&mask, sizeof(sigset_t) }.
    let sigmask: *const crate::task::signal::Signals = if sigmask_args != 0 {
        let token = current_user_token();
        match UserPtr::<usize>::from_addr(sigmask_args).read(token) {
            Ok(ptr) => {
                if ptr != 0 {
                    ptr as *const crate::task::signal::Signals
                } else {
                    core::ptr::null()
                }
            }
            Err(errno) => return errno,
        }
    } else {
        core::ptr::null()
    };
    let token = current_user_token();
    let mut kread_fds = match UserPtr::new(read_fds as *const FdSet).read_optional(token) {
        Ok(fds) => fds,
        Err(errno) => return errno,
    };
    let mut kwrite_fds = match UserPtr::new(write_fds as *const FdSet).read_optional(token) {
        Ok(fds) => fds,
        Err(errno) => return errno,
    };
    let mut kexception_fds = match UserPtr::new(exception_fds as *const FdSet).read_optional(token) {
        Ok(fds) => fds,
        Err(errno) => return errno,
    };
    let ktimeout = match UserPtr::new(timeout as *const TimeSpec).read_optional(token) {
        Ok(timeout) => timeout,
        Err(errno) => return errno,
    };
    let mut ret = pselect(
        nfds,
        &mut kread_fds,
        &mut kwrite_fds,
        &mut kexception_fds,
        &ktimeout,
        sigmask,
    );
    if ret < 0 {
        return ret;
    }
    /*
    WARNING! The EFAULT errno is NOT mentioned in man for Linux.
    However, it is mentioned in BSD man, so we keep it anyway.
     */
    if let Some(kread_fds) = &kread_fds {
        trace!("[pselect] read_fds: {:?}", kread_fds);
        if UserPtrMut::new(read_fds).write(token, kread_fds).is_err() {
            log::error!("[sys_pselect] Error copying to read_fds {:?}", read_fds);
            ret = EFAULT;
        };
    }
    if let Some(kwrite_fds) = &kwrite_fds {
        trace!("[pselect] write_fds: {:?}", kwrite_fds);
        if UserPtrMut::new(write_fds).write(token, kwrite_fds).is_err() {
            log::error!("[sys_pselect] Error copying to write_fds {:?}", write_fds);
            ret = EFAULT;
        };
    }
    if let Some(kexception_fds) = &kexception_fds {
        trace!("[pselect] exception_fds: {:?}", kexception_fds);
        if UserPtrMut::new(exception_fds)
            .write(token, kexception_fds)
            .is_err()
        {
            log::error!(
                "[sys_pselect] Error copying to exception_fds {:?}",
                exception_fds
            );
            ret = EFAULT;
        };
    }

    ret
}
