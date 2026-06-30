//! LoongArch64 链接所需的 libc 兼容符号。
//!
//! 为裸机内核补齐编译器或库代码可能引用的少量 C ABI 符号。

extern crate rlibc;
use rlibc::memcmp;
#[no_mangle]
pub unsafe extern "C" fn bcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    memcmp(s1, s2, n)
}
#[no_mangle]
pub extern "C" fn _Unwind_Resume() {}

#[lang = "eh_personality"]
extern "C" fn eh_personality() {}
