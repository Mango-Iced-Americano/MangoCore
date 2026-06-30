//! LoongArch64 CSR wrapper 的公共生成宏。
//!
//! 宏生成固定 CSR 编号的 `read`/`write` 方法和基础 bitfield 包装类型。
//! 所有实际硬件访问都集中在这里，调用方文件只描述寄存器字段语义。

macro_rules! impl_read_csr {
    ($csr_number:literal,$csr_ident:ident) => {
        impl $csr_ident {
            /// 读取当前处理器核心上的 CSR 值。
            #[inline(always)]
            pub fn read() -> $csr_ident {
                $csr_ident {
                    // Safety: `csrrd` 只把常量 CSR 编号对应的寄存器值读入通用寄存器，
                    // 不访问内存也不修改栈。CSR 编号由宏调用点以字面量给出。
                    bits: unsafe {
                        let bits:usize;
                        core::arch::asm!("csrrd {},{}", out(reg) bits, const $csr_number);
                        bits
                    },
                }
            }
        }
    };
}

macro_rules! impl_write_csr {
    ($csr_number:literal,$csr_ident:ident) => {
        impl $csr_ident {
            /// 将 wrapper 中的原始位写回当前处理器核心上的 CSR。
            #[inline(always)]
            pub fn write(self) {
                // Safety: `csrwr` 只把 `self.bits` 写入常量 CSR 编号对应的寄存器。
                // 调用方通过选择具体 CSR wrapper 负责保证该写入在当前上下文合法。
                unsafe {
                    core::arch::asm!("csrwr {},{}", in(reg) self.bits, const $csr_number);
                }
            }
        }
    };
}
macro_rules! impl_define_csr {
    ($csr_ident:ident,$doc:expr) => {
        #[doc = $doc]
        #[derive(Copy, Clone)]
        pub struct $csr_ident {
            bits: usize,
        }
        impl $csr_ident {
            /// 构造全 0 的 CSR wrapper 值。
            pub fn empty() -> Self {
                Self { bits: 0 }
            }
            /// 从原始寄存器位构造 CSR wrapper 值。
            pub fn from(bits: usize) -> Self {
                Self { bits }
            }
        }
        impl bit_field::BitField for $csr_ident {
            const BIT_LENGTH: usize = usize::BIT_LENGTH;

            fn get_bit(&self, bit: usize) -> bool {
                self.bits.get_bit(bit)
            }

            fn get_bits<T: core::ops::RangeBounds<usize>>(&self, range: T) -> Self {
                Self {
                    bits: self.bits.get_bits(range),
                }
            }

            fn set_bit(&mut self, bit: usize, value: bool) -> &mut Self {
                self.bits.set_bit(bit, value);
                self
            }

            fn set_bits<T: core::ops::RangeBounds<usize>>(
                &mut self,
                range: T,
                value: Self,
            ) -> &mut Self {
                self.bits.set_bits(range, value.bits);
                self
            }
        }
    };
}
