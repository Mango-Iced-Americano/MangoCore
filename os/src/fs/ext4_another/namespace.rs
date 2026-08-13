macro_rules! writable_namespace_inode_mutations {
    () => {
        fn create(
            &self,
            name: &str,
            file_type: crate::fs::vfs::FileType,
            mode: crate::fs::vfs::InodeMode,
        ) -> Result<alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>, crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            if self.file_type != crate::fs::vfs::FileType::Dir {
                return Err(crate::utils::error::SyscallErr::ENOTDIR);
            }
            if name.is_empty() || name.len() > 255 || name.contains('/') {
                return Err(crate::utils::error::SyscallErr::EINVAL);
            }
            match file_type {
                crate::fs::vfs::FileType::File => {
                    let permission = another_ext4::InodeMode::from_bits_retain(
                        (mode.bits() & 0o777) as u16,
                    );
                    let parent = u32::try_from(self.key.inode_id())
                        .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
                    let child_id = fs.run_metadata_operation(|| {
                        fs.inner().create(
                            parent,
                            name,
                            another_ext4::InodeMode::from_type_and_perm(
                                another_ext4::FileType::RegularFile,
                                permission,
                            ),
                        )
                    })?;
                    super::inode::Ext4Inode::new(fs, child_id)
                        .map(|inode| inode as alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>)
                }
                crate::fs::vfs::FileType::Dir => self.mkdir(name, mode),
                crate::fs::vfs::FileType::SymLink => self.symlink(name, ""),
                _ => Err(crate::utils::error::SyscallErr::EINVAL),
            }
        }

        fn create_with_attrs(
            &self,
            name: &str,
            file_type: crate::fs::vfs::FileType,
            attrs: crate::fs::vfs::CreateAttrs,
        ) -> Result<alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>, crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            if self.file_type != crate::fs::vfs::FileType::Dir {
                return Err(crate::utils::error::SyscallErr::ENOTDIR);
            }
            if name.is_empty() || name.len() > 255 || name.contains('/') {
                return Err(crate::utils::error::SyscallErr::EINVAL);
            }
            let parent = u32::try_from(self.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            let permission = another_ext4::InodeMode::from_bits_retain(
                (attrs.mode.bits() & 0o7777) as u16,
            );
            let owner = another_ext4::InodeOwner {
                uid: attrs.uid,
                gid: attrs.gid,
            };
            let child_id = match file_type {
                crate::fs::vfs::FileType::File => fs.run_metadata_operation(|| {
                    fs.inner().create_with_owner(
                        parent,
                        name,
                        another_ext4::InodeMode::from_type_and_perm(
                            another_ext4::FileType::RegularFile,
                            permission,
                        ),
                        owner,
                    )
                })?,
                crate::fs::vfs::FileType::Dir => fs.run_metadata_operation(|| {
                    fs.inner().mkdir_with_owner(parent, name, permission, owner)
                })?,
                _ => return Err(crate::utils::error::SyscallErr::EINVAL),
            };
            super::inode::Ext4Inode::new(fs, child_id)
                .map(|inode| inode as alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>)
        }

        fn symlink(
            &self,
            name: &str,
            target: &str,
        ) -> Result<alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>, crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            if self.file_type != crate::fs::vfs::FileType::Dir {
                return Err(crate::utils::error::SyscallErr::ENOTDIR);
            }
            if name.is_empty() || name.len() > 255 || name.contains('/') {
                return Err(crate::utils::error::SyscallErr::EINVAL);
            }
            if fs.inner().lookup(
                u32::try_from(self.key.inode_id())
                    .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?,
                name,
            ).is_ok() {
                return Err(crate::utils::error::SyscallErr::EEXIST);
            }
            let parent = u32::try_from(self.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            let child = fs.run_metadata_operation(|| {
                fs.inner().symlink_with_owner_and_attr(
                    parent,
                    name,
                    target.as_bytes(),
                    another_ext4::InodeOwner { uid: 0, gid: 0 },
                )
            })?;
            super::inode::Ext4Inode::new(fs, child.ino)
                .map(|inode| inode as alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>)
        }

        fn symlink_with_attrs(
            &self,
            name: &str,
            target: &str,
            attrs: crate::fs::vfs::CreateAttrs,
        ) -> Result<alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>, crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            if self.file_type != crate::fs::vfs::FileType::Dir {
                return Err(crate::utils::error::SyscallErr::ENOTDIR);
            }
            if name.is_empty() || name.len() > 255 || name.contains('/') {
                return Err(crate::utils::error::SyscallErr::EINVAL);
            }
            let parent = u32::try_from(self.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            let child = fs.run_metadata_operation(|| {
                fs.inner().symlink_with_owner_and_attr(
                    parent,
                    name,
                    target.as_bytes(),
                    another_ext4::InodeOwner {
                        uid: attrs.uid,
                        gid: attrs.gid,
                    },
                )
            })?;
            super::inode::Ext4Inode::new(fs, child.ino)
                .map(|inode| inode as alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>)
        }

        fn link(
            &self,
            name: &str,
            other: &alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>,
        ) -> Result<(), crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            if self.file_type != crate::fs::vfs::FileType::Dir {
                return Err(crate::utils::error::SyscallErr::ENOTDIR);
            }
            if name.is_empty() || name.len() > 255 || name.contains('/') {
                return Err(crate::utils::error::SyscallErr::EINVAL);
            }
            let parent = u32::try_from(self.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            if fs.inner().lookup(parent, name).is_ok() {
                return Err(crate::utils::error::SyscallErr::EEXIST);
            }
            let target = other
                .as_any_ref()
                .downcast_ref::<super::inode::Ext4Inode>()
                .ok_or(crate::utils::error::SyscallErr::EXDEV)?;
            let target_fs = target.fs_arc()?;
            if fs.fs_id() != target_fs.fs_id() {
                return Err(crate::utils::error::SyscallErr::EXDEV);
            }
            let child = u32::try_from(target.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            fs.run_metadata_operation(|| fs.inner().link(child, parent, name))
        }

        fn mknod(
            &self,
            name: &str,
            mode: crate::fs::vfs::InodeMode,
            dev_t: u64,
        ) -> Result<alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>, crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            if self.file_type != crate::fs::vfs::FileType::Dir {
                return Err(crate::utils::error::SyscallErr::ENOTDIR);
            }
            if name.is_empty() || name.len() > 255 || name.contains('/') {
                return Err(crate::utils::error::SyscallErr::EINVAL);
            }
            let permission = another_ext4::InodeMode::from_bits_retain((mode.bits() & 0o7777) as u16);
            let type_bits = mode & crate::fs::vfs::InodeMode::S_IFMT;
            let (file_type, has_device) = match type_bits {
                bits if bits == crate::fs::vfs::InodeMode::S_IFIFO => {
                    (another_ext4::FileType::Fifo, false)
                }
                bits if bits == crate::fs::vfs::InodeMode::S_IFCHR => {
                    (another_ext4::FileType::CharacterDev, true)
                }
                bits if bits == crate::fs::vfs::InodeMode::S_IFBLK => {
                    (another_ext4::FileType::BlockDev, true)
                }
                bits if bits == crate::fs::vfs::InodeMode::S_IFSOCK => {
                    (another_ext4::FileType::Socket, false)
                }
                _ => return Err(crate::utils::error::SyscallErr::EINVAL),
            };
            let (major, minor) = if has_device {
                (
                    ((dev_t >> 8) & 0xfff) as u32,
                    ((dev_t & 0xff) | ((dev_t >> 12) & 0xfffff00)) as u32,
                )
            } else {
                (0, 0)
            };
            let parent = u32::try_from(self.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            let child_id = fs.run_metadata_operation(|| {
                fs.inner().mknod_with_owner(
                    parent,
                    name,
                    another_ext4::InodeMode::from_type_and_perm(file_type, permission),
                    major,
                    minor,
                    another_ext4::InodeOwner { uid: 0, gid: 0 },
                )
            })?;
            super::inode::Ext4Inode::new(fs, child_id)
                .map(|inode| inode as alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>)
        }

        fn create_with_data_and_attrs(
            &self,
            name: &str,
            file_type: crate::fs::vfs::FileType,
            attrs: crate::fs::vfs::CreateAttrs,
            data: usize,
        ) -> Result<alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>, crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            if self.file_type != crate::fs::vfs::FileType::Dir {
                return Err(crate::utils::error::SyscallErr::ENOTDIR);
            }
            if name.is_empty() || name.len() > 255 || name.contains('/') {
                return Err(crate::utils::error::SyscallErr::EINVAL);
            }
            let permission = another_ext4::InodeMode::from_bits_retain(
                (attrs.mode.bits() & 0o7777) as u16,
            );
            let (another_type, has_device) = match file_type {
                crate::fs::vfs::FileType::Pipe => (another_ext4::FileType::Fifo, false),
                crate::fs::vfs::FileType::CharDevice => {
                    (another_ext4::FileType::CharacterDev, true)
                }
                crate::fs::vfs::FileType::BlockDevice => {
                    (another_ext4::FileType::BlockDev, true)
                }
                crate::fs::vfs::FileType::Socket => (another_ext4::FileType::Socket, false),
                _ => return Err(crate::utils::error::SyscallErr::EINVAL),
            };
            let dev_t = data as u64;
            let (major, minor) = if has_device {
                (
                    ((dev_t >> 8) & 0xfff) as u32,
                    ((dev_t & 0xff) | ((dev_t >> 12) & 0xfffff00)) as u32,
                )
            } else {
                (0, 0)
            };
            let parent = u32::try_from(self.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            let child_id = fs.run_metadata_operation(|| {
                fs.inner().mknod_with_owner(
                    parent,
                    name,
                    another_ext4::InodeMode::from_type_and_perm(another_type, permission),
                    major,
                    minor,
                    another_ext4::InodeOwner {
                        uid: attrs.uid,
                        gid: attrs.gid,
                    },
                )
            })?;
            super::inode::Ext4Inode::new(fs, child_id)
                .map(|inode| inode as alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>)
        }

        fn mkdir(
            &self,
            name: &str,
            mode: crate::fs::vfs::InodeMode,
        ) -> Result<alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>, crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            if self.file_type != crate::fs::vfs::FileType::Dir {
                return Err(crate::utils::error::SyscallErr::ENOTDIR);
            }
            if name.is_empty() || name.len() > 255 || name.contains('/') {
                return Err(crate::utils::error::SyscallErr::EINVAL);
            }
            let permission = another_ext4::InodeMode::from_bits_retain((mode.bits() & 0o777) as u16);
            let parent = u32::try_from(self.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            let child_id = fs.run_metadata_operation(|| {
                fs.inner().mkdir(
                    parent,
                    name,
                    permission,
                )
            })?;
            super::inode::Ext4Inode::new(fs, child_id)
                .map(|inode| inode as alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>)
        }

        fn rename(
            &self,
            old_name: &str,
            new_parent: &alloc::sync::Arc<dyn crate::fs::vfs::IndexNode>,
            new_name: &str,
            flags: u32,
        ) -> Result<(), crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            if self.file_type != crate::fs::vfs::FileType::Dir {
                return Err(crate::utils::error::SyscallErr::ENOTDIR);
            }
            if old_name.is_empty()
                || new_name.is_empty()
                || old_name.len() > 255
                || new_name.len() > 255
                || old_name.contains('/')
                || new_name.contains('/')
                || flags != 0
            {
                return Err(crate::utils::error::SyscallErr::EINVAL);
            }
            let target_parent = new_parent
                .as_any_ref()
                .downcast_ref::<super::inode::Ext4Inode>()
                .ok_or(crate::utils::error::SyscallErr::EXDEV)?;
            let target_fs = target_parent.fs_arc()?;
            if fs.fs_id() != target_fs.fs_id() {
                return Err(crate::utils::error::SyscallErr::EXDEV);
            }
            let old_parent = u32::try_from(self.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            let new_parent = u32::try_from(target_parent.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            let reclaim = fs.run_metadata_operation(|| {
                fs.inner().rename(
                    old_parent,
                    old_name,
                    new_parent,
                    new_name,
                )
            })?;
            if let Some(handle) = reclaim {
                fs.attach_reclaim_handle(handle)?;
            }
            Ok(())
        }

        fn unlink(&self, name: &str) -> Result<(), crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            if self.file_type != crate::fs::vfs::FileType::Dir {
                return Err(crate::utils::error::SyscallErr::ENOTDIR);
            }
            if name.is_empty() || name.len() > 255 || name.contains('/') {
                return Err(crate::utils::error::SyscallErr::EINVAL);
            }
            let parent = u32::try_from(self.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            let reclaim = fs.run_metadata_operation(|| fs.inner().unlink(parent, name))?;
            if let Some(handle) = reclaim {
                fs.attach_reclaim_handle(handle)?;
            }
            Ok(())
        }

        fn rmdir(&self, name: &str) -> Result<(), crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            if self.file_type != crate::fs::vfs::FileType::Dir {
                return Err(crate::utils::error::SyscallErr::ENOTDIR);
            }
            if name.is_empty() || name.len() > 255 || name.contains('/') {
                return Err(crate::utils::error::SyscallErr::EINVAL);
            }
            let parent = u32::try_from(self.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            let reclaim = fs.run_metadata_operation(|| fs.inner().rmdir(parent, name))?;
            if let Some(handle) = reclaim {
                fs.attach_reclaim_handle(handle)?;
            }
            Ok(())
        }
    };
}

pub(crate) use writable_namespace_inode_mutations;
