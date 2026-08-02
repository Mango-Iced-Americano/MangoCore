use crate::mm::{UserPtr, UserPtrMut};
use crate::task::signal::Signals;
use crate::task::WaitQueue;

use alloc::sync::Arc;
use core::any::Any;
use lazy_static::lazy_static;
use log::{info, warn};
use num_enum::FromPrimitive;
use spin::Mutex;

use crate::fs::dev::DEV_FS;
use crate::fs::vfs::event::{EPollEvent, EventWaitQueue};
use crate::fs::vfs::file_system::FileSystem as NewFileSystem;
use crate::fs::vfs::{FilePrivateData, FileType, IndexNode, InodeFlags, InodeMode, Metadata};
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
    input: TtyInputBuffer,
    noncanonical_read_active: bool,
    noncanonical_deadline: Option<TimeSpec>,
    foreground_pgid: u32,
    winsize: WinSize,
    termios: Termios,
}

impl Default for TeletypeInner {
    fn default() -> Self {
        Self {
            input: TtyInputBuffer::new(),
            noncanonical_read_active: false,
            noncanonical_deadline: None,
            foreground_pgid: Default::default(),
            winsize: WinSize::default(),
            termios: Termios::default(),
        }
    }
}

pub struct Teletype {
    inner: Mutex<TeletypeInner>,
    read_waiters: EventWaitQueue,
}

impl Default for Teletype {
    fn default() -> Self {
        Self {
            inner: Mutex::new(TeletypeInner::default()),
            read_waiters: EventWaitQueue::new(),
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

const TTY_INPUT_CAPACITY: usize = 1024;
const VDISABLE: u8 = 0xff;
const VINTR: usize = 0;
const VERASE: usize = 2;
const VKILL: usize = 3;
const VEOF: usize = 4;
const VTIME: usize = 5;
const VMIN: usize = 6;
const VEOL: usize = 11;
const VEOL2: usize = 16;

/// Fixed-capacity input queue used by the serial line discipline.
///
/// `canonical_ready` covers complete records at the head of the queue, while
/// `current_line_len` covers the editable record at its tail.  Keeping this
/// buffer allocation-free is important because characters are inserted from
/// the scheduler/console production path.
struct TtyInputBuffer {
    bytes: [u8; TTY_INPUT_CAPACITY],
    head: usize,
    len: usize,
    canonical_ready: usize,
    current_line_len: usize,
    eof_pending: bool,
}

impl TtyInputBuffer {
    const fn new() -> Self {
        Self {
            bytes: [0; TTY_INPUT_CAPACITY],
            head: 0,
            len: 0,
            canonical_ready: 0,
            current_line_len: 0,
            eof_pending: false,
        }
    }

    fn push(&mut self, byte: u8) -> bool {
        if self.len == self.bytes.len() {
            return false;
        }
        let tail = (self.head + self.len) % self.bytes.len();
        self.bytes[tail] = byte;
        self.len += 1;
        true
    }

    fn has_space(&self) -> bool {
        self.len < self.bytes.len()
    }

    fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let byte = self.bytes[self.head];
        self.head = (self.head + 1) % self.bytes.len();
        self.len -= 1;
        Some(byte)
    }

    fn push_pending(&mut self, byte: u8) -> bool {
        if !self.push(byte) {
            return false;
        }
        self.current_line_len += 1;
        true
    }

    fn erase_pending(&mut self) -> bool {
        if self.current_line_len == 0 {
            return false;
        }
        self.len -= 1;
        self.current_line_len -= 1;
        true
    }

    fn kill_pending(&mut self) -> usize {
        let removed = self.current_line_len;
        self.len -= removed;
        self.current_line_len = 0;
        removed
    }

    fn finish_line(&mut self, delimiter: u8) -> bool {
        let delimiter_queued = self.push_pending(delimiter);
        let ready = self.current_line_len;
        self.canonical_ready += ready;
        self.current_line_len = 0;
        delimiter_queued || ready != 0
    }

    fn finish_eof(&mut self) {
        if self.current_line_len == 0 {
            self.eof_pending = true;
        } else {
            self.canonical_ready += self.current_line_len;
            self.current_line_len = 0;
        }
    }

    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
        self.canonical_ready = 0;
        self.current_line_len = 0;
        self.eof_pending = false;
    }

    fn change_canonical_mode(&mut self, was_canonical: bool, is_canonical: bool) {
        if was_canonical == is_canonical {
            return;
        }
        self.eof_pending = false;
        if is_canonical {
            self.canonical_ready = 0;
            self.current_line_len = self.len;
        } else {
            self.canonical_ready = 0;
            self.current_line_len = 0;
        }
    }
}

impl TeletypeInner {
    fn is_canonical(&self) -> bool {
        self.termios.lflag & LocalModes::ICANON.bits() != 0
    }

