/*  In this file, we ported codes from RustSBI.
    Thus we can handle serial in S mode.
*/
use core::convert::Infallible;
use core::ptr::{read_volatile, write_volatile};
use embedded_hal::serial::nb::{Read, Write};

pub const LSR_OVERRUN: u8 = 1 << 1;
pub const LSR_PARITY: u8 = 1 << 2;
pub const LSR_FRAMING: u8 = 1 << 3;
pub const LSR_BREAK: u8 = 1 << 4;

#[derive(Clone, Copy)]
pub struct Ns16550a {
    pub base: usize,
    size: usize,
    register_shift: usize,
    register_io_width: usize,
}

impl Ns16550a {
    pub const fn new(base: usize, size: usize, register_shift: usize, register_io_width: usize) -> Self {
        Self {
            base,
            size,
            register_shift,
            register_io_width,
        }
    }

    fn register_address(&self, register: usize) -> Option<usize> {
        let offset = register.checked_shl(self.register_shift as u32)?;
        let end = offset.checked_add(self.register_io_width)?;
        (end <= self.size).then_some(self.base.checked_add(offset)?)
    }

    fn read_register(&self, register: usize) -> Option<u8> {
        let address = self.register_address(register)?;
        match self.register_io_width {
            1 => {
                // SAFETY: FDT validation bounds the selected register within the
                // identity-mapped UART range; byte-wide UARTs permit u8 MMIO.
                Some(unsafe { read_volatile(address as *const u8) })
            }
            4 => {
                // SAFETY: reg-shift=2 yields a four-byte-aligned register address
                // within the validated DW APB UART range; only the low byte is used.
                Some(unsafe { read_volatile(address as *const u32) as u8 })
            }
            _ => None,
        }
    }

    fn write_register(&self, register: usize, value: u8) -> bool {
        let Some(address) = self.register_address(register) else {
            return false;
        };
        match self.register_io_width {
            1 => {
                // SAFETY: FDT validation bounds the selected register within the
                // identity-mapped UART range; byte-wide UARTs permit u8 MMIO.
                unsafe { write_volatile(address as *mut u8, value) };
                true
            }
            4 => {
                // SAFETY: reg-shift=2 yields a four-byte-aligned register address
                // within the validated DW APB UART range; writes use its u32 MMIO ABI.
                unsafe { write_volatile(address as *mut u32, value as u32) };
                true
            }
            _ => false,
        }
    }

    pub fn read_line_status(&self) -> Option<u8> {
        self.read_register(offsets::LSR)
    }

    pub fn read_interrupt_identification(&self) -> Option<u8> {
        self.read_register(offsets::IIR)
    }

    pub fn read_rx(&self) -> Option<(u8, u8)> {
        let status = self.read_line_status()?;
        if status & masks::DR == 0 {
            return None;
        }
        self.read_register(offsets::RBR).map(|byte| (byte, status))
    }

    pub fn drain_rx(&self, limit: usize, mut receive: impl FnMut(u8, u8) -> bool) -> usize {
        let mut drained = 0;
        while drained < limit {
            let Some((byte, status)) = self.read_rx() else {
                break;
            };
            drained += 1;
            if !receive(byte, status) {
                break;
            }
        }
        drained
    }

    pub fn enable_receive_interrupts(&self) -> bool {
        self.write_register(offsets::FCR, masks::FIFO_ENABLE)
            && self.write_register(offsets::IER, masks::RDA_INTERRUPT | masks::RLS_INTERRUPT)
    }

    pub fn disable_receive_interrupts(&self) -> bool {
        self.write_register(offsets::IER, 0)
    }

    pub fn try_write(&self, word: u8) -> bool {
        self.read_line_status()
            .is_some_and(|status| status & masks::THRE != 0)
            && self.write_register(offsets::THR, word)
    }
}

impl embedded_hal::serial::ErrorType for Ns16550a {
    type Error = Infallible;
}

impl Read<u8> for Ns16550a {
    fn read(&mut self) -> nb::Result<u8, Self::Error> {
        self.read_rx()
            .map(|(word, _)| word)
            .ok_or(nb::Error::WouldBlock)
    }
}

impl Write<u8> for Ns16550a {
    fn write(&mut self, word: u8) -> nb::Result<(), Self::Error> {
        self.try_write(word).then_some(()).ok_or(nb::Error::WouldBlock)
    }

    fn flush(&mut self) -> nb::Result<(), Self::Error> {
        self.read_line_status()
            .is_some_and(|status| status & masks::THRE != 0)
            .then_some(())
            .ok_or(nb::Error::WouldBlock)
    }
}

#[allow(unused)]
mod offsets {
    pub const RBR: usize = 0x0;
    pub const THR: usize = 0x0;

    pub const IER: usize = 0x1;
    pub const IIR: usize = 0x2;
    pub const FCR: usize = 0x2;
    pub const LCR: usize = 0x3;
    pub const MCR: usize = 0x4;
    pub const LSR: usize = 0x5;

    pub const DLL: usize = 0x0;
    pub const DLH: usize = 0x1;
}

mod masks {
    pub const THRE: u8 = 1 << 5;
    pub const DR: u8 = 1;
    pub const OE: u8 = 1 << 1;
    pub const PE: u8 = 1 << 2;
    pub const FE: u8 = 1 << 3;
    pub const BI: u8 = 1 << 4;
    pub const RDA_INTERRUPT: u8 = 1;
    pub const RLS_INTERRUPT: u8 = 1 << 2;
    pub const FIFO_ENABLE: u8 = 1;
}
