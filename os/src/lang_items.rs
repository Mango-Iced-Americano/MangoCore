#[cfg(target_arch = "riscv64")]
use core::fmt::Write;
use crate::hal::shutdown;
use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // syscall 受控窗口内可能开着本地中断进入 panic。必须在
    // console 输出、锁诊断和跨核 STOP 之前立即关闭，避免递归 trap。
    let _ = crate::hal::local_irq_save();
    match info.location() {
        Some(location) => {
            println!(
                "[kernel] panicked at '{}', {}:{}:{}",
                info.message(),
                location.file(),
                location.line(),
                location.column()
            );
        }
        None => println!("[kernel] panicked at '{}'", info.message()),
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