    fn map_input(&self, byte: u8) -> Option<u8> {
        let modes = InputModes::from_bits_truncate(self.termios.iflag);
        if byte == b'\r' {
            if modes.contains(InputModes::IGNCR) {
                return None;
            }
            if modes.contains(InputModes::ICRNL) {
                return Some(b'\n');
            }
        } else if byte == b'\n' && modes.contains(InputModes::INLCR) {
            return Some(b'\r');
        }
        Some(byte)
    }

    fn is_delimiter(&self, byte: u8) -> bool {
        byte == b'\n'
            || (self.termios.cc[VEOL] != VDISABLE && byte == self.termios.cc[VEOL])
            || (self.termios.cc[VEOL2] != VDISABLE && byte == self.termios.cc[VEOL2])
    }

    fn poll_readable(&self) -> bool {
        if self.is_canonical() {
            self.input.canonical_ready != 0 || self.input.eof_pending
        } else {
            self.input.len != 0
        }
    }

    fn reset_noncanonical_read(&mut self) {
        self.noncanonical_read_active = false;
        self.noncanonical_deadline = None;
    }

    fn read_canonical(&mut self, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        if self.input.canonical_ready == 0 {
            if self.input.eof_pending {
                self.input.eof_pending = false;
                return Ok(0);
            }
            return Err(SyscallErr::EAGAIN);
        }

        let mut count = 0;
        while count < buf.len() && self.input.canonical_ready != 0 {
            let byte = self.input.pop().expect("canonical byte is missing");
            self.input.canonical_ready -= 1;
            buf[count] = byte;
            count += 1;
            if self.is_delimiter(byte) {
                break;
            }
        }
        Ok(count)
    }

