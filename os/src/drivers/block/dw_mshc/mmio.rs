use crate::timer;
use core::convert::TryFrom;

use super::jh7110::Jh7110MshcConfig;
use super::sd::{command_word, SdCardInfo};
use super::DwMshcError;

mod dma;
mod transfer;

pub(crate) use transfer::{transfer_command, transfer_needs_stop};

const CTRL: usize = 0x00;
const PWREN: usize = 0x04;
const CLKDIV: usize = 0x08;
const CLKSRC: usize = 0x0c;
const CLKENA: usize = 0x10;
const TMOUT: usize = 0x14;
const CTYPE: usize = 0x18;
const BLKSIZ: usize = 0x1c;
const BYTCNT: usize = 0x20;
const INTMASK: usize = 0x24;
const CMDARG: usize = 0x28;
const CMD: usize = 0x2c;
const RESP0: usize = 0x30;
const MINTSTS: usize = 0x40;
const RINTSTS: usize = 0x44;
const STATUS: usize = 0x48;
const FIFOTH: usize = 0x4c;
const VERID: usize = 0x6c;
const BMOD: usize = 0x80;
const PLDMND: usize = 0x84;
const DBADDR: usize = 0x88;
const IDSTS: usize = 0x8c;
const IDINTEN: usize = 0x90;
const DATA_LEGACY: usize = 0x100;
const DATA_NEW: usize = 0x200;
const BMOD_RESET: u32 = 1;
const CTRL_RESET: u32 = 0x7;
const CTRL_INT_ENABLE: u32 = 1 << 4;
const CMD_RESP_EXP: u32 = 1 << 6;
const CMD_RESP_LONG: u32 = 1 << 7;
const CMD_RESP_CRC: u32 = 1 << 8;
const CMD_DATA_EXP: u32 = 1 << 9;
const CMD_DAT_WR: u32 = 1 << 10;
const CMD_STOP: u32 = 1 << 14;
const CMD_INIT: u32 = 1 << 15;
const CMD_UPDATE_CLOCK: u32 = 1 << 21;
const CMD_PRV_DAT_WAIT: u32 = 1 << 13;
const CMD_USE_HOLD_REG: u32 = 1 << 29;
const CMD_START: u32 = 1 << 31;
const CLKENA_ENABLE_LOW_POWER: u32 = 0x10001;
const STATUS_BUSY: u32 = 1 << 9;
const INT_CD: u32 = 1 << 0;
const INT_RESP_ERR: u32 = 1 << 1;
const INT_CMD_DONE: u32 = 1 << 2;
const INT_DATA_OVER: u32 = 1 << 3;
const INT_RXDR: u32 = 1 << 5;
const INT_TXDR: u32 = 1 << 4;
const INT_RCRC: u32 = 1 << 6;
const INT_DCRC: u32 = 1 << 7;
const INT_RTO: u32 = 1 << 8;
const INT_DRTO: u32 = 1 << 9;
const INT_HTO: u32 = 1 << 10;
const INT_FRUN: u32 = 1 << 11;
const INT_HLE: u32 = 1 << 12;
const INT_SBE: u32 = 1 << 13;
const INT_EBE: u32 = 1 << 15;
const ALL_INTERRUPTS: u32 = 0x1ffff;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Response { None, R1, R1b, R2, R3, R6, R7 }

impl Response {
    pub(crate) const fn command_bits(self) -> u32 {
        match self {
            Self::None => 0,
            Self::R1 | Self::R1b | Self::R6 | Self::R7 => CMD_RESP_EXP | CMD_RESP_CRC,
            Self::R2 => CMD_RESP_EXP | CMD_RESP_LONG | CMD_RESP_CRC,
            Self::R3 => CMD_RESP_EXP,
        }
    }
}

pub(crate) struct DwMshcHost {
    base: usize,
    data_offset: usize,
    fifo_depth: u32,
    input_clock_hz: u32,
    dma: Option<dma::DmaResources>,
}

impl DwMshcHost {
    pub(crate) fn new(config: Jh7110MshcConfig) -> Self {
        Self {
            base: config.base,
            data_offset: DATA_LEGACY,
            fifo_depth: config.fifo_depth,
            input_clock_hz: config.input_clock_hz,
            dma: None,
        }
    }

