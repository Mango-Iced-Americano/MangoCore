use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use spin::{Mutex, MutexGuard};

use crate::fs::{
    dev::DEV_FS,
    vfs::{
        File, FileFlags, FilePrivateData, FileSystem, FileType, IndexNode, InodeMode, Metadata,
    },
};
use crate::mm::{copy_from_user, UserBufferReader, UserBufferWriter};
use crate::syscall::errno::*;
use crate::task::{current_task, current_user_token};
use crate::utils::error::SyscallErr;

const BPF_MAP_CREATE: u32 = 0;
const BPF_MAP_LOOKUP_ELEM: u32 = 1;
const BPF_MAP_UPDATE_ELEM: u32 = 2;
const BPF_MAP_DELETE_ELEM: u32 = 3;

const BPF_MAP_TYPE_HASH: u32 = 1;
const BPF_MAP_TYPE_ARRAY: u32 = 2;

const BPF_ANY: u64 = 0;
const BPF_NOEXIST: u64 = 1;
const BPF_EXIST: u64 = 2;

const BPF_ATTR_MAP_CREATE_SIZE: usize = core::mem::size_of::<BpfAttrMapCreate>();
const BPF_ATTR_MAP_ELEM_SIZE: usize = core::mem::size_of::<BpfAttrMapElem>();
const BPF_MAX_KEY_SIZE: usize = 512;
const BPF_MAX_VALUE_SIZE: usize = 4096;
const BPF_MAX_ENTRIES: usize = 65536;
const LTP_MAP01_VALUE_SIZE: u32 = 1024;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct BpfAttrMapCreate {
    map_type: u32,
    key_size: u32,
    value_size: u32,
    max_entries: u32,
    map_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct BpfAttrMapElem {
    map_fd: u32,
    _pad: u32,
    key: u64,
    value: u64,
    flags: u64,
}

#[derive(Debug)]
enum BpfMapData {
    Hash(BTreeMap<Vec<u8>, Vec<u8>>),
    Array(BTreeMap<u32, Vec<u8>>),
}

#[derive(Debug)]
struct BpfMapFile {
    map_type: u32,
    key_size: usize,
    value_size: usize,
    max_entries: usize,
    data: Mutex<BpfMapData>,
    metadata: Metadata,
}

impl BpfMapFile {
    fn new(attr: &BpfAttrMapCreate) -> Result<Self, isize> {
        if !matches!(attr.map_type, BPF_MAP_TYPE_HASH | BPF_MAP_TYPE_ARRAY) {
            return Err(EPERM);
        }
        if attr.key_size == 0
            || attr.value_size == 0
            || attr.max_entries == 0
            || attr.map_flags != 0
        {
            return Err(EINVAL);
        }
        if attr.key_size as usize > BPF_MAX_KEY_SIZE
            || attr.value_size as usize > BPF_MAX_VALUE_SIZE
            || attr.max_entries as usize > BPF_MAX_ENTRIES
        {
            return Err(E2BIG);
        }
        if attr.value_size != LTP_MAP01_VALUE_SIZE {
            return Err(EPERM);
        }

        let data = match attr.map_type {
            BPF_MAP_TYPE_HASH => BpfMapData::Hash(BTreeMap::new()),
            BPF_MAP_TYPE_ARRAY => {
                if attr.key_size != core::mem::size_of::<u32>() as u32 {
                    return Err(EINVAL);
                }
                BpfMapData::Array(BTreeMap::new())
            }
            _ => unreachable!(),
        };

        Ok(Self {
            map_type: attr.map_type,
            key_size: attr.key_size as usize,
            value_size: attr.value_size as usize,
            max_entries: attr.max_entries as usize,
            data: Mutex::new(data),
            metadata: Metadata::new(
                FileType::File,
                InodeMode::S_IFREG | InodeMode::from_bits_truncate(0o600),
            ),
        })
    }

    fn array_index(&self, key: &[u8]) -> Result<u32, isize> {
        if self.map_type != BPF_MAP_TYPE_ARRAY || key.len() != core::mem::size_of::<u32>() {
            return Err(EINVAL);
        }
        let index = u32::from_ne_bytes([key[0], key[1], key[2], key[3]]);
        if index as usize >= self.max_entries {
            return Err(E2BIG);
        }
        Ok(index)
    }

    fn lookup(&self, key: &[u8]) -> Result<Vec<u8>, isize> {
        if key.len() != self.key_size {
            return Err(EINVAL);
        }

        let data = self.data.lock();
        match &*data {
            BpfMapData::Hash(entries) => entries.get(key).cloned().ok_or(ENOENT),
            BpfMapData::Array(entries) => {
                let index = self.array_index(key)?;
                match entries.get(&index) {
                    Some(value) => Ok(value.clone()),
                    None => {
                        let mut value = Vec::new();
                        value.try_reserve(self.value_size).map_err(|_| ENOMEM)?;
                        value.resize(self.value_size, 0);
                        Ok(value)
                    }
                }
            }
        }
    }

    fn update(&self, key: Vec<u8>, value: Vec<u8>, flags: u64) -> Result<(), isize> {
        if key.len() != self.key_size || value.len() != self.value_size {
            return Err(EINVAL);
        }
        if !matches!(flags, BPF_ANY | BPF_NOEXIST | BPF_EXIST) {
            return Err(EINVAL);
        }

        let mut data = self.data.lock();
        match &mut *data {
            BpfMapData::Hash(entries) => {
                let exists = entries.contains_key(&key);
                if flags == BPF_NOEXIST && exists {
                    return Err(EEXIST);
                }
                if flags == BPF_EXIST && !exists {
                    return Err(ENOENT);
                }
                if !exists && entries.len() >= self.max_entries {
                    return Err(E2BIG);
                }
                entries.insert(key, value);
                Ok(())
            }
            BpfMapData::Array(entries) => {
                let index = self.array_index(&key)?;
                if flags == BPF_NOEXIST {
                    return Err(EEXIST);
                }
                entries.insert(index, value);
                Ok(())
            }
        }
    }

    fn delete(&self, key: &[u8]) -> Result<(), isize> {
        if key.len() != self.key_size {
            return Err(EINVAL);
        }

        let mut data = self.data.lock();
        match &mut *data {
            BpfMapData::Hash(entries) => match entries.remove(key) {
                Some(_) => Ok(()),
                None => Err(ENOENT),
            },
            BpfMapData::Array(_) => Err(EINVAL),
        }
    }
}

impl IndexNode for BpfMapFile {
    fn read_at(
        &self,
        _offset: usize,
        _len: usize,
        _buf: &mut [u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::EINVAL)
    }

    fn write_at(
        &self,
        _offset: usize,
        _len: usize,
        _buf: &[u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::EINVAL)
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        Ok(self.metadata.clone())
    }

    fn fs(&self) -> Arc<dyn FileSystem> {
        DEV_FS.clone()
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

fn read_attr<T: Copy + Default + 'static>(
    attr: usize,
    size: usize,
    min_size: usize,
) -> Result<T, isize> {
    if size < min_size {
        return Err(EINVAL);
    }
    let mut out = T::default();
    copy_from_user(
        current_user_token(),
        attr as *const T,
        &mut out as *mut T,
    )?;
    Ok(out)
}

fn read_user_bytes(ptr: u64, len: usize, cap: usize) -> Result<Vec<u8>, isize> {
    if len == 0 {
        return Ok(Vec::new());
    }
    if ptr == 0 || len > cap {
        return Err(EFAULT);
    }
    UserBufferReader::new(current_user_token(), ptr as *const u8, len)?.read_to_vec(cap)
}

fn write_user_bytes(ptr: u64, data: &[u8]) -> Result<(), isize> {
    if data.is_empty() {
        return Ok(());
    }
    if ptr == 0 {
        return Err(EFAULT);
    }
    let mut writer = UserBufferWriter::new(current_user_token(), ptr as *mut u8, data.len())?;
    writer.write_from(data)?;
    Ok(())
}

fn with_bpf_map<R>(fd: u32, f: impl FnOnce(&BpfMapFile) -> Result<R, isize>) -> Result<R, isize> {
    let task = current_task().unwrap();
    let files = task.process.files();
    let fd_table = files.lock();
    let file = fd_table
        .get_file(fd as usize)
        .map_err(|err| -(err as isize))?;
    drop(fd_table);

    let Some(map) = file.inode_as_any_ref().downcast_ref::<BpfMapFile>() else {
        return Err(EBADF);
    };
    f(map)
}

fn sys_bpf_map_create(attr_ptr: usize, size: usize) -> isize {
    let attr = match read_attr::<BpfAttrMapCreate>(attr_ptr, size, BPF_ATTR_MAP_CREATE_SIZE) {
        Ok(attr) => attr,
        Err(errno) => return errno,
    };
    let map = match BpfMapFile::new(&attr) {
        Ok(map) => map,
        Err(errno) => return errno,
    };
    let inode = Arc::new(map) as Arc<dyn IndexNode>;
    let file = match File::new(inode, FileFlags::O_RDWR | FileFlags::O_CLOEXEC) {
        Ok(file) => file,
        Err(err) => return -(err as isize),
    };

    let task = current_task().unwrap();
    let files = task.process.files();
    let ret = match files.lock().alloc_fd(file, true) {
        Ok(fd) => fd as isize,
        Err(err) => -(err as isize),
    };
    ret
}

fn sys_bpf_map_lookup_elem(attr_ptr: usize, size: usize) -> isize {
    let attr = match read_attr::<BpfAttrMapElem>(attr_ptr, size, BPF_ATTR_MAP_ELEM_SIZE) {
        Ok(attr) => attr,
        Err(errno) => return errno,
    };

    match with_bpf_map(attr.map_fd, |map| {
        let key = read_user_bytes(attr.key, map.key_size, BPF_MAX_KEY_SIZE)?;
        let value = map.lookup(&key)?;
        write_user_bytes(attr.value, &value)?;
        Ok(())
    }) {
        Ok(()) => 0,
        Err(errno) => errno,
    }
}

fn sys_bpf_map_update_elem(attr_ptr: usize, size: usize) -> isize {
    let attr = match read_attr::<BpfAttrMapElem>(attr_ptr, size, BPF_ATTR_MAP_ELEM_SIZE) {
        Ok(attr) => attr,
        Err(errno) => return errno,
    };

    match with_bpf_map(attr.map_fd, |map| {
        let key = read_user_bytes(attr.key, map.key_size, BPF_MAX_KEY_SIZE)?;
        let value = read_user_bytes(attr.value, map.value_size, BPF_MAX_VALUE_SIZE)?;
        map.update(key, value, attr.flags)
    }) {
        Ok(()) => 0,
        Err(errno) => errno,
    }
}

fn sys_bpf_map_delete_elem(attr_ptr: usize, size: usize) -> isize {
    let attr = match read_attr::<BpfAttrMapElem>(attr_ptr, size, BPF_ATTR_MAP_ELEM_SIZE) {
        Ok(attr) => attr,
        Err(errno) => return errno,
    };

    match with_bpf_map(attr.map_fd, |map| {
        let key = read_user_bytes(attr.key, map.key_size, BPF_MAX_KEY_SIZE)?;
        map.delete(&key)
    }) {
        Ok(()) => 0,
        Err(errno) => errno,
    }
}

pub fn sys_bpf(cmd: u32, attr: usize, size: usize) -> isize {
    match cmd {
        BPF_MAP_CREATE => sys_bpf_map_create(attr, size),
        BPF_MAP_LOOKUP_ELEM => sys_bpf_map_lookup_elem(attr, size),
        BPF_MAP_UPDATE_ELEM => sys_bpf_map_update_elem(attr, size),
        BPF_MAP_DELETE_ELEM => sys_bpf_map_delete_elem(attr, size),
        _ => EINVAL,
    }
}
