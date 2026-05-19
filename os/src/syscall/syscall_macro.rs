// 根据 fd 拿 socket
#[macro_export]
macro_rules! get_socket {
    ($sockfd:expr) => {{
        let task = crate::task::current_task().unwrap();
        let fd_table = task.files.lock();
        let file = match fd_table.get_file($sockfd as usize) {
            Err(e) => return -(e as isize),
            Ok(f) => {
                // O_PATH 打开的 fd 视为 inoperable，应返回 EBADF
                if f.flags().contains(crate::fs::vfs::FileFlags::O_PATH) {
                    return -(crate::utils::error::SyscallErr::EBADF as isize);
                }
                f
            }
        };
        // downcast IndexNode → SocketFile → 取 .inner 拿到 Arc<dyn Socket>
        let any_ref = file.inode.as_any_ref();
        match any_ref.downcast_ref::<crate::net::SocketFile>() {
            Some(socket_file) => socket_file.inner.clone(),
            None => return crate::syscall::errno::ENOTSOCK,
        }
    }};
}

// 用户输入 buffer 转成切片
#[macro_export]
macro_rules! trans_ref {
    ($addr:expr, $addrlen:expr) => {{
        let token = crate::task::current_task().unwrap().get_user_token();
        // access_ok: 用户地址必须在 [0, USER_VA_END) 范围内，且不溢出
        // 防止地址 0xFFFFFFFFFFFFFFFF 等非法值绕过 translated_byte_buffer 的整数溢出
        // PIE 程序可能有低地址映射，这里只查上界
        let addr_val = $addr as usize;
        let len_val = $addrlen as usize;
        // 长度为 0 就不碰用户地址
        if len_val == 0 {
            unsafe { core::slice::from_raw_parts(core::ptr::NonNull::<u8>::dangling().as_ptr(), 0) }
        } else {
            let user_va_end = crate::hal::config::USER_VA_END;
            if addr_val >= user_va_end
                || len_val > crate::hal::config::TASK_SIZE
                || addr_val.checked_add(len_val).is_none()
                || addr_val + len_val > user_va_end
            {
                return crate::syscall::errno::EFAULT;
            }
            // NULL 指针（addr=0）且 len>0 直接返回 EFAULT
            if addr_val == 0 {
                return crate::syscall::errno::EFAULT;
            }
            // 跨页逐页检查
            // 有坏页就返回 EFAULT
            if crate::mm::translated_byte_buffer(
                token,
                $addr as *const u8,
                $addrlen as usize,
                crate::mm::UserAccess::Read,
            )
            .is_err()
            {
                return crate::syscall::errno::EFAULT;
            }
            // 范围查过后再拿首地址做切片
            let addr = crate::mm::translate_user_va_checked(
                token,
                crate::mm::VirtAddr::from($addr as usize),
                crate::mm::UserAccess::Read,
            )
            .unwrap()
            .get_ref::<u8>();
            unsafe { core::slice::from_raw_parts(addr as *const u8, $addrlen as usize) }
        }
    }};
}

// 用户输出 buffer 转成可写切片
#[macro_export]
macro_rules! trans_refmut {
    ($addr:expr, $addrlen:expr) => {{
        let token = crate::task::current_task().unwrap().get_user_token();
        let addr_val = $addr as usize;
        let len_val = $addrlen as usize;
        // 长度为 0 就不碰用户地址
        if len_val == 0 {
            unsafe {
                core::slice::from_raw_parts_mut(core::ptr::NonNull::<u8>::dangling().as_ptr(), 0)
            }
        } else {
            let user_va_end = crate::hal::config::USER_VA_END;
            if addr_val >= user_va_end
                || len_val > crate::hal::config::TASK_SIZE
                || addr_val.checked_add(len_val).is_none()
                || addr_val + len_val > user_va_end
            {
                return crate::syscall::errno::EFAULT;
            }
            // NULL 指针（addr=0）且 len>0 直接返回 EFAULT
            if addr_val == 0 {
                return crate::syscall::errno::EFAULT;
            }
            if crate::mm::translated_byte_buffer(
                token,
                $addr as *const u8,
                $addrlen as usize,
                crate::mm::UserAccess::Write,
            )
            .is_err()
            {
                return crate::syscall::errno::EFAULT;
            }
            let addr = crate::mm::translate_user_va_checked(
                token,
                crate::mm::VirtAddr::from($addr as usize),
                crate::mm::UserAccess::Write,
            )
            .unwrap()
            .get_mut::<u8>();
            unsafe { core::slice::from_raw_parts_mut(addr as *mut u8, $addrlen as usize) }
        }
    }};
}
