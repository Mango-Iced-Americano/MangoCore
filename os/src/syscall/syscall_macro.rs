/// 根据给出的 sockfd，返回 socket，找不到则返回 ENOTSOCK
#[macro_export]
macro_rules! get_socket {
    ($sockfd:expr) => {{
        let task = crate::task::current_task().unwrap();
        let fd_table = task.files.lock();
        let fd_ref = match fd_table.get_ref($sockfd as usize) {
            Err(e) => return e,
            Ok(fd) => {
                // O_PATH 打开的 fd 视为 inoperable，应返回 EBADF
                if fd.get_flags().contains(crate::fs::OpenFlags::O_PATH) {
                    return -(crate::utils::error::SyscallErr::EBADF as isize);
                }
                fd
            }
        };
        // downcast File → SocketFile → 取 .inner 拿到 Arc<dyn Socket>
        match fd_ref.file.clone().downcast_arc::<crate::net::SocketFile>() {
            Ok(socket_file) => socket_file.inner.clone(),
            Err(_) => return crate::syscall::errno::ENOTSOCK,
        }
    }};
}

/// 根据给出的 addr 和 addrlen，将用户空间的虚拟地址转化为物理地址buf，地址不合法返回错误
#[macro_export]
macro_rules! trans_ref {
    ($addr:expr, $addrlen:expr) => {{
        let token = crate::task::current_task().unwrap().get_user_token();
        // access_ok: 用户地址必须在 [USER_VA_BASE, USER_VA_BASE + TASK_SIZE) 范围内，且不溢出
        // 防止地址 0xFFFFFFFFFFFFFFFF 等非法值绕过 translated_byte_buffer 的整数溢出
        let addr_val = $addr as usize;
        let len_val = $addrlen as usize;
        let user_va_base = crate::hal::config::USER_VA_BASE;
        let user_va_end = crate::hal::config::USER_VA_END;
        if addr_val < user_va_base
            || addr_val >= user_va_end
            || len_val > crate::hal::config::TASK_SIZE
            || addr_val.checked_add(len_val).is_none()
            || addr_val + len_val > user_va_end
        {
            return crate::syscall::errno::EFAULT;
        }
        // NULL 指针（addr=0）且 len>0 直接返回 EFAULT
        if addr_val == 0 && len_val > 0 {
            return crate::syscall::errno::EFAULT;
        }
        // 校验整个 [addr, addr+addrlen) 范围：translated_byte_buffer 逐页遍历，
        // 任一页缺页/越权都通过 check_page_fault → EFAULT
        if crate::mm::translated_byte_buffer(token, $addr as *const u8, $addrlen as usize).is_err()
        {
            return crate::syscall::errno::EFAULT;
        }
        // 校验通过后 translated_ref 不会失败，直接 unwrap
        let addr = crate::mm::translated_ref(token, $addr as *const u8).unwrap();
        unsafe { core::slice::from_raw_parts(addr as *const u8, $addrlen as usize) }
    }};
}

/// trans_ref! 的可变引用版本，返回 &mut [u8]
#[macro_export]
macro_rules! trans_refmut {
    ($addr:expr, $addrlen:expr) => {{
        let token = crate::task::current_task().unwrap().get_user_token();
        let addr_val = $addr as usize;
        let len_val = $addrlen as usize;
        let user_va_base = crate::hal::config::USER_VA_BASE;
        let user_va_end = crate::hal::config::USER_VA_END;
        if addr_val < user_va_base
            || addr_val >= user_va_end
            || len_val > crate::hal::config::TASK_SIZE
            || addr_val.checked_add(len_val).is_none()
            || addr_val + len_val > user_va_end
        {
            return crate::syscall::errno::EFAULT;
        }
        // NULL 指针（addr=0）且 len>0 直接返回 EFAULT
        if addr_val == 0 && len_val > 0 {
            return crate::syscall::errno::EFAULT;
        }
        if crate::mm::translated_byte_buffer(token, $addr as *const u8, $addrlen as usize).is_err()
        {
            return crate::syscall::errno::EFAULT;
        }
        let addr = crate::mm::translated_refmut(token, $addr as *mut u8).unwrap();
        unsafe { core::slice::from_raw_parts_mut(addr as *mut u8, $addrlen as usize) }
    }};
}
