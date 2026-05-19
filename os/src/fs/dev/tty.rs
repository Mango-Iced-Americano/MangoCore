use crate::hal::console_getchar;
use crate::mm::{UserPtr, UserPtrMut};
use crate::syscall::errno::*;
use crate::task::signal::Signals;
use crate::task::WaitQueue;

use alloc::sync::Arc;
use core::any::Any;
use lazy_static::lazy_static;
use log::{info, warn};
use num_enum::FromPrimitive;
use spin::Mutex;

use crate::fs::vfs::{
    FilePrivateData, FileType, IndexNode, InodeFlags, InodeMode, Metadata,
};
use crate::fs::vfs::event::EPollEvent;
use crate::fs::vfs::file_system::FileSystem as NewFileSystem;
use crate::fs::dev::DEV_FS;
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;

lazy_static! {
    pub static ref TTY: Arc<Teletype> = Arc::new(Teletype::default());
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WinSize {
    ws_row: u16,
    ws_col: u16,
    xpixel: u16,
    ypixel: u16,
}

impl Default for WinSize {
    fn default() -> Self {
        Self {
            ws_row: 24,
            ws_col: 80,
            xpixel: 0,
            ypixel: 0,
        }
    }
}

pub struct TeletypeInner {
    last_char: u8,
    foreground_pgid: u32,
    winsize: WinSize,
    termios: Termios,
}

impl Default for TeletypeInner {
    fn default() -> Self {
        Self {
            last_char: 255,
            foreground_pgid: Default::default(),
            winsize: WinSize::default(),
            termios: Termios::default(),
        }
    }
}

pub struct Teletype {
    inner: Mutex<TeletypeInner>,
    read_waiters: Mutex<WaitQueue>,
}

impl Default for Teletype {
    fn default() -> Self {
        Self {
            inner: Mutex::new(TeletypeInner::default()),
            read_waiters: Mutex::new(WaitQueue::new()),
        }
    }
}

impl core::fmt::Debug for Teletype {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Teletype").finish()
    }
}

impl Teletype {
    pub fn new() -> Self {
        Default::default()
    }
}

/// If ISIG is set and `ch` is VINTR (default Ctrl-C, cc[0]), send SIGINT
/// to the foreground process group. Falls back to sending to all interruptible
/// tasks if fg_pgid is 0.
fn vintr_send_sigint(inner: &TeletypeInner, ch: u8) -> bool {
    if inner.termios.lflag & LocalModes::ISIG.bits() == 0
        || inner.termios.cc[0] == 255
        || ch != inner.termios.cc[0]
    {
        return false;
    }
    let fg_pgid = inner.foreground_pgid;
    if fg_pgid != 0 {
        let mut sent = false;
        for process in crate::task::find_processes_by_pgid(fg_pgid as usize) {
            crate::task::send_process_signal(&process, Signals::SIGINT);
            sent = true;
        }
        sent
    } else if let Some(task) = crate::task::current_task() {
        crate::task::send_process_signal(&task.process, Signals::SIGINT);
        true
    } else if fg_pgid == 0 {
        // Fallback: fg_pgid not set and no current task (scheduler loop).
        // Send SIGINT to all interruptible tasks (the actual foreground job).
        crate::task::send_signal_to_interruptible(Signals::SIGINT)
    } else {
        false
    }
}

impl Teletype {
    /// Called from the scheduler loop. Checks whether `ch` is VINTR and
    /// sends SIGINT to the appropriate task(s). Returns true if consumed.
    pub fn handle_vintr(ch: u8) -> bool {
        let inner = TTY.inner.lock();
        let result = vintr_send_sigint(&inner, ch);
        if result {
            let fg = inner.foreground_pgid;
            log::info!(
                "[vintr] ch={:#x} VINTR={:#x} ISIG={} fg_pgid={} sigint_sent=true",
                ch,
                inner.termios.cc[0],
                inner.termios.lflag & LocalModes::ISIG.bits() != 0,
                fg,
            );
        }
        drop(inner);
        result
    }

    /// Read foreground_pgid for debugging.
    pub fn foreground_pgid() -> u32 {
        TTY.inner.lock().foreground_pgid
    }
}

