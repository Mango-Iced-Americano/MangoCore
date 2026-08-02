use core::convert::TryFrom;

use crate::timer;

use super::super::sd::{command_word, SdCardInfo};
use super::super::DwMshcError;
use super::{
    check_card_status, data_error, io_fence, retryable, transfer_parameters, DwMshcHost, Response, ALL_INTERRUPTS, BLKSIZ, BYTCNT,
    CMD, CMDARG, CMD_DAT_WR, CMD_PRV_DAT_WAIT, CMD_START, CMD_USE_HOLD_REG, CTRL, INT_CMD_DONE,
    INT_DATA_OVER, INT_DCRC, INT_DRTO, INT_EBE, INT_FRUN, INT_HLE, INT_HTO, INT_RCRC, INT_RESP_ERR,
    INT_RTO, INT_RXDR, INT_SBE, INT_TXDR, RESP0, RINTSTS, STATUS,
};

const SECTOR_BYTES: usize = 512;
const FIFO_RESET: u32 = 1 << 1;

pub(crate) const fn transfer_command(sectors: usize, write: bool) -> u8 {
    match (sectors > 1, write) {
        (false, false) => 17,
        (true, false) => 18,
        (false, true) => 24,
        (true, true) => 25,
    }
}

pub(crate) const fn transfer_needs_stop(sectors: usize) -> bool {
    sectors > 1
}

impl DwMshcHost {
    pub(crate) fn read_sector(
        &mut self,
        card: &SdCardInfo,
        sector: u64,
        out: &mut [u8],
    ) -> Result<(), DwMshcError> {
        self.read_blocks(card, sector, out)
    }

    pub(crate) fn write_sector(
        &mut self,
        card: &SdCardInfo,
        sector: u64,
        data: &[u8],
    ) -> Result<(), DwMshcError> {
        self.write_blocks(card, sector, data)
    }

    pub(crate) fn read_blocks(
        &mut self,
        card: &SdCardInfo,
        sector: u64,
        out: &mut [u8],
    ) -> Result<(), DwMshcError> {
        let (argument, sectors) = transfer_parameters(card, sector, out.len())?;
        for attempt in 0..=2 {
            match self.read_blocks_once(argument, sectors, out) {
                Ok(()) => return Ok(()),
                Err(error) if attempt < 2 && retryable(error) => self.recover_data_path(),
                Err(error) => return Err(error),
            }
        }
        Err(DwMshcError::ShortTransfer)
    }

    pub(crate) fn write_blocks(
        &mut self,
        card: &SdCardInfo,
        sector: u64,
        data: &[u8],
    ) -> Result<(), DwMshcError> {
        let (argument, sectors) = transfer_parameters(card, sector, data.len())?;
        for attempt in 0..=2 {
            match self.write_blocks_once(argument, sectors, data) {
                Ok(()) => return self.wait_card_ready(card),
                Err(error) if attempt < 2 && retryable(error) => self.recover_data_path(),
                Err(error) => return Err(error),
            }
        }
        Err(DwMshcError::ShortTransfer)
    }

    fn read_blocks_once(
        &mut self,
        argument: u32,
        sectors: usize,
        out: &mut [u8],
    ) -> Result<(), DwMshcError> {
        if self.dma_supported(out.len()) {
            return self.read_dma_blocks_once(argument, sectors, out);
        }
        self.read_pio_blocks_once(argument, sectors, out)
    }

    fn read_pio_blocks_once(
        &mut self,
        argument: u32,
        sectors: usize,
        out: &mut [u8],
    ) -> Result<(), DwMshcError> {
        let command = transfer_command(sectors, false);
        self.prepare_data_transfer(out.len())?;
        self.start_data_command(command, argument, false)?;

        let mut copied = 0;
        let deadline = timer::get_time_ms().saturating_add(500);
        loop {
            let status = self.read(RINTSTS);
            self.check_data_status(command, status)?;
            let words = ((self.read(STATUS) >> 17) & 0x1fff).min(self.fifo_depth) as usize;
            for _ in 0..words {
                if copied >= out.len() {
                    break;
                }
                let bytes = self.read(self.data_offset).to_le_bytes();
                let count = (out.len() - copied).min(4);
                out[copied..copied + count].copy_from_slice(&bytes[..count]);
                copied += count;
            }
            self.acknowledge_data_status(status, INT_RXDR);
            if status & INT_DATA_OVER != 0 {
                self.write(RINTSTS, INT_DATA_OVER);
                if copied != out.len() {
                    return Err(DwMshcError::ShortTransfer);
                }
                return self.finish_data_transfer(sectors);
            }
            if timer::get_time_ms() >= deadline {
                return Err(DwMshcError::DataTimeout);
            }
            core::hint::spin_loop();
        }
    }

