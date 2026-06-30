//! `EENTRY`：普通例外入口基址寄存器。
//!
//! trap 初始化代码通过该 CSR 指定非 TLB refill、非 machine error 例外的入口地址。

use crate::mm::VirtPageNum;

impl_define_csr!(EEntry, "Exception Entry Base Address CSR\n\
                          This register is used to configure the entry base address for general exceptions and interrupts.");
impl_write_csr!(0xC, EEntry);
impl_read_csr!(0xC, EEntry);

impl EEntry {
    pub fn get_exception_entry(&self) -> VirtPageNum {
        // 12位以后,以页对齐
        VirtPageNum(self.bits)
    }
    pub fn set_exception_entry(&mut self, eentry: usize) -> &mut Self {
        debug_assert_eq!(eentry & 0xfff, 0);
        self.bits = eentry;
        self
    }
}