impl IndexNode for Teletype {
    fn read_at(
        &self,
        _offset: usize,
        _len: usize,
        buf: &mut [u8],
        _data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        if buf.is_empty() {
            return Ok(0);
        }
        let result;
        {
            let mut inner = self.inner.lock();
            if inner.last_char == 255 {
                inner.last_char =
                    crate::trace::pop_stashed().unwrap_or_else(|| console_getchar() as u8);
                crate::trace::check_magic_key(inner.last_char, "tty:read_at");
                if vintr_send_sigint(&inner, inner.last_char) {
                    inner.last_char = 255;
                    return Err(SyscallErr::EAGAIN);
                }
            }
            if inner.last_char == 255 {
                return Err(SyscallErr::EAGAIN);
            }
            buf[0] = inner.last_char;
            if inner.termios.lflag & LocalModes::ECHO.bits() != 0 {
                if inner.last_char == b'\r' {
                    print!("\n");
                } else {
                    log::info!("[tty] echo '{}' (0x{:02x})", inner.last_char as char, inner.last_char);
                    print!("{}", inner.last_char as char);
                }
            }
            inner.last_char = 255;
            result = Ok(1);
        }
        self.read_waiters.lock().wake_at_most(1);
        result
    }

    fn write_at(
        &self,
        _offset: usize,
        _len: usize,
        buf: &[u8],
        _data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        let _inner = self.inner.lock();
        match core::str::from_utf8(buf) {
            Ok(content) => print!("{}", content),
            Err(_) => warn!("[tty_write] Non-UTF8 characters: {:?}", buf),
        }
        Ok(buf.len())
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
            raw_dev: crate::makedev!(0x88, 0),
        })
    }

    fn is_stream(&self) -> bool {
        true
    }

    fn poll(&self, _private_data: &FilePrivateData) -> Result<usize, SyscallErr> {
        let mut inner = self.inner.lock();
        let has_data = if inner.last_char != 255 {
            true
        } else {
            inner.last_char =
                crate::trace::pop_stashed().unwrap_or_else(|| console_getchar() as u8);
            crate::trace::check_magic_key(inner.last_char, "tty:poll");
            if vintr_send_sigint(&inner, inner.last_char) {
                inner.last_char = 255;
                false
            } else {
                inner.last_char != 255
            }
        };
        drop(inner);
        let mut revents: usize = 0;
        if has_data {
            revents |= EPollEvent::EPOLLIN.bits();
            self.read_waiters.lock().wake_at_most(1);
        }
        revents |= EPollEvent::EPOLLOUT.bits();
        Ok(revents)
    }

    fn ioctl(
        &self,
        cmd: u32,
        argp: usize,
        _private_data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        info!(
            "[tty_ioctl] cmd: {:?}, arg: {:X}",
            TeletypeCommand::from_primitive(cmd),
            argp
        );
        let mut inner = self.inner.lock();
        let token = crate::task::current_user_token();
        match TeletypeCommand::from_primitive(cmd) {
            TeletypeCommand::TCGETS | TeletypeCommand::TCGETA => {
                match UserPtrMut::from_addr(argp).write(token, &inner.termios) {
                    Ok(()) => Ok(0),
                    Err(_) => Err(SyscallErr::EFAULT),
                }
            }
            TeletypeCommand::TCSETS
            | TeletypeCommand::TCSETSW
            | TeletypeCommand::TCSETSF
            | TeletypeCommand::TCSETA
            | TeletypeCommand::TCSETAW
            | TeletypeCommand::TCSETAF => {
                match UserPtr::from_addr(argp).read(token) {
                    Ok(termios) => {
                        inner.termios = termios;
                        Ok(0)
                    }
                    Err(_) => Err(SyscallErr::EFAULT),
                }
            }
            // TCXONC (0x540A) — software flow control. No-op for virtual terminal.
            TeletypeCommand::TCXONC => Ok(0),
            TeletypeCommand::TIOCGPGRP => {
                match UserPtrMut::from_addr(argp).write(token, &inner.foreground_pgid) {
                    Ok(()) => Ok(0),
                    Err(_) => Err(SyscallErr::EFAULT),
                }
            }
            TeletypeCommand::TIOCSPGRP => match UserPtr::<u32>::from_addr(argp).read(token) {
                Ok(word) => {
                    log::info!("[tty-ioctl] TIOCSPGRP: set foreground_pgid to {}", word);
                    inner.foreground_pgid = word;
                    Ok(0)
                }
                Err(_errno) => Err(SyscallErr::EFAULT),
            },
            TeletypeCommand::TIOCGWINSZ => {
                match UserPtrMut::from_addr(argp).write(token, &inner.winsize) {
                    Ok(()) => Ok(0),
                    Err(_) => Err(SyscallErr::EFAULT),
                }
            }
            TeletypeCommand::TIOCSWINSZ => {
                match UserPtr::from_addr(argp).read(token) {
                    Ok(winsize) => {
                        inner.winsize = winsize;
                        Ok(0)
                    }
                    Err(_) => Err(SyscallErr::EFAULT),
                }
            }
            _ => {
                warn!(
                    "[tty_ioctl] unsupported ioctl cmd: {:?} ({:#X})",
                    TeletypeCommand::from_primitive(cmd),
                    cmd
                );
                Err(SyscallErr::ENOTTY)
            }
        }
    }

    fn read_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(&self.read_waiters)
    }

    fn fs(&self) -> Arc<dyn NewFileSystem> {
        DEV_FS.clone()
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

#[allow(non_camel_case_types)]
#[derive(Debug, Eq, PartialEq, FromPrimitive)]
#[repr(u32)]
pub enum TeletypeCommand {
    // For struct termios
    /// Gets the current serial port settings.
    TCGETS = 0x5401,
    /// Sets the serial port settings immediately.
    TCSETS = 0x5402,
    /// Sets the serial port settings after allowing the input and output buffers to drain/empty.
    TCSETSW = 0x5403,
    /// Sets the serial port settings after flushing the input and output buffers.
    TCSETSF = 0x5404,

    /// For struct termio
    /// Gets the current serial port settings.
    TCGETA = 0x5405,
    /// Sets the serial port settings immediately.
    TCSETA = 0x5406,
    /// Sets the serial port settings after allowing the input and output buffers to drain/empty.
    TCSETAW = 0x5407,
    /// Sets the serial port settings after flushing the input and output buffers.
    TCSETAF = 0x5408,

    /// Software flow control (tcflow).
    TCXONC = 0x540A,

    /// Get the process group ID of the foreground process group on this terminal.
    TIOCGPGRP = 0x540F,
    /// Set the foreground process group ID of this terminal.
    TIOCSPGRP = 0x5410,

    /// Get window size.
    TIOCGWINSZ = 0x5413,
    /// Set window size.
    TIOCSWINSZ = 0x5414,

    /// Non-cloexec
    FIONCLEX = 0x5450,
    /// Cloexec
    FIOCLEX = 0x5451,

    /// rustc using pipe and ioctl pipe file with this request id
    /// for non-blocking/blocking IO control setting
    FIONBIO = 0x5421,

    /// Read time
    RTC_RD_TIME = 0x80247009,

    #[num_enum(default)]
    ILLEAGAL,
}

#[repr(C)]
#[derive(Clone, Copy)]
/// The termios functions describe a general terminal interface that
/// is provided to control asynchronous communications ports.
pub struct Termios {
    /// input modes
    pub iflag: u32,
    /// ouput modes
    pub oflag: u32,
    /// control modes
    pub cflag: u32,
    /// local modes
    pub lflag: u32,
    pub line: u8,
    /// terminal special characters.
    pub cc: [u8; 19],
}

impl Default for Termios {
    fn default() -> Self {
        Termios {
            // IMAXBEL | IUTF8 | IXON | IXANY | ICRNL | BRKINT
            iflag: 0o66402,
            // OPOST | ONLCR
            oflag: 0o5,
            // HUPCL | CREAD | CSIZE | EXTB
            cflag: 0o2277,
            // IEXTEN | ECHOTCL | ECHOKE ECHO | ECHOE | ECHOK | ISIG | ICANON
            lflag: 0o105073,
            line: 0,
            cc: [
                3,   // VINTR Ctrl-C
                28,  // VQUIT
                127, // VERASE
                21,  // VKILL
                4,   // VEOF Ctrl-D
                0,   // VTIME
                1,   // VMIN
                0,   // VSWTC
                17,  // VSTART
                19,  // VSTOP
                26,  // VSUSP Ctrl-Z
                255, // VEOL
                18,  // VREPAINT
                15,  // VDISCARD
                23,  // VWERASE
                22,  // VLNEXT
                255, // VEOL2
                0, 0,
            ],
        }
    }
}

bitflags! {
    pub struct LocalModes : u32 {
        const ISIG = 0o000001;
        const ICANON = 0o000002;
        const ECHO = 0o000010;
        const ECHOE = 0o000020;
        const ECHOK = 0o000040;
        const ECHONL = 0o000100;
        const NOFLSH = 0o000200;
        const TOSTOP = 0o000400;
        const IEXTEN = 0o100000;
        const XCASE = 0o000004;
        const ECHOCTL = 0o001000;
        const ECHOPRT = 0o002000;
        const ECHOKE = 0o004000;
        const FLUSHO = 0o010000;
        const PENDIN = 0o040000;
        const EXTPROC = 0o200000;
    }
}
