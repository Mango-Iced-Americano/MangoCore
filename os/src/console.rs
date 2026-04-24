use crate::hal::{console_flush, console_putchar};
use crate::task::current_task;
use crate::timer::get_time_ms;
use core::fmt::{self, Write};
use log::{self, Level, LevelFilter, Log, Metadata, Record};

// won't require lock, but unlikely to cause problem
struct KernelOutput;

impl Write for KernelOutput {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut i = 0;
        for c in s.chars() {
            console_putchar(c as usize);
            i += 1;
            if i >= 4 {
                console_flush();
                i = 0;
            }
        }
        if i != 0 {
            console_flush();
        }
        Ok(())
    }
}

pub fn print(args: fmt::Arguments) {
    KernelOutput.write_fmt(args).unwrap();
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
    println!("[kernel] logger inited, level= {:?}", log::max_level());
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

        print!("\x1b[{}m", level_to_color_code(record.level()));
        match current_task() {
            Some(task) => println!(
                "[{}.{:03}] pid {}: {}",
                sec,
                msec,
                task.pid.0,
                record.args()
            ),
            None => println!("[{}.{:03}] kernel: {}", sec, msec, record.args()),
        }
        print!("\x1b[0m")
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
