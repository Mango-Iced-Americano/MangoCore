//! `CPUID`：当前处理器核心标识寄存器。
//!
//! 该 wrapper 用于读取 LoongArch64 hart/core id，供启动和调试路径区分核心。

impl_define_csr!(
    CPUId,
    "This register contains the processor core number information."
);
impl_read_csr!(0x20, CPUId);

impl CPUId {
    pub fn get_core_id(&self) -> usize {
        self.bits
    }
}
