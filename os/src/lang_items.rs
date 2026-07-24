#[cfg(target_arch = "riscv64")]
use core::fmt::Write;
use crate::hal::shutdown;
use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    match (info.location(), info.message()) {
        (Some(location), Some(message)) => {
            println!("[kernel] panicked at {}: {}:{}:{}", message, location.file(), location.line(), location.column());
        }
        (Some(location), None) => {
            println!("[kernel] panicked at {}:{}:{}", location.file(), location.line(), location.column());
        }
        (None, Some(message)) => println!("[kernel] panicked: {}", message),
        (None, None) => println!("[kernel] panicked"),
    }
    crate::panic_diag::dump_panic_context();
    shutdown()
}

#[macro_export]
macro_rules! color_text {
    ($text:expr, $color:expr) => {{
        format_args!("\x1b[{}m{}\x1b[0m", $color, $text)
    }};
}

pub trait Bytes<T> {
    fn as_bytes(&self) -> &[u8] {
        let size = core::mem::size_of::<T>();
        unsafe {
            core::slice::from_raw_parts(self as *const _ as *const T as usize as *const u8, size)
        }
    }

    fn as_bytes_mut(&mut self) -> &mut [u8] {
        let size = core::mem::size_of::<T>();
        unsafe {
            core::slice::from_raw_parts_mut(self as *mut _ as *mut T as usize as *mut u8, size)
        }
    }
}