    fn read_noncanonical(&mut self, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        let available = self.input.len;
        let vmin = self.termios.cc[VMIN] as usize;
        let vtime = self.termios.cc[VTIME] as usize;
        let threshold = vmin.min(buf.len());

        let should_return = if vmin == 0 {
            available != 0
        } else {
            available >= threshold
        };
        if should_return {
            let count = available.min(buf.len());
            for slot in &mut buf[..count] {
                *slot = self.input.pop().expect("noncanonical byte is missing");
            }
            self.reset_noncanonical_read();
            return Ok(count);
        }

        if vtime == 0 {
            self.reset_noncanonical_read();
            return if vmin == 0 {
                Ok(0)
            } else {
                Err(SyscallErr::EAGAIN)
            };
        }

        let now = TimeSpec::now();
        if !self.noncanonical_read_active {
            self.noncanonical_read_active = true;
            self.noncanonical_deadline = if vmin == 0 || available != 0 {
                Some(now + TimeSpec::from_ms(vtime * 100))
            } else {
                None
            };
        }
        if self
            .noncanonical_deadline
            .map(|deadline| now >= deadline)
            .unwrap_or(false)
        {
            let count = self.input.len.min(buf.len());
            for slot in &mut buf[..count] {
                *slot = self.input.pop().expect("timed byte is missing");
            }
            self.reset_noncanonical_read();
            return Ok(count);
        }
        Err(SyscallErr::EAGAIN)
    }
}

fn is_vintr(inner: &TeletypeInner, ch: u8) -> bool {
    if inner.termios.lflag & LocalModes::ISIG.bits() == 0
        || inner.termios.cc[VINTR] == VDISABLE
        || ch != inner.termios.cc[VINTR]
    {
        return false;
    }
    true
}

/// Send SIGINT after the TTY lock has been released. Falls back to the current
/// process, or to interruptible tasks when called from the scheduler loop.
fn send_vintr_sigint(fg_pgid: u32) -> bool {
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

#[derive(Clone, Copy)]
struct VintrEvent {
    byte: u8,
    foreground_pgid: u32,
    echo_control: bool,
}

impl Teletype {
    /// Drain UART characters previously placed in the trace stash into the
    /// line discipline.  All readiness notifications originate here, after
    /// the character has actually made input readable.
    pub fn receive_stashed() {
        while let Some(byte) = crate::trace::pop_stashed() {
            let _ = Self::receive_char(byte);
        }
    }

    /// Deliver one console transport byte in task context.
    ///
    /// Returns false only when the TTY input ring rejected the byte, so the
    /// UART producer can apply transport-level backpressure.
    pub fn receive_console_char(byte: u8) -> bool {
        Self::receive_char(byte)
    }

    /// Return whether the TTY input ring can accept another transport byte.
    pub fn input_has_space() -> bool {
        TTY.inner.lock().input.has_space()
    }

    fn receive_char(byte: u8) -> bool {
        let (notify_readable, vintr_event, input_overflow) = {
            let mut inner = TTY.inner.lock();
            let Some(byte) = inner.map_input(byte) else {
                return true;
            };
            if is_vintr(&inner, byte) {
                let event = VintrEvent {
                    byte,
                    foreground_pgid: inner.foreground_pgid,
                    echo_control: inner.termios.lflag & LocalModes::ECHOCTL.bits() != 0,
                };
                if inner.termios.lflag & LocalModes::NOFLSH.bits() == 0 {
                    inner.input.clear();
                    inner.reset_noncanonical_read();
                }
                (false, Some(event), false)
            } else {
                let (input_changed, input_overflow) = if inner.is_canonical() {
                    if inner.termios.cc[VERASE] != VDISABLE && byte == inner.termios.cc[VERASE] {
                        if inner.input.erase_pending()
                            && inner.termios.lflag & LocalModes::ECHO.bits() != 0
                        {
                            if inner.termios.lflag & LocalModes::ECHOE.bits() != 0 {
                                print!("\x08 \x08");
                            } else {
                                print!("{}", byte as char);
                            }
                        }
                        (false, false)
                    } else if inner.termios.cc[VKILL] != VDISABLE && byte == inner.termios.cc[VKILL]
                    {
                        let removed = inner.input.kill_pending();
                        if removed != 0 && inner.termios.lflag & LocalModes::ECHO.bits() != 0 {
                            if inner.termios.lflag & LocalModes::ECHOKE.bits() != 0 {
                                for _ in 0..removed {
                                    print!("\x08 \x08");
                                }
                            } else if inner.termios.lflag & LocalModes::ECHOK.bits() != 0 {
                                print!("\n");
                            }
                        }
                        (false, false)
                    } else if inner.termios.cc[VEOF] != VDISABLE && byte == inner.termios.cc[VEOF] {
                        inner.input.finish_eof();
                        (true, false)
                    } else if inner.is_delimiter(byte) {
                        let input_was_full = !inner.input.has_space();
                        let accepted = inner.input.finish_line(byte);
                        if inner.termios.lflag & (LocalModes::ECHO | LocalModes::ECHONL).bits() != 0
                        {
                            if byte == b'\n' {
                                print!("\n");
                            } else {
                                print!("{}", byte as char);
                            }
                        }
                        (accepted, input_was_full)
                    } else {
                        let accepted = inner.input.push_pending(byte);
                        if accepted && inner.termios.lflag & LocalModes::ECHO.bits() != 0 {
                            print!("{}", byte as char);
                        }
                        (false, !accepted)
                    }
                } else {
                    let accepted = inner.input.push(byte);
                    if accepted {
                        if inner.termios.lflag & LocalModes::ECHO.bits() != 0 {
                            if byte == b'\n' {
                                print!("\n");
                            } else {
                                print!("{}", byte as char);
                            }
                        }
                        if inner.noncanonical_read_active && inner.termios.cc[VTIME] != 0 {
                            inner.noncanonical_deadline = Some(
                                TimeSpec::now()
                                    + TimeSpec::from_ms(inner.termios.cc[VTIME] as usize * 100),
                            );
                        }
                    }
                    (accepted, !accepted)
                };
                (input_changed, None, input_overflow)
            }
        };

        if input_overflow {
            #[cfg(target_arch = "riscv64")]
            crate::hal::arch::riscv::sbi::note_tty_input_overrun();
        }

        if let Some(event) = vintr_event {
            let sent = send_vintr_sigint(event.foreground_pgid);
            if event.echo_control {
                print!("^C\n");
            }
            log::info!(
                "[vintr] ch={:#x} VINTR={:#x} ISIG=true fg_pgid={} sigint_sent={}",
                event.byte,
                event.byte,
                event.foreground_pgid,
                sent,
            );
            return true;
        }
        if notify_readable {
            TTY.read_waiters
                .notify_events_all(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM);
        }
        !input_overflow
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
        let mut inner = self.inner.lock();
        if inner.is_canonical() {
            inner.read_canonical(buf)
        } else {
            inner.read_noncanonical(buf)
        }
    }

    fn write_at(
        &self,
        _offset: usize,
        _len: usize,
        buf: &[u8],
        _data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        // Read termios flags under lock, then release before I/O.
        let onlcr = {
            let inner = self.inner.lock();
            // OPOST (bit 0) must be set for any output processing;
            // ONLCR (bit 2) maps NL → CR-NL.
            inner.termios.oflag & 0o5 == 0o5
        };

        if onlcr {
            let mut start = 0;
            for (i, &b) in buf.iter().enumerate() {
                if b == b'\n' {
                    if i > start {
                        crate::console::write_bytes_atomic(&buf[start..i]);
                    }
                    crate::console::write_bytes_atomic(b"\r\n");
                    start = i + 1;
                }
            }
            if start < buf.len() {
                crate::console::write_bytes_atomic(&buf[start..]);
            }
        } else {
            crate::console::write_bytes_atomic(buf);
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
        let has_data = self.inner.lock().poll_readable();
        let mut revents: usize = 0;
        if has_data {
            revents |= (EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM).bits();
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
            | TeletypeCommand::TCSETAF => match UserPtr::from_addr(argp).read(token) {
                Ok(termios) => {
                    let was_canonical = inner.is_canonical();
                    let was_readable = inner.poll_readable();
                    let flush_input = matches!(
                        TeletypeCommand::from_primitive(cmd),
                        TeletypeCommand::TCSETSF | TeletypeCommand::TCSETAF
                    );
                    inner.termios = termios;
                    let is_canonical = inner.is_canonical();
                    if flush_input {
                        inner.input.clear();
                    } else {
                        inner
                            .input
                            .change_canonical_mode(was_canonical, is_canonical);
                    }
                    inner.reset_noncanonical_read();
                    let notify_readable = !was_readable && inner.poll_readable();
                    drop(inner);
                    if notify_readable {
                        self.read_waiters
                            .notify_events_all(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM);
                    }
                    Ok(0)
                }
                Err(_) => Err(SyscallErr::EFAULT),
            },
            // TCXONC (0x540A) — software flow control. No-op for virtual terminal.
            TeletypeCommand::TCXONC => Ok(0),
            TeletypeCommand::TIOCGPGRP => {
                // If foreground_pgid has never been set, return the caller's
                // pgid as the default without modifying foreground_pgid.
                // This matches Linux semantics: tcgetpgrp is a pure read.
                let pgid = inner.foreground_pgid;
                let val = if pgid == 0 {
                    crate::task::current_task()
                        .map(|t| t.process.getpgid() as u32)
                        .unwrap_or(0)
                } else {
                    pgid
                };
                match UserPtrMut::from_addr(argp).write(token, &val) {
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
            TeletypeCommand::TIOCSWINSZ => match UserPtr::from_addr(argp).read(token) {
                Ok(winsize) => {
                    inner.winsize = winsize;
                    Ok(0)
                }
                Err(_) => Err(SyscallErr::EFAULT),
            },
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
        Some(self.read_waiters.wait_queue())
    }

    fn read_event_queue(&self) -> Option<&crate::fs::vfs::event::EventWaitQueue> {
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
    pub struct InputModes : u32 {
        const INLCR = 0o000100;
        const IGNCR = 0o000200;
        const ICRNL = 0o000400;
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
