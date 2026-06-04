//! InitramFS — 内核内嵌 initramfs newc cpio 解包器
//!
//! 在 VFS_ROOT 初始化阶段被调用，将嵌入内核的 newc cpio 归档
//! 解包到刚创建的 RamFS 根文件系统中。
//!
//! 支持的 cpio entry 类型：
//! - S_IFDIR  → 目录
//! - S_IFREG  → 普通文件（写数据）
//! - S_IFLNK  → 符号链接
//!
//! 不支持的类型（char/block/fifo/socket）静默跳过，因为 /dev
//! 由 DevFS 管理。

use alloc::{
    string::String,
    sync::Arc,
    vec::Vec,
};
use crate::utils::error::SyscallErr;

use super::vfs::{
    FileType, IndexNode, InodeMode, MountFS as _,
    File, FileFlags, MountFS,
};

// ── 常量 ────────────────────────────────────────────────────────────────

/// newc cpio magic
const NEWC_MAGIC: &[u8; 6] = b"070701";

/// Header 固定长度
const HEADER_LEN: usize = 110;

// ── 错误类型 ────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum InitramfsError {
    BadMagic,
    BadHex,
    Truncated,
    BadName,
    UnsupportedType(u32),
    Vfs(SyscallErr),
}

impl core::fmt::Display for InitramfsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            InitramfsError::BadMagic => write!(f, "bad cpio magic"),
            InitramfsError::BadHex => write!(f, "bad hex field"),
            InitramfsError::Truncated => write!(f, "truncated archive"),
            InitramfsError::BadName => write!(f, "bad filename"),
            InitramfsError::UnsupportedType(m) => write!(f, "unsupported file type: mode={:o}", m),
            InitramfsError::Vfs(e) => write!(f, "vfs error: {:?}", e),
        }
    }
}

// ── 解包统计 ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct InitramfsStats {
    pub files: usize,
    pub dirs: usize,
    pub symlinks: usize,
    pub bytes: usize,
}

// ── 嵌入归档访问 ────────────────────────────────────────────────────────

/// 返回嵌入内核的 initramfs cpio 归档切片
pub fn embedded_archive() -> &'static [u8] {
    extern "C" {
        static sinitramfs: u8;
        static einitramfs: u8;
    }
    // SAFETY: sinitramfs/einitramfs 是链接器定义的符号，
    // 指向 .data 段中由 .incbin 嵌入的 cpio 归档数据。
    unsafe {
        let start = &sinitramfs as *const u8 as usize;
        let end = &einitramfs as *const u8 as usize;
        if end <= start {
            return &[];
        }
        core::slice::from_raw_parts(start as *const u8, end - start)
    }
}

/// 解包嵌入的 initramfs 到指定 MountFS 根
pub fn unpack_embedded(root: &Arc<MountFS>) -> Result<InitramfsStats, InitramfsError> {
    let archive = embedded_archive();
    if archive.is_empty() {
        println!("[initramfs] embedded archive is empty, skipping unpack");
        return Ok(InitramfsStats::default());
    }
    unpack_newc(root, archive)
}

// ── newc header 解析 ────────────────────────────────────────────────────

/// 从 ASCII 十六进制字节串解析为 u32
fn parse_hex(bytes: &[u8]) -> Result<u32, InitramfsError> {
    if bytes.is_empty() {
        return Err(InitramfsError::BadHex);
    }
    let s = core::str::from_utf8(bytes).map_err(|_| InitramfsError::BadHex)?;
    u32::from_str_radix(s.trim(), 16).map_err(|_| InitramfsError::BadHex)
}

/// 从 newc header 字节解析 8 字节 ASCII hex 字段
fn field_u32(header: &[u8], offset: usize) -> Result<u32, InitramfsError> {
    parse_hex(&header[offset..offset + 8])
}

/// newc entry 解析结果
struct NewcEntry<'a> {
    /// 权限 mode
    mode: u32,
    /// 文件名
    name: &'a str,
    /// 文件数据
    data: &'a [u8],
}

