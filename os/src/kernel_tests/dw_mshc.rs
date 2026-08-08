use alloc::vec::Vec;

use crate::drivers::block::dw_mshc;
use crate::kernel_tests::runner::KernelTest;

pub(crate) fn tests() -> Vec<KernelTest> {
    dw_mshc::ktests()
}
