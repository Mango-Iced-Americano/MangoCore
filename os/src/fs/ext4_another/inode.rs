use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::{any::Any, convert::TryFrom, fmt, sync::atomic::Ordering};
use spin::{Mutex, MutexGuard};

use crate::fs::{
    page_cache::PageCache,
    vfs::{
        FileFlags, FilePrivateData, FileSystem, FileType, IndexNode, InodeFlags, InodeId,
        InodeMode, Metadata,
    },
};
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;

use super::errno::from_another;
use super::fs::Ext4FileSystem;
use super::lifetime::{InodeKey, InodeLifetime};
use super::page_cache::AnotherExt4PageCacheBackend;

/// Writable VFS inode identified by its stable ext4 inode number.
pub(crate) struct Ext4Inode {
    owner: InodeOwner,
    key: InodeKey,
    file_type: FileType,
    self_ref: Mutex<Weak<Ext4Inode>>,
    lifetime: Arc<InodeLifetime>,
    page_cache: Mutex<Option<Arc<PageCache>>>,
}

/// Filesystem ownership for an inode.
///
/// Ordinary inodes keep their filesystem alive for their whole VFS lifetime.
/// The canonical root is owned by the filesystem itself, so its reverse edge
/// must be weak to avoid a reference cycle.
enum InodeOwner {
    Strong(Arc<Ext4FileSystem>),
    CanonicalRoot(Weak<Ext4FileSystem>),
}

impl InodeOwner {
    fn upgrade(&self) -> Result<Arc<Ext4FileSystem>, SyscallErr> {
        match self {
            Self::Strong(fs) => Ok(fs.clone()),
            Self::CanonicalRoot(fs) => fs.upgrade().ok_or(SyscallErr::EIO),
        }
    }
}

impl fmt::Debug for Ext4Inode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnotherExt4Inode")
            .field("inode_id", &self.key.inode_id())
            .field("file_type", &self.file_type)
            .finish()
    }
}

impl Ext4Inode {
    pub(crate) fn new(fs: Arc<Ext4FileSystem>, inode_id: u32) -> Result<Arc<Self>, SyscallErr> {
        let owner = InodeOwner::Strong(fs.clone());
        Self::new_with_owner(fs, owner, inode_id)
    }

    pub(crate) fn new_root(
        fs: &Arc<Ext4FileSystem>,
        inode_id: u32,
    ) -> Result<Arc<Self>, SyscallErr> {
        let owner = InodeOwner::CanonicalRoot(Arc::downgrade(fs));
        Self::new_with_owner(fs.clone(), owner, inode_id)
    }

    fn new_with_owner(
        fs: Arc<Ext4FileSystem>,
        owner: InodeOwner,
        inode_id: u32,
    ) -> Result<Arc<Self>, SyscallErr> {
        let attr = fs
            .inner()
            .getattr(inode_id)
            .map_err(|error| from_another(error.code()))?;
        let inode_id = usize::try_from(inode_id).map_err(|_| SyscallErr::EFBIG)?;
        let file_type = map_file_type(attr.ftype);
        let size = usize::try_from(attr.size).map_err(|_| SyscallErr::EFBIG)?;
        let key = InodeKey::new(fs.fs_id(), inode_id, attr.generation);
        let lifetime = fs.lifetime(key, size);
        Ok(Arc::new_cyclic(|self_ref| Self {
            owner,
            key,
            file_type,
            self_ref: Mutex::new(self_ref.clone()),
            lifetime,
            page_cache: Mutex::new(None),
        }))
    }

    fn fs_arc(&self) -> Result<Arc<Ext4FileSystem>, SyscallErr> {
        self.owner.upgrade()
    }

    fn attr(&self, fs: &Ext4FileSystem) -> Result<another_ext4::FileAttr, SyscallErr> {
        fs.inner()
            .getattr(u32::try_from(self.key.inode_id()).map_err(|_| SyscallErr::EFBIG)?)
            .map_err(|error| from_another(error.code()))
    }