/// 将 `mode` 中的文件类型位映射为 vfs FileType
fn mode_to_filetype(mode: u32) -> Option<FileType> {
    const S_IFMT: u32 = 0o170000;
    const S_IFSOCK: u32 = 0o140000;
    const S_IFLNK: u32 = 0o120000;
    const S_IFREG: u32 = 0o100000;
    const S_IFBLK: u32 = 0o060000;
    const S_IFDIR: u32 = 0o040000;
    const S_IFCHR: u32 = 0o020000;
    const S_IFIFO: u32 = 0o010000;

    match mode & S_IFMT {
        S_IFDIR => Some(FileType::Dir),
        S_IFREG => Some(FileType::File),
        S_IFLNK => Some(FileType::SymLink),
        S_IFBLK => None,   // skip, managed by DevFS
        S_IFCHR => None,   // skip, managed by DevFS
        S_IFIFO => None,   // skip
        S_IFSOCK => None,  // skip
        _ => None,         // unknown
    }
}

/// 将 `mode` 中的权限位提取为 InodeMode
fn mode_to_inodemode(mode: u32) -> InodeMode {
    InodeMode::from_bits_truncate(mode as u32 & 0o7777)
}

/// 读取下一个 newc entry，返回 (entry, consumed_bytes)
fn next_entry(archive: &[u8], pos: usize) -> Result<Option<(NewcEntry, usize)>, InitramfsError> {
    // 检查是否有足够空间放 header
    if pos + HEADER_LEN > archive.len() {
        return if pos >= archive.len() {
            Ok(None)
        } else {
            Err(InitramfsError::Truncated)
        };
    }

    let header = &archive[pos..pos + HEADER_LEN];

    // 检查 magic — 任何非 TRAILER 状态下的坏 magic 都是格式错误
    if &header[0..6] != NEWC_MAGIC {
        return Err(InitramfsError::BadMagic);
    }

    let namesize = field_u32(header, 94)? as usize;
    let filesize = field_u32(header, 54)? as usize;

    // 检查文件名是否在 archive 内（需要 NUL 终止符）
    if namesize < 1 || pos + HEADER_LEN + namesize > archive.len() {
        return Err(InitramfsError::Truncated);
    }

    let name_bytes = &archive[pos + HEADER_LEN..pos + HEADER_LEN + namesize];
    // newc 格式要求文件名以 NUL 结尾
    let name_nul_pos = name_bytes.iter().position(|&b| b == 0)
        .ok_or(InitramfsError::BadName)?;
    let name = core::str::from_utf8(&name_bytes[..name_nul_pos])
        .map_err(|_| InitramfsError::BadName)?;

    // newc 对齐规则：header + filename 整体对齐到 4 字节
    let header_filename_end = pos + HEADER_LEN + namesize;
    let data_start = align4(header_filename_end);
    let data_end = data_start + filesize;
    // 数据之后也要对齐到 4 字节
    let data_aligned = align4(data_end);

    if data_end > archive.len() || data_aligned > archive.len() {
        return Err(InitramfsError::Truncated);
    }

    let data = &archive[data_start..data_end];

    // 检查 TRAILER!!!
    if name == "TRAILER!!!" {
        return Ok(None);
    }

    let mode = field_u32(header, 14)?;

    Ok(Some((NewcEntry {
        mode,
        name,
        data,
    }, data_aligned)))
}

/// 对齐到 4 字节
fn align4(n: usize) -> usize {
    (n + 3) & !3
}

// ── 主解包函数 ──────────────────────────────────────────────────────────

