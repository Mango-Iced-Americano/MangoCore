macro_rules! writable_data_inode_mutations {
    () => {
        fn write_at(
            &self,
            offset: usize,
            len: usize,
            buffer: &[u8],
            data: spin::MutexGuard<crate::fs::vfs::FilePrivateData>,
        ) -> Result<usize, crate::utils::error::SyscallErr> {
            drop(data);
            let fs = self.fs_arc()?;
            match self.file_type {
                crate::fs::vfs::FileType::Dir => {
                    return Err(crate::utils::error::SyscallErr::EISDIR)
                }
                crate::fs::vfs::FileType::File => {}
                _ => return Err(crate::utils::error::SyscallErr::EINVAL),
            }
            let actual = len.min(buffer.len());
            if actual == 0 {
                return Ok(0);
            }
            let end = offset
                .checked_add(actual)
                .ok_or(crate::utils::error::SyscallErr::EFBIG)?;
            let inode_id = u32::try_from(self.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            let old_size = self
                .lifetime
                .logical_size
                .load(core::sync::atomic::Ordering::Acquire);
            let written = self.regular_page_cache(&fs).write_kernel(
                offset,
                &buffer[..actual],
                old_size,
            )?;
            self.lifetime
                .logical_size
                .fetch_max(end, core::sync::atomic::Ordering::AcqRel);
            self.lifetime
                .size_generation
                .fetch_add(1, core::sync::atomic::Ordering::AcqRel);
            Ok(written)
        }

        fn write_at_user(
            &self,
            offset: usize,
            len: usize,
            source: &crate::mm::UserBuffer,
        ) -> Result<usize, crate::utils::error::SyscallErr> {
            let _fs = self.fs_arc()?;
            let actual = len.min(source.len());
            let mut buffer = alloc::vec::Vec::new();
            buffer
                .try_reserve_exact(actual)
                .map_err(|_| crate::utils::error::SyscallErr::ENOMEM)?;
            buffer.resize(actual, 0);
            let copied = source
                .read_into(&mut buffer)
                .map_err(|_| crate::utils::error::SyscallErr::EFAULT)?;
            let private = spin::Mutex::new(crate::fs::vfs::FilePrivateData::Unused);
            self.write_at(offset, copied, &buffer[..copied], private.lock())
        }

        fn write_direct(
            &self,
            offset: usize,
            len: usize,
            buffer: &[u8],
            data: spin::MutexGuard<crate::fs::vfs::FilePrivateData>,
        ) -> Result<usize, crate::utils::error::SyscallErr> {
            self.write_at(offset, len, buffer, data)
        }

        fn write_sync(
            &self,
            offset: usize,
            buffer: &[u8],
        ) -> Result<usize, crate::utils::error::SyscallErr> {
            let private = spin::Mutex::new(crate::fs::vfs::FilePrivateData::Unused);
            let written = self.write_at(offset, buffer.len(), buffer, private.lock())?;
            self.sync()?;
            Ok(written)
        }

        fn resize(&self, len: usize) -> Result<(), crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            match self.file_type {
                crate::fs::vfs::FileType::Dir => {
                    return Err(crate::utils::error::SyscallErr::EISDIR)
                }
                crate::fs::vfs::FileType::File => {}
                _ => return Err(crate::utils::error::SyscallErr::EINVAL),
            }
            let cache = self.page_cache();
            if let Some(cache) = cache.as_ref() {
                cache.writeback_all()?;
            }
            let inode_id = u32::try_from(self.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            fs.inner()
                .commit_inode_size(inode_id, len as u64, None)
                .map_err(|error| super::errno::from_another(error.code()))?;
            if let Some(cache) = cache {
                cache.truncate(len)?;
            }
            self.lifetime
                .logical_size
                .store(len, core::sync::atomic::Ordering::Release);
            Ok(())
        }

        fn sync(&self) -> Result<(), crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            if let Some(cache) = self.page_cache() {
                cache.writeback_all()?;
            }
            let id = u32::try_from(self.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            let generation = self
                .lifetime
                .size_generation
                .load(core::sync::atomic::Ordering::Acquire);
            let size = self
                .lifetime
                .logical_size
                .load(core::sync::atomic::Ordering::Acquire);
            fs.inner()
                .commit_inode_size(id, size as u64, None)
                .map_err(|error| super::errno::from_another(error.code()))?;
            let _ = self.lifetime.size_generation.compare_exchange(
                generation,
                0,
                core::sync::atomic::Ordering::AcqRel,
                core::sync::atomic::Ordering::Acquire,
            );
            fs.flush_device()
        }

        fn datasync(&self) -> Result<(), crate::utils::error::SyscallErr> {
            self.sync()
        }

        fn discard_write_at(
            &self,
            _offset: usize,
            _len: usize,
            _data: spin::MutexGuard<crate::fs::vfs::FilePrivateData>,
        ) -> Result<usize, crate::utils::error::SyscallErr> {
            let _fs = self.fs_arc()?;
            Err(crate::utils::error::SyscallErr::EOPNOTSUPP)
        }
    };
}

pub(crate) use writable_data_inode_mutations;
