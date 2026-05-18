/*
    此文件用于解析ELF文件
    内容与RISCV版本相同，无需修改
*/
use alloc::boxed::Box;

use crate::{
    fs::{vfs, vfs_lookup_absolute},
    mm::KERNEL_SPACE,
    syscall::errno::*,
};

#[derive(Clone, Copy)]
#[allow(non_camel_case_types, unused)]
#[repr(usize)]
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
pub struct AuxvEntry {
    auxv_type: AuxvType,
    auxv_val: usize,
}

impl AuxvEntry {
    pub fn new(auxv_type: AuxvType, auxv_val: usize) -> Self {
        Self {
            auxv_type,
            auxv_val,
        }
    }
}

#[repr(C)]
pub struct ELFInfo {
    // 入口地址
    pub entry: usize,
    // 解析器入口地址
    pub interp_entry: Option<usize>,
    // 基地址
    pub base: usize,
    // 程序头表条目数量
    pub phnum: usize,
    // 程序头表条目大小
    pub phent: usize,
    // 程序头表地址
    pub phdr: usize,
}

/// 加载ELF解释器（使用新 VFS）
pub fn load_elf_interp(path: &str) -> Result<&'static [u8], isize> {
    log::info!("[load_elf_interp]Loading ELF interpreter: {}", path);
    // 使用新 VFS 查找并打开解释器文件
    let inode = vfs_lookup_absolute(path)?;
    let file = vfs::File::new(inode, vfs::FileFlags::O_RDONLY).map_err(|e| e as isize)?;
    // 增加一层防护：解释器必须是普通文件
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
    // 读取文件头的前4个字节，即魔数'\x7fELF'
    let mut magic_number = [0u8; 4];
    let n = file.pread(0, &mut magic_number).map_err(|e| e as isize)?;
    if n < 4 || &magic_number != b"\x7fELF" {
        return Err(ELIBBAD);
    }
    // 映射到内核空间（使用最高可用地址作为映射基址）
    let buffer_addr = KERNEL_SPACE.lock().highest_addr();
    Ok(file.map_to_kernel_space(buffer_addr.0))
}
