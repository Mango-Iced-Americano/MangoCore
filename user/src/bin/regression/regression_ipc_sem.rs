//! Regression: a SysV semaphore decrement blocks until another process releases it.

use user_lib::{println, sleep};
use user_lib::syscall::*;

const IPC_PRIVATE: isize = 0;
const IPC_CREAT: usize = 0o1000;
const IPC_RMID: usize = 0;
const SETVAL: usize = 16;
const RELEASE_DELAY_MS: usize = 100;
const MIN_BLOCK_MS: isize = 40;
const WATCHDOG_DELAY_MS: usize = 5_000;
const SIGKILL: usize = 9;
const SIGTERM: usize = 15;

fn reap(pid: isize) -> bool {
    let mut status = 0;
    sys_waitpid(pid, &mut status) == pid && status == 0
}

pub fn run() -> i32 {
    println!("[regression_ipc_sem] start");

    let semid = sys_semget(IPC_PRIVATE, 1, IPC_CREAT | 0o666);
    if semid < 0 {
        println!("FAIL: semget returned {}", semid);
        return 1;
    }
    let semid = semid as i32;
    if sys_semctl(semid, 0, SETVAL, 0) != 0 {
        println!("FAIL: semctl SETVAL failed");
        let _ = sys_semctl(semid, 0, IPC_RMID, 0);
        return 1;
    }

    let releaser = sys_fork();
    if releaser == 0 {
        sleep(RELEASE_DELAY_MS);
        let release = [SemBuf { sem_num: 0, sem_op: 1, sem_flg: 0 }];
        sys_exit(if sys_semop(semid, &release) == 0 { 0 } else { 1 });
    }
    if releaser < 0 {
        println!("FAIL: releaser fork returned {}", releaser);
        let _ = sys_semctl(semid, 0, IPC_RMID, 0);
        return 1;
    }

    let parent = sys_getpid();
    let watchdog = sys_fork();
    if watchdog == 0 {
        sleep(WATCHDOG_DELAY_MS);
        let _ = sys_kill(parent as usize, SIGTERM);
        sys_exit(1);
    }
    if watchdog < 0 {
        println!("FAIL: watchdog fork returned {}", watchdog);
        let _ = sys_kill(releaser as usize, SIGKILL);
        let _ = reap(releaser);
        let _ = sys_semctl(semid, 0, IPC_RMID, 0);
        return 1;
    }

    let acquire = [SemBuf { sem_num: 0, sem_op: -1, sem_flg: 0 }];
    let start = sys_get_time();
    let acquired = sys_semop(semid, &acquire);
    let elapsed = sys_get_time() - start;
    let _ = sys_kill(watchdog as usize, SIGKILL);
    let releaser_ok = reap(releaser);
    let _ = reap(watchdog);
    let _ = sys_semctl(semid, 0, IPC_RMID, 0);

    if acquired != 0 || elapsed < MIN_BLOCK_MS || !releaser_ok {
        println!(
            "FAIL: acquire={} elapsed={}ms releaser_ok={}",
            acquired, elapsed, releaser_ok
        );
        return 1;
    }

    println!("[regression_ipc_sem] PASS: blocked {}ms", elapsed);
    0
}