    fn self_arc(&self) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        self.self_ref
            .lock()
            .upgrade()
            .map(|inode| inode as Arc<dyn IndexNode>)
            .ok_or(SyscallErr::EIO)
    }

    fn regular_page_cache(&self, fs: &Arc<Ext4FileSystem>) -> Arc<PageCache> {
        if let Some(cache) = self.page_cache.lock().clone() {
            return cache;
        }
        if let Some(cache) = self.lifetime.page_cache() {
            let mut page_cache = self.page_cache.lock();
            return page_cache.get_or_insert(cache).clone();
        }
        let cache = PageCache::new();
        cache.set_backend(Arc::new(AnotherExt4PageCacheBackend::new(
            fs.clone(),
            self.key,
            self.lifetime.clone(),
        )));
        let cache = self.lifetime.install_page_cache(cache);
        let mut page_cache = self.page_cache.lock();
        page_cache.get_or_insert(cache).clone()
    }

    fn read_regular(
        &self,
        fs: &Arc<Ext4FileSystem>,
        offset: usize,
        len: usize,
        buffer: &mut [u8],
    ) -> Result<usize, SyscallErr> {
        let size = self.lifetime.logical_size.load(Ordering::Acquire);
        let actual = len.min(buffer.len()).min(size.saturating_sub(offset));
        if actual == 0 {
            return Ok(0);
        }
        self.regular_page_cache(fs)
            .read(offset, &mut buffer[..actual])
            .map_err(|_| SyscallErr::EIO)
    }
}

impl IndexNode for Ext4Inode {
    fn read_at(
        &self,
        offset: usize,
        len: usize,
        buffer: &mut [u8],
        data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        drop(data);
        let fs = self.fs_arc()?;
        match self.file_type {
            FileType::File => self.read_regular(&fs, offset, len, buffer),
            FileType::SymLink => {
                let actual = len.min(buffer.len());
                fs.inner()
                    .readlink(
                        u32::try_from(self.key.inode_id()).map_err(|_| SyscallErr::EFBIG)?,
                        offset,
                        &mut buffer[..actual],
                    )
                    .map_err(|error| from_another(error.code()))
            }
            FileType::Dir => Err(SyscallErr::EISDIR),
            FileType::CharDevice
            | FileType::BlockDevice
            | FileType::Socket
            | FileType::Pipe
            | FileType::FramebufferDevice
            | FileType::KvmDevice => Err(SyscallErr::EINVAL),
        }
    }

    super::mutations::writable_data_inode_mutations!();
    super::namespace::writable_namespace_inode_mutations!();

    fn create_with_data(
        &self,
        name: &str,
        file_type: FileType,
        mode: InodeMode,
        _data: usize,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        match file_type {
            FileType::Pipe
            | FileType::CharDevice
            | FileType::BlockDevice
            | FileType::Socket => self.mknod(name, mode, _data as u64),
            _ => self.create(name, file_type, mode),
        }
    }

    fn open(
        &self,
        _data: MutexGuard<FilePrivateData>,
        flags: &FileFlags,
    ) -> Result<(), SyscallErr> {
        let _fs = self.fs_arc()?;
        if flags.contains(FileFlags::O_TRUNC) {
            self.resize(0)?;
        }
        Ok(())
    }

    fn close(&self, _data: MutexGuard<FilePrivateData>) -> Result<(), SyscallErr> {
        drop(_data);
        self.sync()
    }