    pub(crate) fn initialize(&mut self) -> Result<(), DwMshcError> {
        let version = self.read(VERID) & 0xffff;
        if version == 0 || version == 0xffff { return Err(DwMshcError::UnsupportedController); }
        self.data_offset = if version < 0x240a { DATA_LEGACY } else { DATA_NEW };
        self.write(PWREN, 1);
        delay_ms(1);
        self.write(CTRL, CTRL_RESET);
        self.wait_clear(CTRL, CTRL_RESET, 500, DwMshcError::CoreResetTimeout)?;
        self.write(INTMASK, 0);
        self.write(CTRL, self.read(CTRL) & !CTRL_INT_ENABLE);
        self.write(RINTSTS, ALL_INTERRUPTS);
        self.write(TMOUT, u32::MAX);
        self.write(CTYPE, 0);
        self.write(BMOD, BMOD_RESET);
        self.write(IDINTEN, 0);
        let fifo_half = self.fifo_depth / 2;
        self.write(FIFOTH, (0x2 << 28) | ((fifo_half - 1) << 16) | fifo_half);
        self.set_card_clock(63)?;
        self.dma = dma::DmaResources::new();
        self.initialize_idmac()
    }

    pub(crate) fn set_bus_width_4bit(&mut self) { self.write(CTYPE, 1); }

    pub(crate) fn set_card_clock(&mut self, divider: u32) -> Result<(), DwMshcError> {
        self.write(CLKENA, 0);
        self.write(CLKSRC, 0);
        self.update_clock()?;
        self.write(CLKDIV, divider);
        self.update_clock()?;
        self.write(CLKENA, CLKENA_ENABLE_LOW_POWER);
        self.update_clock()
    }

    pub(crate) fn command(&mut self, index: u8, argument: u32, response: Response, init: bool, busy: bool) -> Result<[u32; 4], DwMshcError> {
        if busy { self.wait_idle(500)?; }
        self.write(RINTSTS, ALL_INTERRUPTS);
        self.write(CMDARG, argument);
        io_fence();
        let flags = command_word(index, response, false, init)
            | CMD_USE_HOLD_REG
            | if index == 12 { CMD_STOP } else { CMD_PRV_DAT_WAIT };
        self.write(CMD, CMD_START | flags);
        self.wait_clear(CMD, CMD_START, 500, DwMshcError::CommandTimeout(index))?;
        let status = self.wait_command_status(index)?;
        let response_words = match response {
            Response::R2 => [self.read(RESP0 + 12), self.read(RESP0 + 8), self.read(RESP0 + 4), self.read(RESP0)],
            _ => [self.read(RESP0), 0, 0, 0],
        };
        self.write(RINTSTS, status);
        if matches!(response, Response::R1 | Response::R1b) { check_card_status(index, response_words[0])?; }
        Ok(response_words)
    }

    fn update_clock(&mut self) -> Result<(), DwMshcError> {
        self.write(RINTSTS, ALL_INTERRUPTS);
        self.write(CMD, CMD_START | CMD_UPDATE_CLOCK | CMD_PRV_DAT_WAIT);
        self.wait_clear(CMD, CMD_START, 500, DwMshcError::CommandTimeout(0))?;
        let status = self.read(RINTSTS);
        self.write(RINTSTS, status);
        if status & INT_HLE != 0 { Err(DwMshcError::HardwareLocked) } else { Ok(()) }
    }

    pub(crate) fn dump_registers(&self) {
        crate::println!(
            "[dw_mshc] registers: CTRL={:#010x} CLKENA={:#010x} STATUS={:#010x} RINTSTS={:#010x}",
            self.read(CTRL), self.read(CLKENA), self.read(STATUS), self.read(RINTSTS),
        );
        crate::println!(
            "[dw_mshc] registers: CLKDIV={:#010x} CLKSRC={:#010x} FIFOTH={:#010x} VERID={:#010x}",
            self.read(CLKDIV), self.read(CLKSRC), self.read(FIFOTH), self.read(VERID),
        );
    }

