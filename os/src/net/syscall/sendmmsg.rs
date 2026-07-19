use crate::mm::UserPtrMut;
use crate::net::posix::MMsgHdr;
use crate::task::current_task;
use crate::utils::error::SyscallErr;

use super::sendmsg::sys_sendmsg;

const UIO_MAXIOV: u32 = 1024;

/// Send multiple messages through one socket.
///
/// Each entry uses `sendmsg` validation and blocking semantics. Once at least
/// one message succeeds, a later error is reported as a short batch so callers
/// can retry from the first unsent entry, matching Linux behavior.
pub fn sys_sendmmsg(sockfd: u32, msgvec: usize, vlen: u32, flags: u32) -> isize {
    if vlen == 0 {
        return 0;
    }
    if vlen > UIO_MAXIOV {
        return -(SyscallErr::EINVAL as isize);
    }

    let Some(task) = current_task() else {
        return -(SyscallErr::ESRCH as isize);
    };
    let token = task.get_user_token();
    let entry_size = core::mem::size_of::<MMsgHdr>();
    let msg_len_offset = core::mem::offset_of!(MMsgHdr, msg_len);
    let mut sent = 0u32;

    for index in 0..vlen as usize {
        let entry_ptr = index
            .checked_mul(entry_size)
            .and_then(|offset| msgvec.checked_add(offset));
        let Some(entry_ptr) = entry_ptr else {
            return batch_error(sent, SyscallErr::EFAULT);
        };

        let result = sys_sendmsg(sockfd, entry_ptr, flags);
        if result < 0 {
            return if sent > 0 { sent as isize } else { result };
        }

        let Some(msg_len_ptr) = entry_ptr.checked_add(msg_len_offset) else {
            return batch_error(sent, SyscallErr::EFAULT);
        };
        if UserPtrMut::<u32>::from_addr(msg_len_ptr)
            .write(token, &(result as u32))
            .is_err()
        {
            return batch_error(sent, SyscallErr::EFAULT);
        }
        sent += 1;
    }

    sent as isize
}

fn batch_error(sent: u32, error: SyscallErr) -> isize {
    if sent > 0 {
        sent as isize
    } else {
        -(error as isize)
    }
}