/// 将 newc cpio 归档解包到指定 MountFS 根
///
/// # 安全
///
/// 本函数在 `VFS_ROOT` lazy_static 初始化期间被调用。
/// **不能**调用 `vfs_root()`、`vfs_lookup_parent()`、`create_or_open_file()`
/// 等会**递归触发 `VFS_ROOT`** 的函数。
///
/// 使用传入的 `root.mountpoint_root_inode()` 作为查找起点。
pub fn unpack_newc(
    root: &Arc<MountFS>,
    archive: &[u8],
) -> Result<InitramfsStats, InitramfsError> {
    let root_inode: Arc<dyn IndexNode> = root.mountpoint_root_inode();
    let mut stats = InitramfsStats::default();
    let mut pos = 0;

    while let Some((entry, next_pos)) = next_entry(archive, pos)? {
        pos = next_pos;

        // 处理路径：去除前导 "./" 或 "/"
        let clean_name = if let Some(rest) = entry.name.strip_prefix("./") {
            rest
        } else if let Some(rest) = entry.name.strip_prefix('/') {
            rest
        } else {
            entry.name
        };

        // 跳过 "." 和空路径
        if clean_name.is_empty() || clean_name == "." {
            continue;
        }

        // 拒绝 ".."
        if clean_name == ".." || clean_name.contains("/..") || clean_name.starts_with("..") {
            println!("[initramfs] skipping unsafe path: {}", entry.name);
            continue;
        }

        let filetype = match mode_to_filetype(entry.mode) {
            Some(ft) => ft,
            None => {
                // 跳过不支持的设备类型
                continue;
            }
        };

        // 逐级查找/创建目录，获取目标父目录和文件名
        let (parent, basename) = resolve_parent(&root_inode, clean_name)?;

        match filetype {
            FileType::Dir => {
                if parent.find(basename).is_ok() {
                    // 已存在，跳过
                    continue;
                }
                let mode = mode_to_inodemode(entry.mode) | InodeMode::S_IFDIR;
                parent.create(basename, FileType::Dir, mode)
                    .map_err(InitramfsError::Vfs)?;
                stats.dirs += 1;
            }
            FileType::File => {
                let inode = if let Ok(existing) = parent.find(basename) {
                    existing
                } else {
                    let mode = mode_to_inodemode(entry.mode) | InodeMode::S_IFREG;
                    parent.create(basename, FileType::File, mode)
                        .map_err(InitramfsError::Vfs)?
                };
                // 写入文件内容
                if !entry.data.is_empty() {
                    let file = File::new(inode, FileFlags::O_RDWR)
                        .map_err(InitramfsError::Vfs)?;
                    file.write(entry.data).map_err(InitramfsError::Vfs)?;
                }
                stats.files += 1;
                stats.bytes += entry.data.len();
            }
            FileType::SymLink => {
                if parent.find(basename).is_ok() {
                    continue;
                }
                let target = core::str::from_utf8(entry.data)
                    .map_err(|_| InitramfsError::BadName)?;
                parent.symlink(basename, target)
                    .map_err(InitramfsError::Vfs)?;
                stats.symlinks += 1;
            }
            _ => {} // 不应到达
        }
    }

    Ok(stats)
}

// ── 路径解析辅助 ────────────────────────────────────────────────────────

/// 在 root_inode 下解析路径，返回 (父目录 inode, 文件名)。
/// 自动创建中间目录。
fn resolve_parent<'a>(
    root: &Arc<dyn IndexNode>,
    path: &'a str,
) -> Result<(Arc<dyn IndexNode>, &'a str), InitramfsError> {
    let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if components.is_empty() {
        return Err(InitramfsError::BadName);
    }

    let basename = components[components.len() - 1];
    let parent_components = &components[..components.len() - 1];

    let mut current = root.clone();
    for &part in parent_components {
        if part == "." {
            continue;
        }
        current = match current.find(part) {
            Ok(inode) => inode,
            Err(_) => {
                // 自动创建中间目录
                current.create(part, FileType::Dir, InodeMode::from_bits_truncate(0o755))
                    .map_err(InitramfsError::Vfs)?
            }
        };
    }

    Ok((current, basename))
}
