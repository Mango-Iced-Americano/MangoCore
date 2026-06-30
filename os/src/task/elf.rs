//! ELF 装载辅助类型。
//!
//! 本文件保存用户栈 auxv 条目、ELF 装载结果摘要，以及动态解释器
//! 的 VFS 查找与内核映射逻辑。真正的 ELF 段解析在 `mm::AddressSpace::from_elf`
//! 中完成。

use alloc::boxed::Box;

use crate::{
    fs::{vfs, vfs_lookup_absolute},
    mm::KERNEL_SPACE,
    syscall::errno::*,
};

#[derive(Clone, Copy)]
#[allow(non_camel_case_types, unused)]
#[repr(usize)]
/// Linux 用户栈 auxiliary vector 的键值类型。
///
/// # Linux Compatibility
///
/// 枚举值保持 Linux ABI 编号，用于 `execve` 构造初始用户栈。当前只会实际
/// 写入内核支持的子集，未使用项保留编号以便后续扩展。
pub enum AuxvType {
    NULL = 0,
    IGNORE = 1,
    EXECFD = 2,
    PHDR = 3,
    PHENT = 4,
    PHNUM = 5,
    PAGESZ = 6,
    BASE = 7,
    FLAGS = 8,
    ENTRY = 9,
    NOTELF = 10,
    UID = 11,
    EUID = 12,
    GID = 13,
    EGID = 14,
    PLATFORM = 15,
    HWCAP = 16,
    CLKTCK = 17,
    FPUCW = 18,
    DCACHEBSIZE = 19,
    ICACHEBSIZE = 20,
    UCACHEBSIZE = 21,
    IGNOREPPC = 22,
    SECURE = 23,
    BASE_PLATFORM = 24,
    RANDOM = 25,
    HWCAP2 = 26,
    EXECFN = 31,
    SYSINFO = 32,
    SYSINFO_EHDR = 33,
    L1I_CACHESHAPE = 34,
    L1D_CACHESHAPE = 35,
    L2_CACHESHAPE = 36,
    L3_CACHESHAPE = 37,
    L1I_CACHESIZE = 40,
    L1I_CACHEGEOMETRY = 41,
    L1D_CACHESIZE = 42,
    L1D_CACHEGEOMETRY = 43,
    L2_CACHESIZE = 44,
    L2_CACHEGEOMETRY = 45,
    L3_CACHESIZE = 46,
    L3_CACHEGEOMETRY = 47,
    MINSIGSTKSZ = 51,
}

#[derive(Clone, Copy)]
#[allow(unused)]
#[repr(C)]
/// 写入用户栈的单个 auxv 条目。
pub struct AuxvEntry {
    auxv_type: AuxvType,
    auxv_val: usize,
}

impl AuxvEntry {
    /// 构造一个 auxv 键值对。
    pub fn new(auxv_type: AuxvType, auxv_val: usize) -> Self {
        Self {
            auxv_type,
            auxv_val,
        }
    }
}

#[repr(C)]
/// ELF 装载后供用户栈构造和入口跳转使用的摘要信息。
pub struct ELFInfo {
    /// 主 ELF 入口地址。
    pub entry: usize,
    /// 动态解释器入口地址；静态 ELF 为 `None`。
    pub interp_entry: Option<usize>,
    /// 程序装载基址。
    pub base: usize,
    /// 程序头表条目数量。
    pub phnum: usize,
    /// 程序头表条目大小。
    pub phent: usize,
    /// 用户可见程序头表地址。
    pub phdr: usize,
}

/// 通过 VFS 加载 ELF 动态解释器并映射到内核空间。
///
/// # Errors
///
/// - `-ENOEXEC`：路径存在但不是普通文件。
/// - `-ELIBBAD`：文件过小或 ELF 魔数不匹配。
/// - 其他负 errno：VFS lookup/open/read 失败。
///
/// # Locking
///
/// 函数只短暂持有 VFS/File 内部锁，不在锁内进入调度等待点。
pub fn load_elf_interp(path: &str) -> Result<&'static [u8], isize> {
    let inode = vfs_lookup_absolute(path)?;
    let file = vfs::File::new(inode, vfs::FileFlags::O_RDONLY).map_err(|e| e as isize)?;
    if file.file_type() != vfs::FileType::File {
        log::warn!(
            "[load_elf_interp] Interpreter {} is not a Regular File!",
            path
        );
        return Err(ENOEXEC);
    }
    let size = file.get_size();
    if size < 4 {
        return Err(ELIBBAD);
    }
    // ELF 解释器必须至少有标准魔数，后续完整解析由 ELF loader 负责。
    let mut magic_number = [0u8; 4];
    let n = file.pread(0, &mut magic_number).map_err(|e| e as isize)?;
    if n < 4 || &magic_number != b"\x7fELF" {
        return Err(ELIBBAD);
    }
    // 使用当前内核空间最高可用地址创建临时只读映射，调用方完成解析后清理。
    let buffer_addr = KERNEL_SPACE.lock().highest_addr();
    Ok(file.map_to_kernel_space(buffer_addr.0))
}