    fn write_blocks_once(
        &mut self,
        argument: u32,
        sectors: usize,
        data: &[u8],
    ) -> Result<(), DwMshcError> {
        if self.dma_supported(data.len()) {
            return self.write_dma_blocks_once(argument, sectors, data);
        }
        self.write_pio_blocks_once(argument, sectors, data)
    }

    fn write_pio_blocks_once(
        &mut self,
        argument: u32,
        sectors: usize,
        data: &[u8],
    ) -> Result<(), DwMshcError> {
        let command = transfer_command(sectors, true);
        self.prepare_data_transfer(data.len())?;
        self.start_data_command(command, argument, true)?;

        let mut pushed = 0;
        let deadline = timer::get_time_ms().saturating_add(500);
        loop {
            let status = self.read(RINTSTS);
            self.check_data_status(command, status)?;
            let words = (self.fifo_depth as usize)
                .saturating_sub(((self.read(STATUS) >> 17) & 0x1fff) as usize);
            for _ in 0..words {
                if pushed >= data.len() {
                    break;
                }
                let count = (data.len() - pushed).min(4);
                let mut bytes = [0u8; 4];
                bytes[..count].copy_from_slice(&data[pushed..pushed + count]);
                self.write(self.data_offset, u32::from_le_bytes(bytes));
                pushed += count;
            }
            self.acknowledge_data_status(status, INT_TXDR);
            if status & INT_DATA_OVER != 0 {
                self.write(RINTSTS, INT_DATA_OVER);
                if pushed != data.len() {
                    return Err(DwMshcError::ShortTransfer);
                }
                return self.finish_data_transfer(sectors);
            }
            if timer::get_time_ms() >= deadline {
                return Err(DwMshcError::DataTimeout);
            }
            core::hint::spin_loop();
        }
    }

    pub(super) fn prepare_data_transfer(&mut self, bytes: usize) -> Result<(), DwMshcError> {
        let bytes = u32::try_from(bytes).map_err(|_| DwMshcError::OutOfRange)?;
        self.write(CTRL, self.read(CTRL) | FIFO_RESET);
        self.wait_clear(CTRL, FIFO_RESET, 500, DwMshcError::CoreResetTimeout)?;
        self.write(BLKSIZ, SECTOR_BYTES as u32);
        self.write(BYTCNT, bytes);
        self.write(RINTSTS, ALL_INTERRUPTS);
        self.wait_idle(500)
    }

    pub(super) fn start_data_command(
        &mut self,
        command: u8,
        argument: u32,
        write: bool,
    ) -> Result<(), DwMshcError> {
        self.write(CMDARG, argument);
        io_fence();
        let flags = command_word(command, Response::R1, true, false)
            | CMD_USE_HOLD_REG
            | CMD_PRV_DAT_WAIT
            | if write { CMD_DAT_WR } else { 0 };
        self.write(CMD, CMD_START | flags);
        self.wait_clear(CMD, CMD_START, 500, DwMshcError::CommandTimeout(command))
    }

    pub(super) fn check_data_status(
        &mut self,
        command: u8,
        status: u32,
    ) -> Result<(), DwMshcError> {
        if let Some(error) = data_error(command, status) {
            self.write(RINTSTS, status);
            return Err(error);
        }
        if status & INT_CMD_DONE != 0 {
            check_card_status(command, self.read(RESP0))?;
        }
        Ok(())
    }

    pub(super) fn acknowledge_data_status(&mut self, status: u32, fifo_ready: u32) {
        let acknowledge = status & (fifo_ready | INT_CMD_DONE);
        if acknowledge != 0 {
            self.write(RINTSTS, acknowledge);
        }
    }

    pub(super) fn finish_data_transfer(&mut self, sectors: usize) -> Result<(), DwMshcError> {
        if transfer_needs_stop(sectors) {
            self.command(12, 0, Response::R1b, false, true)?;
        }
        Ok(())
    }

    fn wait_card_ready(&mut self, card: &SdCardInfo) -> Result<(), DwMshcError> {
        let deadline = timer::get_time_ms().saturating_add(500);
        loop {
            let response = self.command(13, (card.rca as u32) << 16, Response::R1, false, false)?;
            if response[0] & (1 << 8) != 0 {
                return Ok(());
            }
            if timer::get_time_ms() >= deadline {
                return Err(DwMshcError::DataTimeout);
            }
            core::hint::spin_loop();
        }
    }
}
