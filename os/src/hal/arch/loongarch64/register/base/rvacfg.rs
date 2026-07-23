//! `RVACFG`：缩减虚拟地址配置寄存器。
//!
//! 该 CSR 描述当前实现裁剪虚拟地址位宽的方式，影响高位地址合法性判断。

use bit_field::BitField;
use core::fmt::Debug;

impl_define_csr!(RVACfg, "Reduced Virtual Address Configuration\n\
                          This register is used to control the length of the address being reduced in the virtual address reduction mode.");
impl_write_csr!(0x1f, RVACfg);
impl_read_csr!(0x1f, RVACfg);
impl Debug for RVACfg {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RVACfg")
            .field("rbits", &self.get_rbits())
            .finish()
    }
}
impl RVACfg {
    /// The number of the high order bits of the address to be reduced in the virtual address reduction mode.
    /// It can be configured to a value between 0 and 8.
    /// Specially, 0 means that the virtual address reduction mode is disabled.
    /// The processor behavior with `rbits` over 8 is undefined.
    pub fn get_rbits(&self) -> usize {
        self.bits.get_bits(0..=3)
    }
    /// The number of the high order bits of the address to be reduced in the virtual address reduction mode.
    /// It can be configured to a value between 0 and 8.
    /// Specially, 0 means that the virtual address reduction mode is disabled.
    /// # Warning!
    /// The processor behavior with `rbits` over 8 is UNDEFINED.
    pub fn set_rbits(&mut self, val: usize) -> &mut Self {
        assert!(val <= 8, "RVACFG.RBits must be in 0..=8");
        // RBits 只占第 3:0 位。必须保留 CSR 的其他字段，不能用缩减位数覆盖整个寄存器。
        self.bits.set_bits(0..=3, val);
        self
    }
}
