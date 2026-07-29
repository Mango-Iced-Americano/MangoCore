//! StarFive VisionFive 2 (JH7110 SoC) 平台常量。
//!
//! 基于 OpenSBI 的 SBI ecall 接口。UART/PLIC 由 OpenSBI 管理，
//! 内核通过 SBI 进行字符 I/O、定时器设置和关机操作。
//!
//! 参考：JH7110 设备树 timebase-frequency = <4000000> (4 MHz)。

pub const CLOCK_FREQ: usize = 4_000_000;

pub const MMIO: &[(usize, usize)] = &[
    (0x1000_0000, 0x1000_0000), // 外设块 (UART, I2C, SPI, virtio probe, GMAC0 等)
    (0x0C00_0000, 0x0040_0000), // PLIC
    (0x0201_0000, 0x0000_1000), // JH7110 L2 缓存控制器 (FLUSH64 在 +0x0200)
];
