use crate::hal::{console_write_bytes, local_irq_restore, local_irq_save, panic_console_write};
use crate::task::current_task;
use crate::timer::get_time_ms;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};
use log::{self, Level, LevelFilter, Log, Metadata, Record};
use spin::Mutex;

// 关本地中断只防止同 CPU 重入；全局锁负责跨 CPU 串行化完整的一次输出。
// panic 会永久切换到 raw 路径，因此即使崩溃点正持有此锁也不会自死锁。
static OUTPUT_LOCK: Mutex<()> = Mutex::new(());
static PANICKING: AtomicBool = AtomicBool::new(false);

struct KernelOutput {
    raw: bool,
}

impl Write for KernelOutput {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.raw {
            panic_console_write(s.as_bytes());
        } else {
            console_write_bytes(s.as_bytes());
        }
        Ok(())
    }
}

/// 执行一次完整 console 输出；闭包参数表示是否已经进入无锁 panic 路径。
/// 正常闭包运行在 console 叶子锁内，只能调用 HAL writer，不能再获取业务锁。
fn with_output(f: impl FnOnce(bool)) {
    if PANICKING.load(Ordering::Acquire) {
        f(true);
        return;
    }

    let irq_state = local_irq_save();
    let guard = loop {
        if let Some(guard) = OUTPUT_LOCK.try_lock() {
            break guard;
        }
        // 其它 CPU 可能在我们等待期间 panic。此时不能继续等一个可能永不释放的锁。
        if PANICKING.load(Ordering::Acquire) {
            local_irq_restore(irq_state);
            f(true);
            return;
        }
        core::hint::spin_loop();
    };
    f(false);
    drop(guard);
    local_irq_restore(irq_state);
}

/// 进入不可逆的 panic 输出模式，后续打印不再等待任何内核 console 锁。
pub fn enter_panic() {
    PANICKING.store(true, Ordering::Release);
}

/// 正常模式下以 irq-save 全局临界区原子写入原始字节。
///
/// 与 [`print!`]/[`println!`] 不同，本接口接收已经准备好的字节切片，供
/// [`Teletype::write_at`] 绕开逐字符 SBI 开销；panic 模式则直接使用无锁后端。
pub fn write_bytes_atomic(data: &[u8]) {
    with_output(|raw| {
        if raw {
            panic_console_write(data);
        } else {
            console_write_bytes(data);
        }
    });
}

pub fn print(args: fmt::Arguments) {
    with_output(|raw| KernelOutput { raw }.write_fmt(args).unwrap());
}

#[macro_export]
macro_rules! print {
    ($fmt: literal $(, $($arg: tt)+)?) => {
        $crate::console::print(format_args!($fmt $(, $($arg)+)?))
    }
}

#[macro_export]
macro_rules! println {
    ($fmt: literal $(, $($arg: tt)+)?) => {
        $crate::console::print(format_args!(concat!($fmt, crate::newline!()) $(, $($arg)+)?))
    }
}

/// Early-boot diagnostics stay enabled on emulators and can be explicitly
/// restored on 2K1000LA. Production board images omit them entirely.
#[macro_export]
macro_rules! boot_trace {
    ($fmt: literal $(, $($arg: tt)+)?) => {{
        #[cfg(any(
            not(feature = "board_2k1000"),
            feature = "board_bringup_trace"
        ))]
        $crate::println!($fmt $(, $($arg)+)?);
    }}
}

pub fn log_init() {
    static LOGGER: Logger = Logger;
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(match option_env!("LOG") {
        Some("error") => LevelFilter::Error,
        Some("warn") => LevelFilter::Warn,
        Some("info") => LevelFilter::Info,
        Some("debug") => LevelFilter::Debug,
        Some("trace") => LevelFilter::Trace,
        _ => LevelFilter::Off,
    });
    boot_trace!("[kernel] logger inited, level= {:?}", log::max_level());
}

struct Logger;
impl Log for Logger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let ms = get_time_ms();
        let sec = ms / 1000;
        let msec = ms % 1000;

        let color = level_to_color_code(record.level());
        match current_task() {
            Some(task) => println!(
                "\x1b[{}m[{}.{:03}] tid {} pid {}: {}\x1b[0m",
                color,
                sec,
                msec,
                task.gettid(),
                task.pid(),
                record.args()
            ),
            None => println!(
                "\x1b[{}m[{}.{:03}] kernel: {}\x1b[0m",
                color,
                sec,
                msec,
                record.args()
            ),
        }
    }

    fn flush(&self) {}
}

fn level_to_color_code(level: Level) -> u8 {
    match level {
        Level::Error => 31, // Red
        Level::Warn => 93,  // BrightYellow
        Level::Info => 34,  // Blue
        Level::Debug => 32, // Green
        Level::Trace => 90, // BrightBlack
    }
}
