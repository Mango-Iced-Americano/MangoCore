//! `ERA`：普通例外返回地址寄存器。
//!
//! 当普通例外发生时，硬件在这里保存 faulting PC，异常返回路径据此恢复执行流。

impl_define_csr!(ERA, "Exception Return Address (ERA)\n\
                       Record the resulting PC in case of exceptions other than TLB Refill and Machine Error.");
impl_write_csr!(0x6, ERA);
impl_read_csr!(0x6, ERA);

impl ERA {
    /// 将返回地址推进到下一条固定长度指令。
    pub fn next_ins(&mut self) -> &mut Self {
        self.bits += 4;
        self
    }
    /// 设置普通例外返回 PC。
    pub fn set_pc(&mut self, pc: usize) -> &mut Self {
        self.bits = pc;
        self
    }
    /// 读取普通例外返回 PC。
    pub fn get_pc(&self) -> usize {
        self.bits
    }
}
