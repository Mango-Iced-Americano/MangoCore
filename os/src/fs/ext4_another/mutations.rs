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
            let cache = self.regular_page_cache(&fs);
            let written = cache.write_with_after_copy(
                offset,
                &buffer[..actual],
                Some(old_size),
                |_| {
                    self.lifetime
                        .logical_size
                        .fetch_max(end, core::sync::atomic::Ordering::AcqRel);
                    self.lifetime
                        .size_generation
                        .fetch_add(1, core::sync::atomic::Ordering::AcqRel);
                    self.lifetime.retain_dirty_page_cache(&cache);
                },
            )?;
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
            let copied = source.read(&mut buffer);
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
            let inode_id = u32::try_from(self.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            let old_size = self
                .lifetime
                .logical_size
                .load(core::sync::atomic::Ordering::Acquire);
            let cache = self.regular_page_cache(&fs);
            if len < old_size {
                cache.with_io_gate(|| {
                    cache.writeback_all_with_io_gate_held()?;
                    cache.truncate_with_io_gate_held(len)?;
                    cache.writeback_all_with_io_gate_held()?;
                    fs.inner()
                        .truncate_inode(inode_id, len as u64)
                        .map_err(|error| super::errno::from_another(error.code()))?;
                    self.lifetime
                        .logical_size
                        .store(len, core::sync::atomic::Ordering::Release);
                    self.lifetime
                        .size_generation
                        .fetch_add(1, core::sync::atomic::Ordering::AcqRel);
                    Ok(())
                })?;
            } else {
                cache.with_io_gate(|| {
                    fs.inner()
                        .truncate_inode(inode_id, len as u64)
                        .map_err(|error| super::errno::from_another(error.code()))?;
                    self.lifetime
                        .logical_size
                        .store(len, core::sync::atomic::Ordering::Release);
                    self.lifetime
                        .size_generation
                        .fetch_add(1, core::sync::atomic::Ordering::AcqRel);
                    Ok(())
                })?;
            }
            Ok(())
        }

        fn sync(&self) -> Result<(), crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            if let Some(cache) = self.page_cache() {
                return cache.with_io_gate(|| {
                    let generation = self
                        .lifetime
                        .size_generation
                        .load(core::sync::atomic::Ordering::Acquire);
                    cache.writeback_all_with_io_gate_held()?;
                    let id = u32::try_from(self.key.inode_id())
                        .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
                    let size = self
                        .lifetime
                        .logical_size
                        .load(core::sync::atomic::Ordering::Acquire);
                    fs.inner()
                        .commit_inode_size(id, size as u64, None)
                        .map_err(|error| super::errno::from_another(error.code()))?;
                    fs.flush_device()?;
                    if self
                        .lifetime
                        .size_generation
                        .compare_exchange(
                            generation,
                            0,
                            core::sync::atomic::Ordering::AcqRel,
                            core::sync::atomic::Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        self.lifetime.release_dirty_page_cache();
                    }
                    Ok(())
                });
            }
            let generation = self
                .lifetime
                .size_generation
                .load(core::sync::atomic::Ordering::Acquire);
            let id = u32::try_from(self.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            let size = self
                .lifetime
                .logical_size
                .load(core::sync::atomic::Ordering::Acquire);
            fs.inner()
                .commit_inode_size(id, size as u64, None)
                .map_err(|error| super::errno::from_another(error.code()))?;
            fs.flush_device()?;
            if self
                .lifetime
                .size_generation
                .compare_exchange(
                    generation,
                    0,
                    core::sync::atomic::Ordering::AcqRel,
                    core::sync::atomic::Ordering::Acquire,
                )
                .is_ok()
            {
                self.lifetime.release_dirty_page_cache();
            }
            Ok(())
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
