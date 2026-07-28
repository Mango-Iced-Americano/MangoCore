//! Regression: SysV message queues explicitly wake blocked receivers.

use user_lib::{println, sleep};
use user_lib::syscall::*;

const IPC_PRIVATE: isize = 0;
const IPC_CREAT: usize = 0o1000;
const IPC_RMID: usize = 0;
const MESSAGE_TYPE: isize = 1;
const MESSAGE: [u8; 5] = *b"hello";
const SEND_DELAY_MS: usize = 100;
const MIN_BLOCK_MS: isize = 40;
const WATCHDOG_DELAY_MS: usize = 5_000;
const SIGKILL: usize = 9;
const SIGTERM: usize = 15;

#[repr(C)]
struct Message {
    mtype: isize,
    data: [u8; MESSAGE.len()],
}

const fn outgoing_message() -> Message {
    Message {
        mtype: MESSAGE_TYPE,
        data: MESSAGE,
    }
}

fn reap(pid: isize) -> bool {
    let mut status = 0;
    sys_waitpid(pid, &mut status) == pid && status == 0
}

fn receive_message(msqid: i32) -> (isize, Message) {
    let mut message = Message {
        mtype: 0,
        data: [0; MESSAGE.len()],
    };
    let received = sys_msgrcv(
        msqid,
        &mut message as *mut Message as *mut u8,
        message.data.len(),
        0,
        0,
    );
    (received, message)
}

fn is_expected_message(received: isize, message: &Message) -> bool {
    received == MESSAGE.len() as isize && message.mtype == MESSAGE_TYPE && message.data == MESSAGE
}

fn start_watchdog() -> isize {
    let parent = sys_getpid();
    let watchdog = sys_fork();
    if watchdog == 0 {
        sleep(WATCHDOG_DELAY_MS);
        let _ = sys_kill(parent as usize, SIGTERM);
        sys_exit(1);
    }
    watchdog
}

fn stop_watchdog(watchdog: isize) {
    if watchdog > 0 {
        let _ = sys_kill(watchdog as usize, SIGKILL);
        let _ = reap(watchdog);
    }
}

pub fn run() -> i32 {
    println!("[regression_ipc_msg] start");
    let watchdog = start_watchdog();
    let result = (|| -> bool {
        if watchdog < 0 {
            return false;
        }

        let msqid = sys_msgget(IPC_PRIVATE, IPC_CREAT | 0o666);
        if msqid < 0 {
            return false;
        }
        let msqid = msqid as i32;

        let sender = sys_fork();
        if sender == 0 {
            sleep(SEND_DELAY_MS);
            let message = outgoing_message();
            let sent = sys_msgsnd(
                msqid,
                &message as *const Message as *const u8,
                message.data.len(),
                0,
            );
            sys_exit(if sent == 0 { 0 } else { 1 });
        }
        if sender < 0 {
            let _ = sys_msgctl(msqid, IPC_RMID, 0);
            return false;
        }

        let start = sys_get_time();
        let (received, message) = receive_message(msqid);
        let elapsed = sys_get_time() - start;
        let first_ok = elapsed >= MIN_BLOCK_MS && is_expected_message(received, &message) && reap(sender);
        if !first_ok {
            let _ = sys_msgctl(msqid, IPC_RMID, 0);
            return false;
        }

        let receiver = sys_fork();
        if receiver == 0 {
            let start = sys_get_time();
            let (received, message) = receive_message(msqid);
            let elapsed = sys_get_time() - start;
            sys_exit(if elapsed >= MIN_BLOCK_MS && is_expected_message(received, &message) {
                0
            } else {
                1
            });
        }
        if receiver < 0 {
            let _ = sys_msgctl(msqid, IPC_RMID, 0);
            return false;
        }

        sleep(SEND_DELAY_MS);
        let message = outgoing_message();
        let sent = sys_msgsnd(
            msqid,
            &message as *const Message as *const u8,
            message.data.len(),
            0,
        );
        let second_ok = sent == 0 && reap(receiver);
        let _ = sys_msgctl(msqid, IPC_RMID, 0);
        second_ok
    })();
    stop_watchdog(watchdog);

    if result {
        println!("[regression_ipc_msg] PASS");
        0
    } else {
        println!("[regression_ipc_msg] FAIL");
        1
    }
}
