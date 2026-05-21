//! /proc/config — 内核编译配置（供 LTP 使用）

use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;
use alloc::string::String;
use core::fmt::Write;

pub fn config_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(2048);

    let _ = writeln!(s, "# OSKernel2026-Mango kernel configuration");

    // ── Architecture ──
    if cfg!(target_arch = "riscv64") {
        let _ = writeln!(s, "CONFIG_RISCV=y");
        let _ = writeln!(s, "CONFIG_TICKS_PER_SEC=25");
    } else if cfg!(target_arch = "loongarch64") {
        let _ = writeln!(s, "CONFIG_LOONGARCH=y");
        let _ = writeln!(s, "CONFIG_TICKS_PER_SEC=100");
    }
    let _ = writeln!(s, "CONFIG_64BIT=y");
    let _ = writeln!(s, "CONFIG_MMU=y");

    // ── Memory ──
    let _ = writeln!(s, "CONFIG_PAGE_SIZE={}", crate::config::PAGE_SIZE);
    let _ = writeln!(s, "CONFIG_MEMORY_SIZE={}", crate::config::MEMORY_SIZE);
    let _ = writeln!(
        s,
        "CONFIG_KERNEL_HEAP_SIZE={}",
        crate::config::KERNEL_HEAP_SIZE
    );
    let _ = writeln!(
        s,
        "CONFIG_SYSTEM_TASK_LIMIT={}",
        crate::config::SYSTEM_TASK_LIMIT
    );
    let _ = writeln!(
        s,
        "CONFIG_SYSTEM_FD_LIMIT={}",
        crate::config::SYSTEM_FD_LIMIT
    );
    #[cfg(feature = "swap")]
    let _ = writeln!(s, "CONFIG_SWAP=y");
    #[cfg(not(feature = "swap"))]
    let _ = writeln!(s, "# CONFIG_SWAP is not set");
    #[cfg(feature = "zram")]
    let _ = writeln!(s, "CONFIG_ZRAM=y");
    #[cfg(not(feature = "zram"))]
    let _ = writeln!(s, "# CONFIG_ZRAM is not set");
    #[cfg(feature = "oom_handler")]
    let _ = writeln!(s, "CONFIG_OOM_HANDLER=y");

    // ── Filesystem ──
    let _ = writeln!(s, "CONFIG_EXT4_FS=y");
    let _ = writeln!(s, "CONFIG_FAT_FS=y");
    let _ = writeln!(s, "CONFIG_PROC_FS=y");
    let _ = writeln!(s, "CONFIG_DEVFS=y");
    let _ = writeln!(s, "CONFIG_RAMFS=y");
    let _ = writeln!(s, "CONFIG_PAGE_CACHE=y");
    let _ = writeln!(s, "CONFIG_TMPFS=y");

    // ── Block ──
    let _ = writeln!(s, "CONFIG_BLOCK=y");
    #[cfg(feature = "block_virt")]
    let _ = writeln!(s, "CONFIG_VIRTIO_BLK=y");
    #[cfg(feature = "block_virt_pci")]
    let _ = writeln!(s, "CONFIG_VIRTIO_BLK_PCI=y");
    #[cfg(feature = "block_mem")]
    let _ = writeln!(s, "CONFIG_BLK_MEM=y");

    // ── Network ──
    let _ = writeln!(s, "CONFIG_NET=y");
    let _ = writeln!(s, "CONFIG_INET=y");
    let _ = writeln!(s, "CONFIG_UNIX=y");
    let _ = writeln!(s, "CONFIG_PACKET=y");
    let _ = writeln!(s, "CONFIG_TCP=y");
    let _ = writeln!(s, "CONFIG_UDP=y");
    #[cfg(any(feature = "block_virt", feature = "block_virt_pci"))]
    let _ = writeln!(s, "CONFIG_VIRTIO_NET=y");

    // ── Process / IPC ──
    let _ = writeln!(s, "CONFIG_FUTEX=y");
    let _ = writeln!(s, "CONFIG_EPOLL=y");
    let _ = writeln!(s, "CONFIG_EVENTFD=y");
    let _ = writeln!(s, "CONFIG_SIGNALFD=y");
    let _ = writeln!(s, "CONFIG_TIMERFD=y");
    let _ = writeln!(s, "CONFIG_SIGNAL=y");
    let _ = writeln!(s, "# CONFIG_SYSVIPC is not set");
    let _ = writeln!(s, "# CONFIG_PTRACE is not set");

    // ── Namespaces ──
    let _ = writeln!(s, "CONFIG_PID_NS=y");
    let _ = writeln!(s, "CONFIG_NET_NS=y");
    let _ = writeln!(s, "CONFIG_USER_NS=y");
    let _ = writeln!(s, "# CONFIG_MNT_NS is not set");
    let _ = writeln!(s, "# CONFIG_IPC_NS is not set");

    // ── Devices ──
    let _ = writeln!(s, "CONFIG_TTY=y");
    let _ = writeln!(s, "CONFIG_PIPE=y");

    // ── Compressed ELF ──
    #[cfg(feature = "comp")]
    let _ = writeln!(s, "CONFIG_COMPRESSED_ELF=y");
    #[cfg(not(feature = "comp"))]
    let _ = writeln!(s, "# CONFIG_COMPRESSED_ELF is not set");

    // ── Not implemented (LTP skip-list) ──
    let _ = writeln!(s, "# CONFIG_KVM is not set");
    let _ = writeln!(s, "# CONFIG_SQUASHFS is not set");
    let _ = writeln!(s, "# CONFIG_BLK_DEV_LOOP is not set");
    let _ = writeln!(s, "# CONFIG_TUN is not set");
    let _ = writeln!(s, "# CONFIG_PROVE_LOCKING is not set");
    let _ = writeln!(s, "# CONFIG_LOCKDEP is not set");
    let _ = writeln!(s, "# CONFIG_KASAN is not set");

    proc_read_str(offset, len, buf, &s)
}
