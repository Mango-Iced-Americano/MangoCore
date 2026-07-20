use alloc::sync::Arc;
use core::convert::TryFrom;

use crate::config::PAGE_SIZE;
use crate::fs::vfs::{FilePrivateData, FileType};
use crate::utils::error::SyscallErr;

use super::fixtures::{open_clean_media, ZeroBlockDevice};

pub(super) fn test_clean_media_supports_metadata_lookup_and_page_reads() -> Result<(), &'static str>
{
    let fs = open_clean_media()?;
    let root = fs.root_inode();
    if root
        .metadata()
        .map_err(|_| "root metadata failed")?
        .file_type
        != FileType::Dir
    {
        return Err("ext4 root is not a directory");
    }
    let dot = root.find(".").map_err(|_| "root lookup for . failed")?;
    if dot.metadata().map_err(|_| "dot metadata failed")?.inode_id
        != root
            .metadata()
            .map_err(|_| "root metadata changed")?
            .inode_id
    {
        return Err("root lookup for . changed the inode id");
    }
    let entries = root
        .list_dirents()
        .map_err(|_| "root directory listing failed")?;
    for (name, _, file_type) in entries {
        if file_type != FileType::File {
            continue;
        }
        let file = root
            .find(&name)
            .map_err(|_| "directory entry lookup failed")?;
        let size = usize::try_from(file.metadata().map_err(|_| "file metadata failed")?.size)
            .map_err(|_| "file size does not fit usize")?;
        let cache = file
            .ensure_page_cache()
            .ok_or("regular file has no PageCache")?;
        let mut hole = [0xA5; PAGE_SIZE];
        cache
            .read(
                size.div_ceil(PAGE_SIZE).saturating_mul(PAGE_SIZE),
                &mut hole,
            )
            .map_err(|_| "PageCache backend failed to zero-fill an EOF hole")?;
        if hole.iter().any(|byte| *byte != 0) {
            return Err("PageCache EOF hole was not zero-filled");
        }
        if size > PAGE_SIZE {
            let mut cross_page = [0u8; 2];
            let private = spin::Mutex::new(FilePrivateData::Unused);
            let read = file
                .read_at(
                    PAGE_SIZE - 1,
                    cross_page.len(),
                    &mut cross_page,
                    private.lock(),
                )
                .map_err(|_| "cross-page regular-file read failed")?;
            if read != cross_page.len() {
                return Err("cross-page regular-file read was short");
            }
        }
        return Ok(());
    }
    Err("clean ext4 test image has no regular file at its root")
}

pub(super) fn test_rejects_unreliable_flush_before_media_parse() -> Result<(), &'static str> {
    let result = crate::fs::ext4_backend::open(Arc::new(ZeroBlockDevice));
    match result {
        Err(SyscallErr::EROFS) => Ok(()),
        Err(_) => Err("another_ext4 parsed media before rejecting unreliable flush"),
        Ok(_) => Err("another_ext4 mounted a device without reliable flush"),
    }
}
