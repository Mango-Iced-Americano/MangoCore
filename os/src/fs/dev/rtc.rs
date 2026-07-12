use alloc::sync::Arc;
use core::any::Any;

use crate::fs::dev::DEV_FS;
use crate::fs::vfs::file_system::FileSystem as NewFileSystem;
use crate::fs::vfs::{FilePrivateData, FileType, IndexNode, InodeFlags, InodeMode, Metadata};
use crate::mm::UserPtrMut;
use crate::timer::{current_time_safe, TimeSpec};
use crate::utils::error::SyscallErr;

const RTC_RD_TIME: u32 = 0x8024_7009;
const SECONDS_PER_DAY: u64 = 86_400;

#[derive(Debug)]
pub struct Rtc;

#[repr(C)]
#[derive(Clone, Copy)]
struct RtcTime {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
}

fn is_leap_year(year: u32) -> bool {
    year % 4 == 0 && year % 100 != 0 || year % 400 == 0
}

fn days_in_year(year: u32) -> u32 {
    if is_leap_year(year) {
        366
    } else {
        365
    }
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        0 => 31,
        1 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        2 => 31,
        3 => 30,
        4 => 31,
        5 => 30,
        6 => 31,
        7 => 31,
        8 => 30,
        9 => 31,
        10 => 30,
        11 => 31,
        _ => 30,
    }
}

fn rtc_time_from_unix(timestamp: u64) -> RtcTime {
    let mut days = timestamp / SECONDS_PER_DAY;
    let sec_of_day = timestamp % SECONDS_PER_DAY;
    let tm_hour = (sec_of_day / 3600) as i32;
    let tm_min = ((sec_of_day % 3600) / 60) as i32;
    let tm_sec = (sec_of_day % 60) as i32;
    let tm_wday = ((days + 4) % 7) as i32; // 1970-01-01 was Thursday.

    let mut year = 1970u32;
    while days >= days_in_year(year) as u64 {
        days -= days_in_year(year) as u64;
        year += 1;
    }
    let tm_yday = days as i32;

    let mut month = 0u32;
    while days >= days_in_month(year, month) as u64 {
        days -= days_in_month(year, month) as u64;
        month += 1;
    }

    RtcTime {
        tm_sec,
        tm_min,
        tm_hour,
        tm_mday: days as i32 + 1,
        tm_mon: month as i32,
        tm_year: year as i32 - 1900,
        tm_wday,
        tm_yday,
        tm_isdst: 0,
    }
}

impl IndexNode for Rtc {
    fn ioctl(
        &self,
        cmd: u32,
        argp: usize,
        _private_data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        match cmd {
            RTC_RD_TIME => {
                let token = crate::task::current_user_token();
                let rtc_time = rtc_time_from_unix(current_time_safe());
                UserPtrMut::from_addr(argp)
                    .write(token, &rtc_time)
                    .map_err(|_| SyscallErr::EFAULT)?;
                Ok(0)
            }
            _ => Err(SyscallErr::ENOTTY),
        }
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        Ok(Metadata {
            dev_id: 0,
            inode_id: 0,
            size: 0,
            blk_size: 0,
            blocks: 0,
            atime: TimeSpec::new(),
            mtime: TimeSpec::new(),
            ctime: TimeSpec::new(),
            file_type: FileType::CharDevice,
            mode: InodeMode::S_IFCHR | InodeMode::from_bits_truncate(0o666),
            nlinks: 1,
            uid: 0,
            gid: 0,
            flags: InodeFlags::empty(),
            raw_dev: crate::makedev!(10, 135),
        })
    }

    fn is_stream(&self) -> bool {
        true
    }

    fn fs(&self) -> Arc<dyn NewFileSystem> {
        DEV_FS.clone()
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}
