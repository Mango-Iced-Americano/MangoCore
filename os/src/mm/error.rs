#![allow(dead_code)]

pub type MmError = super::memory_set::MemoryError;
pub type MmResult<T> = Result<T, MmError>;
