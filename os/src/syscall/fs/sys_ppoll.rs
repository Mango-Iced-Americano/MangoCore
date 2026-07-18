use super::common::*;

pub fn sys_ppoll(fds: usize, nfds: usize, tmo_p: usize, sigmask: usize) -> isize {
    ppoll(
        fds as *mut PollFd,
        nfds,
        tmo_p as *const TimeSpec,
        sigmask as *const crate::task::Signals,
    )
}