    fn find(&self, name: &str) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        let fs = self.fs_arc()?;
        if name.is_empty() {
            return Err(SyscallErr::ENOENT);
        }
        if name.len() > 255 {
            return Err(SyscallErr::ENAMETOOLONG);
        }
        if name.contains('/') {
            return Err(SyscallErr::EINVAL);
        }
        if name == "." {
            return self.self_arc();
        }
        let inode_id = fs
            .inner()
            .lookup(
                u32::try_from(self.key.inode_id()).map_err(|_| SyscallErr::EFBIG)?,
                name,
            )
            .map_err(|error| from_another(error.code()))?;
        Ext4Inode::new(fs, inode_id).map(|inode| inode as Arc<dyn IndexNode>)
    }

    fn list(&self) -> Result<Vec<String>, SyscallErr> {
        let fs = self.fs_arc()?;
        let entries = fs
            .inner()
            .listdir(u32::try_from(self.key.inode_id()).map_err(|_| SyscallErr::EFBIG)?)
            .map_err(|error| from_another(error.code()))?;
        Ok(entries.into_iter().map(|entry| entry.name()).collect())
    }

    fn list_dirents(&self) -> Result<Vec<(String, InodeId, FileType)>, SyscallErr> {
        let fs = self.fs_arc()?;
        let entries = fs
            .inner()
            .listdir(u32::try_from(self.key.inode_id()).map_err(|_| SyscallErr::EFBIG)?)
            .map_err(|error| from_another(error.code()))?;
        Ok(entries
            .into_iter()
            .map(|entry| {
                Ok((
                    entry.name(),
                    usize::try_from(entry.inode()).map_err(|_| SyscallErr::EFBIG)?,
                    map_file_type(entry.file_type()),
                ))
            })
            .collect::<Result<Vec<_>, SyscallErr>>()?)
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        let fs = self.fs_arc()?;
        let attr = self.attr(&fs)?;
        let file_type = map_file_type(attr.ftype);
        let permissions = InodeMode::from_bits_truncate(u32::from(attr.perm.bits()));
        Ok(Metadata {
            dev_id: fs.fs_id(),
            inode_id: self.key.inode_id(),
            size: i64::try_from(if self.file_type == FileType::File {
                self.lifetime.logical_size.load(Ordering::Acquire)
            } else {
                usize::try_from(attr.size).map_err(|_| SyscallErr::EFBIG)?
            })
            .map_err(|_| SyscallErr::EFBIG)?,
            blk_size: another_ext4::BLOCK_SIZE,
            blocks: usize::try_from(attr.blocks).map_err(|_| SyscallErr::EFBIG)?,
            atime: TimeSpec::from_s(usize::try_from(attr.atime).map_err(|_| SyscallErr::EFBIG)?),
            mtime: TimeSpec::from_s(usize::try_from(attr.mtime).map_err(|_| SyscallErr::EFBIG)?),
            ctime: TimeSpec::from_s(usize::try_from(attr.ctime).map_err(|_| SyscallErr::EFBIG)?),
            file_type,
            mode: InodeMode::from(file_type) | permissions,
            flags: self.lifetime.inode_flags(),
            nlinks: u64::from(attr.links),
            uid: attr.uid,
            gid: attr.gid,
            raw_dev: (u64::from(attr.rdev.0) << 32) | u64::from(attr.rdev.1),
        })
    }

    fn set_metadata(&self, metadata: &Metadata) -> Result<(), SyscallErr> {
        let fs = self.fs_arc()?;
        let attr = self.attr(&fs)?;
        let permissions = another_ext4::InodeMode::from_bits_retain(
            (metadata.mode & InodeMode::S_IALLUGO).bits() as u16,
        );
        let attributes = another_ext4::SetAttr {
            mode: Some(another_ext4::InodeMode::from_type_and_perm(
                attr.ftype,
                permissions,
            )),
            uid: Some(metadata.uid),
            gid: Some(metadata.gid),
            size: None,
            atime: Some(u32::try_from(metadata.atime.tv_sec).map_err(|_| SyscallErr::EFBIG)?),
            mtime: Some(u32::try_from(metadata.mtime.tv_sec).map_err(|_| SyscallErr::EFBIG)?),
            ctime: Some(u32::try_from(metadata.ctime.tv_sec).map_err(|_| SyscallErr::EFBIG)?),
            crtime: None,
        };
        self.lifetime.set_inode_flags(metadata.flags);
        fs.inner()
            .setattr(
                u32::try_from(self.key.inode_id()).map_err(|_| SyscallErr::EFBIG)?,
                attributes,
            )
            .map_err(|error| from_another(error.code()))
    }

    fn fs(&self) -> Arc<dyn FileSystem> {
        match self.fs_arc() {
            Ok(fs) => fs,
            Err(_) => unreachable!("MountFS must retain the filesystem for a live inode"),
        }
    }

    fn page_cache(&self) -> Option<Arc<PageCache>> {
        self.lifetime.page_cache()
    }

    fn ensure_page_cache(&self) -> Option<Arc<PageCache>> {
        if self.file_type != FileType::File {
            return None;
        }
        self.fs_arc().ok().map(|fs| self.regular_page_cache(&fs))
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

impl Drop for Ext4Inode {
    fn drop(&mut self) {
        self.lifetime.unpin();
    }
}

fn map_file_type(file_type: another_ext4::FileType) -> FileType {
    match file_type {
        another_ext4::FileType::RegularFile | another_ext4::FileType::Unknown => FileType::File,
        another_ext4::FileType::Directory => FileType::Dir,
        another_ext4::FileType::CharacterDev => FileType::CharDevice,
        another_ext4::FileType::BlockDev => FileType::BlockDevice,
        another_ext4::FileType::Fifo => FileType::Pipe,
        another_ext4::FileType::Socket => FileType::Socket,
        another_ext4::FileType::SymLink => FileType::SymLink,
    }
}
