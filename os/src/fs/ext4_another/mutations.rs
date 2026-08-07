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
            let old_size = self
                .lifetime
                .logical_size
                .load(core::sync::atomic::Ordering::Acquire);
            let cache = self.regular_page_cache(&fs)?;
            let written = cache.write_kernel(
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
            self.lifetime.retain_dirty_page_cache(&cache);
            Ok(written)
        }

        fn write_at_user(
            &self,
            offset: usize,
            len: usize,
            source: &crate::mm::UserBuffer,
        ) -> Result<usize, crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            match self.file_type {
                crate::fs::vfs::FileType::Dir => {
                    return Err(crate::utils::error::SyscallErr::EISDIR)
                }
                crate::fs::vfs::FileType::File => {}
                _ => return Err(crate::utils::error::SyscallErr::EINVAL),
            }
            let actual = len.min(source.len());
            if actual == 0 {
                return Ok(0);
            }
            let cache = self.regular_page_cache(&fs)?;
            let old_size = self
                .lifetime
                .logical_size
                .load(core::sync::atomic::Ordering::Acquire);
            let written = cache.write_at_user(offset, actual, source, old_size)?;
            let end = offset
                .checked_add(written)
                .ok_or(crate::utils::error::SyscallErr::EFBIG)?;
            self.lifetime
                .logical_size
                .fetch_max(end, core::sync::atomic::Ordering::AcqRel);
            self.lifetime
                .size_generation
                .fetch_add(1, core::sync::atomic::Ordering::AcqRel);
            self.lifetime.retain_dirty_page_cache(&cache);
            Ok(written)
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
            if let Some(cache) = self.page_cache() {
                cache.writeback_all_before_io_gate()?;
                cache.with_io_gate(|| {
                    if len < old_size {
                        cache.truncate_with_io_gate_held_and_backend(len, || {
                            fs.inner()
                                .truncate_inode(inode_id, len as u64)
                                .map_err(|error| {
                                    super::errno::from_another_op(&error, "truncate_inode(shrink)")
                                })
                        })?;
                    } else {
                        fs.inner()
                            .truncate_inode(inode_id, len as u64)
                            .map_err(|error| {
                                super::errno::from_another_op(&error, "truncate_inode(extend)")
                            })?;
                    }
                    Ok::<(), crate::utils::error::SyscallErr>(())
                })?;
            } else {
                fs.inner()
                    .truncate_inode(inode_id, len as u64)
                    .map_err(|error| super::errno::from_another(error.code()))?;
            }
            self.lifetime
                .logical_size
                .store(len, core::sync::atomic::Ordering::Release);
            self.lifetime
                .size_generation
                .fetch_add(1, core::sync::atomic::Ordering::AcqRel);
            Ok(())
        }

        fn sync(&self) -> Result<(), crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            if let Some(cache) = self.page_cache() {
                cache.writeback_all_before_io_gate()?;
                return cache.with_io_gate(|| {
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
                    let timestamps = fs.commit_lifetime_timestamps(id, &self.lifetime)?;
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
                    if let Some(timestamps) = timestamps {
                        self.lifetime.finish_timestamp_commit(timestamps);
                    }
                    Ok(())
                });
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
            let timestamps = fs.commit_lifetime_timestamps(id, &self.lifetime)?;
            let generation_committed = self.lifetime.size_generation.compare_exchange(
                generation,
                0,
                core::sync::atomic::Ordering::AcqRel,
                core::sync::atomic::Ordering::Acquire,
            ).is_ok();
            fs.flush_device()?;
            if generation_committed {
                self.lifetime.release_dirty_page_cache();
            }
            if let Some(timestamps) = timestamps {
                self.lifetime.finish_timestamp_commit(timestamps);
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
