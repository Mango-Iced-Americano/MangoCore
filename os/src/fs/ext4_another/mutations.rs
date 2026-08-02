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
            let t0 = crate::task::perf::perf_memory_io_time_now();
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
            let cache = self.regular_page_cache()?;
            let t1 = crate::task::perf::perf_memory_io_time_now();
            let written =
                cache.write_with_after_copy(offset, &buffer[..actual], Some(old_size), |_| {
                    let post_start = crate::task::perf::perf_memory_io_time_now();
                    self.lifetime
                        .logical_size
                        .fetch_max(end, core::sync::atomic::Ordering::AcqRel);
                    self.lifetime
                        .size_generation
                        .fetch_add(1, core::sync::atomic::Ordering::AcqRel);
                    self.lifetime.retain_dirty_page_cache(cache);
                    crate::task::perf::record_pwrite_ext4_post(
                        crate::task::perf::perf_memory_io_time_now().wrapping_sub(post_start),
                    );
                })?;
            crate::task::perf::record_pwrite_ext4_setup(t1.wrapping_sub(t0));
            Ok(written)
        }

        fn write_at_user(
            &self,
            offset: usize,
            len: usize,
            source: &crate::mm::UserBuffer,
        ) -> Result<usize, crate::utils::error::SyscallErr> {
            let t0 = crate::task::perf::perf_memory_io_time_now();
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
            let cache = self.regular_page_cache()?;
            let old_size = self
                .lifetime
                .logical_size
                .load(core::sync::atomic::Ordering::Acquire);
            let t1 = crate::task::perf::perf_memory_io_time_now();
            let written = cache.write_user(offset, actual, source, Some(old_size))?;
            let t2 = crate::task::perf::perf_memory_io_time_now();
            let end = offset
                .checked_add(written)
                .ok_or(crate::utils::error::SyscallErr::EFBIG)?;
            self.lifetime
                .logical_size
                .fetch_max(end, core::sync::atomic::Ordering::AcqRel);
            self.lifetime
                .size_generation
                .fetch_add(1, core::sync::atomic::Ordering::AcqRel);
            self.lifetime.retain_dirty_page_cache(cache);
            let t3 = crate::task::perf::perf_memory_io_time_now();
            crate::task::perf::record_pwrite_ext4_setup(t1.wrapping_sub(t0));
            crate::task::perf::record_pwrite_ext4_post(t3.wrapping_sub(t2));
            Ok(written)
        }

        fn write_direct(
            &self,
            offset: usize,
            len: usize,
            buffer: &[u8],
            data: spin::MutexGuard<crate::fs::vfs::FilePrivateData>,
        ) -> Result<usize, crate::utils::error::SyscallErr> {
            drop(data);
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
            self.write_at(
                offset,
                actual,
                &buffer[..actual],
                spin::Mutex::new(crate::fs::vfs::FilePrivateData::Unused).lock(),
            )
        }

        fn write_sync(
            &self,
            offset: usize,
            buffer: &[u8],
        ) -> Result<usize, crate::utils::error::SyscallErr> {
            let private = spin::Mutex::new(crate::fs::vfs::FilePrivateData::Unused);
            let written = self.write_direct(offset, buffer.len(), buffer, private.lock())?;
            self.sync()?;
            Ok(written)
        }

        fn resize(&self, len: usize) -> Result<(), crate::utils::error::SyscallErr> {
            match self.file_type {
                crate::fs::vfs::FileType::Dir => {
                    return Err(crate::utils::error::SyscallErr::EISDIR)
                }
                crate::fs::vfs::FileType::File => {}
                _ => return Err(crate::utils::error::SyscallErr::EINVAL),
            }
            let cache = self.regular_page_cache()?;
            let fs = self.fs_arc()?;
            let inode_id = u32::try_from(self.key.inode_id())
                .map_err(|_| crate::utils::error::SyscallErr::EFBIG)?;
            let old_size = self
                .lifetime
                .logical_size
                .load(core::sync::atomic::Ordering::Acquire);
            if len < old_size {
                cache.writeback_all()?;
                cache.truncate(len)?;
                cache.writeback_all()?;
                fs.inner()
                    .truncate_inode(inode_id, len as u64)
                    .map_err(|error| {
                        super::errno::from_another_op(&error, "truncate_inode(shrink)")
                    })?;
                self.lifetime
                    .logical_size
                    .store(len, core::sync::atomic::Ordering::Release);
                self.lifetime
                    .size_generation
                    .fetch_add(1, core::sync::atomic::Ordering::AcqRel);
            } else {
                fs.inner()
                    .truncate_inode(inode_id, len as u64)
                    .map_err(|error| {
                        super::errno::from_another_op(&error, "truncate_inode(extend)")
                    })?;
                self.lifetime
                    .logical_size
                    .store(len, core::sync::atomic::Ordering::Release);
                self.lifetime
                    .size_generation
                    .fetch_add(1, core::sync::atomic::Ordering::AcqRel);
            }
            Ok(())
        }

        fn sync(&self) -> Result<(), crate::utils::error::SyscallErr> {
            let fs = self.fs_arc()?;
            if let Some(cache) = self.page_cache() {
                return (|| {
                    let generation = self
                        .lifetime
                        .size_generation
                        .load(core::sync::atomic::Ordering::Acquire);
                    cache.writeback_all()?;
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
                })();
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