    fn recover_data_path(&mut self) { self.stop_idmac(); let _ = self.command(12, 0, Response::R1b, false, true); self.write(CTRL, self.read(CTRL) | (1 << 1)); self.write(RINTSTS, ALL_INTERRUPTS); }
    fn wait_idle(&self, timeout_ms: usize) -> Result<(), DwMshcError> { self.wait_clear(STATUS, STATUS_BUSY, timeout_ms, DwMshcError::DataTimeout) }
    fn wait_clear(&self, register: usize, mask: u32, timeout_ms: usize, error: DwMshcError) -> Result<(), DwMshcError> { let deadline = timer::get_time_ms().saturating_add(timeout_ms); while self.read(register) & mask != 0 { if timer::get_time_ms() >= deadline { return Err(error); } core::hint::spin_loop(); } Ok(()) }
    fn wait_command_status(&self, index: u8) -> Result<u32, DwMshcError> { let deadline = timer::get_time_ms().saturating_add(500); loop { let status = self.read(RINTSTS); if status & INT_RTO != 0 { return Err(DwMshcError::CommandTimeout(index)); } if status & (INT_RCRC | INT_RESP_ERR) != 0 { return Err(DwMshcError::ResponseCrc(index)); } if status & INT_HLE != 0 { return Err(DwMshcError::HardwareLocked); } if status & INT_CMD_DONE != 0 { return Ok(status); } if timer::get_time_ms() >= deadline { return Err(DwMshcError::CommandTimeout(index)); } core::hint::spin_loop(); } }
    #[inline(always)] fn read(&self, offset: usize) -> u32 { // SAFETY: Categories 6 and 11. Discovery validates the aligned controller MMIO range before construction.
        unsafe { core::ptr::read_volatile((self.base + offset) as *const u32) }
    }
    #[inline(always)] fn write(&self, offset: usize, value: u32) { // SAFETY: Categories 6 and 11. This private API uses only aligned DesignWare register offsets.
        unsafe { core::ptr::write_volatile((self.base + offset) as *mut u32, value) }
    }
}

pub(super) const fn card_clock_divider(input_clock_hz: u32, target_hz: u32) -> Option<u32> {
    if target_hz == 0 { return None; }
    match input_clock_hz.checked_div(target_hz) {
        Some(ratio) => match ratio.checked_add(1) { Some(rounded) => Some(rounded / 2), None => None },
        None => None,
    }
}
pub(crate) const fn idmac_control(index: usize, descriptors: usize) -> u32 {
    let mut control = (1 << 31) | (1 << 1) | (1 << 4);
    if index == 0 {
        control |= 1 << 3;
    }
    if index + 1 == descriptors {
        control &= !((1 << 1) | (1 << 4));
        control |= 1 << 2;
    }
    control
}
fn transfer_parameters(card: &SdCardInfo, sector: u64, bytes: usize) -> Result<(u32, usize), DwMshcError> { if bytes == 0 || bytes % 512 != 0 { return Err(DwMshcError::ShortTransfer); } let argument = if card.high_capacity { u32::try_from(sector).map_err(|_| DwMshcError::OutOfRange)? } else { sector.checked_mul(512).and_then(|value| u32::try_from(value).ok()).ok_or(DwMshcError::OutOfRange)? }; Ok((argument, bytes / 512)) }
pub(crate) fn data_error(command: u8, status: u32) -> Option<DwMshcError> { if status & INT_RTO != 0 { Some(DwMshcError::CommandTimeout(command)) } else if status & (INT_RCRC | INT_RESP_ERR) != 0 { Some(DwMshcError::ResponseCrc(command)) } else if status & (INT_DRTO | INT_HTO) != 0 { Some(DwMshcError::DataTimeout) } else if status & INT_DCRC != 0 { Some(DwMshcError::DataCrc) } else if status & INT_FRUN != 0 { Some(DwMshcError::FifoRun) } else if status & INT_SBE != 0 { Some(DwMshcError::StartBit) } else if status & INT_EBE != 0 { Some(DwMshcError::EndBit) } else if status & INT_HLE != 0 { Some(DwMshcError::HardwareLocked) } else { None } }
fn retryable(error: DwMshcError) -> bool { matches!(error, DwMshcError::DataCrc | DwMshcError::DataTimeout | DwMshcError::FifoRun) }
fn check_card_status(index: u8, status: u32) -> Result<(), DwMshcError> { if status & (1 << 25) != 0 { return Err(DwMshcError::HardwareLocked); } let errors = (1 << 31) | (1 << 30) | (1 << 29) | (1 << 28) | (1 << 27) | (1 << 26) | (1 << 23) | (1 << 20) | (1 << 19) | (1 << 18) | (1 << 17) | (1 << 16); if status & errors != 0 { Err(DwMshcError::CardStatus(index, status)) } else { Ok(()) } }
fn delay_ms(milliseconds: usize) { let deadline = timer::get_time_ms().saturating_add(milliseconds); while timer::get_time_ms() < deadline { core::hint::spin_loop(); } }
#[inline(always)] fn io_fence() { // SAFETY: RISC-V I/O fence has no memory operands and this module is riscv64-gated.
    unsafe { core::arch::asm!("fence iorw, iorw", options(nostack, preserves_flags)) }
}
