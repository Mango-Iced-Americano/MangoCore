//! 稀疏硬件 hart 到连续逻辑 CPU 拓扑的纯映射测试。
//!
//! VF2 从 hart1 启动，只能启动 U74 的 harts 1..=4；hart0 是没有 S-mode/MMU 的
//! S7，绝不能进入 SBI HSM 目标集合。这里不依赖启动时冻结的真实 FDT，直接锁定
//! 列表映射，使 QEMU 的稠密拓扑继续与原有旋转公式完全等价。

use alloc::vec;
use alloc::vec::Vec;

use crate::kernel_tests::runner::KernelTest;

pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new(
            "smp_topology::dense_lists_match_legacy_rotation",
            dense_lists_match_legacy_rotation,
        ),
        KernelTest::new(
            "smp_topology::vf2_sparse_harts_exclude_hart0",
            vf2_sparse_harts_exclude_hart0,
        ),
        KernelTest::new(
            "smp_topology::sparse_lookup_fails_closed",
            sparse_lookup_fails_closed,
        ),
        #[cfg(target_arch = "riscv64")]
        KernelTest::new(
            "smp_topology::isa_supervisor_check_uses_single_letter_segment",
            isa_supervisor_check_uses_single_letter_segment,
        ),
    ]
}

fn dense_lists_match_legacy_rotation() -> Result<(), &'static str> {
    let fixtures: &[(&[usize], &[usize])] = &[
        (&[0], &[0]),
        (&[0, 1], &[0]),
        (&[0, 1, 2, 3], &[0, 3]),
        (&[0, 1, 2, 3, 4, 5, 6, 7], &[0, 3, 5]),
    ];

    for &(harts, boots) in fixtures {
        for &boot in boots {
            for &hardware in harts {
                let expected = legacy_hardware_to_logical(hardware, boot);
                let logical = crate::smp::hardware_to_logical_id_list(harts, boot, hardware);
                if logical != Some(expected) {
                    return Err("dense hardware-to-logical mapping changed");
                }
                if crate::smp::logical_to_hardware_id_list(harts, boot, expected)
                    != Some(hardware)
                {
                    return Err("dense mapping is not a bijection");
                }
            }
            for logical in 0..harts.len() {
                let expected = legacy_logical_to_hardware(logical, boot);
                if crate::smp::logical_to_hardware_id_list(harts, boot, logical) != Some(expected)
                {
                    return Err("dense logical-to-hardware mapping changed");
                }
            }
        }
    }
    Ok(())
}

fn vf2_sparse_harts_exclude_hart0() -> Result<(), &'static str> {
    let harts = [1usize, 2, 3, 4];
    for (logical, &hardware) in harts.iter().enumerate() {
        if crate::smp::logical_to_hardware_id_list(&harts, 1, logical) != Some(hardware) {
            return Err("VF2 logical-to-hardware mapping is not contiguous");
        }
        if crate::smp::hardware_to_logical_id_list(&harts, 1, hardware) != Some(logical) {
            return Err("VF2 hardware-to-logical mapping is not contiguous");
        }
        if hardware == 0 {
            return Err("VF2 topology included the non-bootable S7 hart0");
        }
    }
    if crate::smp::logical_to_hardware_mask_list(&harts, 1, 0xf) != Some(0x1e) {
        return Err("VF2 logical mask did not exclude hart0");
    }
    Ok(())
}

fn sparse_lookup_fails_closed() -> Result<(), &'static str> {
    let harts = [1usize, 2, 4];
    if crate::smp::hardware_to_logical_id_list(&harts, 1, 0).is_some() {
        return Err("unknown hardware hart mapped to a logical CPU");
    }
    if crate::smp::logical_to_hardware_id_list(&harts, 1, harts.len()).is_some() {
        return Err("out-of-range logical CPU mapped to hardware");
    }
    if crate::smp::hardware_to_logical_id_list(&harts, 0, 1).is_some()
        || crate::smp::logical_to_hardware_id_list(&harts, 0, 1).is_some()
    {
        return Err("topology without its BSP hart did not fail closed");
    }
    Ok(())
}

#[cfg(target_arch = "riscv64")]
fn isa_supervisor_check_uses_single_letter_segment() -> Result<(), &'static str> {
    if crate::hal::firmware::isa_has_supervisor("rv64imacu_zba_zbb") {
        return Err("ISA without supervisor extension was accepted");
    }
    if !crate::hal::firmware::isa_has_supervisor("rv64imafdcbsux_zba_zbb") {
        return Err("ISA supervisor extension was rejected");
    }
    if !crate::hal::firmware::isa_has_supervisor("rv64imafdch_zicsr") {
        return Err("QEMU hypervisor ISA was rejected before supervisor implication handling");
    }
    if crate::hal::firmware::isa_has_supervisor("zicsr") {
        return Err("multi-letter extension created an ISA supervisor false positive");
    }
    if crate::hal::firmware::isa_has_supervisor("") {
        return Err("empty ISA was accepted");
    }
    Ok(())
}

fn legacy_hardware_to_logical(hardware: usize, boot: usize) -> usize {
    if hardware == boot {
        0
    } else if hardware < boot {
        hardware + 1
    } else {
        hardware
    }
}

fn legacy_logical_to_hardware(logical: usize, boot: usize) -> usize {
    if logical == 0 {
        boot
    } else if logical <= boot {
        logical - 1
    } else {
        logical
    }
}
