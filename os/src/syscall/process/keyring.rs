use crate::mm::{translated_str, UserBufferReader, UserBufferWriter};
use crate::syscall::errno::*;
use crate::task::{current_task, current_user_token};
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

const KEY_SPEC_THREAD_KEYRING: i32 = -1;
const KEY_SPEC_PROCESS_KEYRING: i32 = -2;
const KEY_SPEC_SESSION_KEYRING: i32 = -3;
const KEY_SPEC_USER_KEYRING: i32 = -4;
const KEY_SPEC_USER_SESSION_KEYRING: i32 = -5;

const KEY_REQKEY_DEFL_DEFAULT: i32 = 0;
const KEY_REQKEY_DEFL_THREAD_KEYRING: i32 = 1;
const KEY_REQKEY_DEFL_PROCESS_KEYRING: i32 = 2;
const KEY_REQKEY_DEFL_SESSION_KEYRING: i32 = 3;

const KEYCTL_GET_KEYRING_ID: u32 = 0;
const KEYCTL_JOIN_SESSION_KEYRING: u32 = 1;
const KEYCTL_REVOKE: u32 = 3;
const KEYCTL_SETPERM: u32 = 5;
const KEYCTL_CLEAR: u32 = 7;
const KEYCTL_UNLINK: u32 = 9;
const KEYCTL_READ: u32 = 11;
const KEYCTL_SET_REQKEY_KEYRING: u32 = 14;
const KEYCTL_SET_TIMEOUT: u32 = 15;

const KEY_POS_WRITE: u32 = 0x0400_0000;
const KEY_POS_ALL: u32 = 0x3f00_0000;
const KEY_USR_ALL: u32 = 0x003f_0000;
const KEY_GRP_ALL: u32 = 0x0000_3f00;
const KEY_OTH_ALL: u32 = 0x0000_003f;
const KEY_DEFAULT_PERM: u32 = KEY_POS_ALL | KEY_USR_ALL | KEY_GRP_ALL | KEY_OTH_ALL;

const USER_KEY_MAX: usize = 32767;
const BIG_KEY_MAX: usize = (1 << 20) - 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KeyKind {
    Keyring,
    User,
    Logon,
    BigKey,
}

impl KeyKind {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "keyring" => Some(Self::Keyring),
            "user" => Some(Self::User),
            "logon" => Some(Self::Logon),
            "big_key" => Some(Self::BigKey),
            _ => None,
        }
    }

    fn payload_limit(self) -> usize {
        match self {
            Self::Keyring => 0,
            Self::User | Self::Logon => USER_KEY_MAX,
            Self::BigKey => BIG_KEY_MAX,
        }
    }
}

struct Key {
    kind: KeyKind,
    desc: String,
    payload: Vec<u8>,
    revoked: bool,
    expired: bool,
    negative: bool,
    perm: u32,
    children: Vec<i32>,
}

impl Key {
    fn new(kind: KeyKind, desc: String, payload: Vec<u8>) -> Self {
        Self {
            kind,
            desc,
            payload,
            revoked: false,
            expired: false,
            negative: false,
            perm: KEY_DEFAULT_PERM,
            children: Vec::new(),
        }
    }
}

#[derive(Clone, Copy)]
struct ProcessKeyrings {
    thread: i32,
    process: i32,
    session: i32,
    reqkey_default: i32,
}

struct KeyRegistry {
    next_id: i32,
    keys: BTreeMap<i32, Key>,
    processes: BTreeMap<usize, ProcessKeyrings>,
    user_keyrings: BTreeMap<u32, i32>,
    user_session_keyrings: BTreeMap<u32, i32>,
}

impl KeyRegistry {
    fn new() -> Self {
        Self {
            next_id: 1,
            keys: BTreeMap::new(),
            processes: BTreeMap::new(),
            user_keyrings: BTreeMap::new(),
            user_session_keyrings: BTreeMap::new(),
        }
    }

    fn alloc_key(&mut self, kind: KeyKind, desc: String, payload: Vec<u8>) -> i32 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1).max(1);
        self.keys.insert(id, Key::new(kind, desc, payload));
        id
    }

    fn alloc_keyring(&mut self, desc: String) -> i32 {
        self.alloc_key(KeyKind::Keyring, desc, Vec::new())
    }

    fn ensure_process_keyrings(&mut self, pid: usize) -> ProcessKeyrings {
        if let Some(rings) = self.processes.get(&pid) {
            return *rings;
        }
        let rings = ProcessKeyrings {
            thread: self.alloc_keyring(format_keyring_name("thread", pid)),
            process: self.alloc_keyring(format_keyring_name("process", pid)),
            session: self.alloc_keyring(format_keyring_name("session", pid)),
            reqkey_default: KEY_REQKEY_DEFL_DEFAULT,
        };
        self.processes.insert(pid, rings);
        rings
    }

    fn ensure_user_keyring(&mut self, euid: u32) -> i32 {
        if let Some(id) = self.user_keyrings.get(&euid) {
            return *id;
        }
        let id = self.alloc_keyring(format_keyring_name("user", euid as usize));
        self.user_keyrings.insert(euid, id);
        id
    }

    fn ensure_user_session_keyring(&mut self, euid: u32) -> i32 {
        if let Some(id) = self.user_session_keyrings.get(&euid) {
            return *id;
        }
        let id = self.alloc_keyring(format_keyring_name("user_session", euid as usize));
        self.user_session_keyrings.insert(euid, id);
        id
    }

    fn resolve_special_keyring(&mut self, id: i32, pid: usize, euid: u32) -> Option<i32> {
        let rings = self.ensure_process_keyrings(pid);
        match id {
            KEY_SPEC_THREAD_KEYRING => Some(rings.thread),
            KEY_SPEC_PROCESS_KEYRING => Some(rings.process),
            KEY_SPEC_SESSION_KEYRING => Some(rings.session),
            KEY_SPEC_USER_KEYRING => Some(self.ensure_user_keyring(euid)),
            KEY_SPEC_USER_SESSION_KEYRING => Some(self.ensure_user_session_keyring(euid)),
            _ => None,
        }
    }

    fn resolve_keyring(&mut self, id: i32, pid: usize, euid: u32) -> Result<i32, isize> {
        if let Some(id) = self.resolve_special_keyring(id, pid, euid) {
            return Ok(id);
        }
        match self.keys.get(&id) {
            Some(key) if key.kind == KeyKind::Keyring => Ok(id),
            _ => Err(ENOKEY),
        }
    }

    fn resolve_key(&mut self, id: i32, pid: usize, euid: u32) -> Result<i32, isize> {
        if let Some(id) = self.resolve_special_keyring(id, pid, euid) {
            return Ok(id);
        }
        if self.keys.contains_key(&id) {
            Ok(id)
        } else {
            Err(ENOKEY)
        }
    }

    fn request_destination(&mut self, id: i32, pid: usize, euid: u32) -> Result<i32, isize> {
        let rings = self.ensure_process_keyrings(pid);
        match id {
            KEY_REQKEY_DEFL_DEFAULT => match rings.reqkey_default {
                KEY_REQKEY_DEFL_PROCESS_KEYRING => Ok(rings.process),
                KEY_REQKEY_DEFL_SESSION_KEYRING => Ok(rings.session),
                _ => Ok(rings.thread),
            },
            KEY_REQKEY_DEFL_THREAD_KEYRING => Ok(rings.thread),
            KEY_REQKEY_DEFL_PROCESS_KEYRING => Ok(rings.process),
            KEY_REQKEY_DEFL_SESSION_KEYRING => Ok(rings.session),
            _ => self.resolve_keyring(id, pid, euid),
        }
    }

    fn set_request_default(&mut self, pid: usize, default: i32) -> Result<i32, isize> {
        if !matches!(
            default,
            KEY_REQKEY_DEFL_DEFAULT
                | KEY_REQKEY_DEFL_THREAD_KEYRING
                | KEY_REQKEY_DEFL_PROCESS_KEYRING
                | KEY_REQKEY_DEFL_SESSION_KEYRING
        ) {
            return Err(EINVAL);
        }
        let mut rings = self.ensure_process_keyrings(pid);
        let old = rings.reqkey_default;
        rings.reqkey_default = default;
        self.processes.insert(pid, rings);
        Ok(old)
    }

    fn link_key(&mut self, ring_id: i32, key_id: i32) {
        if let Some(ring) = self.keys.get_mut(&ring_id) {
            if ring.kind == KeyKind::Keyring && !ring.children.contains(&key_id) {
                ring.children.push(key_id);
            }
        }
    }

    fn unlink_key(&mut self, ring_id: i32, key_id: i32) -> Result<(), isize> {
        if !self.keys.contains_key(&key_id) {
            return Err(ENOKEY);
        }
        let ring = self.keys.get_mut(&ring_id).ok_or(ENOKEY)?;
        if ring.kind != KeyKind::Keyring {
            return Err(ENOKEY);
        }
        ring.children.retain(|child| *child != key_id);
        Ok(())
    }

    fn clear_keyring(&mut self, ring_id: i32) -> Result<(), isize> {
        let ring = self.keys.get_mut(&ring_id).ok_or(ENOKEY)?;
        if ring.kind != KeyKind::Keyring {
            return Err(ENOKEY);
        }
        ring.children.clear();
        Ok(())
    }

    fn find_key_in_ring(&self, ring_id: i32, kind: KeyKind, desc: &str) -> Option<i32> {
        let ring = self.keys.get(&ring_id)?;
        if ring.kind != KeyKind::Keyring {
            return None;
        }
        ring.children.iter().find_map(|child_id| {
            self.keys.get(child_id).and_then(|key| {
                if key.kind == kind && key.desc == desc {
                    Some(*child_id)
                } else {
                    None
                }
            })
        })
    }

    fn find_key_in_rings(&self, rings: &[i32], kind: KeyKind, desc: &str) -> Option<i32> {
        rings
            .iter()
            .find_map(|ring_id| self.find_key_in_ring(*ring_id, kind, desc))
    }

    fn search_rings(&mut self, pid: usize, euid: u32, destringid: i32) -> Vec<i32> {
        let process_rings = self.ensure_process_keyrings(pid);
        let mut rings = Vec::new();
        rings.push(process_rings.thread);
        rings.push(process_rings.process);
        rings.push(process_rings.session);
        rings.push(self.ensure_user_keyring(euid));
        rings.push(self.ensure_user_session_keyring(euid));
        if let Ok(dest) = self.request_destination(destringid, pid, euid) {
            if !rings.contains(&dest) {
                rings.push(dest);
            }
        }
        rings
    }
}

lazy_static! {
    static ref KEY_REGISTRY: Mutex<KeyRegistry> = Mutex::new(KeyRegistry::new());
}

fn format_keyring_name(prefix: &str, id: usize) -> String {
    let mut name = String::new();
    name.push_str(prefix);
    name.push(':');
    name.push_str(&id.to_string());
    name
}

fn current_key_context() -> (usize, u32) {
    let task = current_task().unwrap();
    let pid = task.pid();
    let euid = task.acquire_inner_lock().euid;
    (pid, euid)
}

fn read_key_string(ptr: *const u8) -> Result<String, isize> {
    translated_str(current_user_token(), ptr)
}

fn read_optional_key_string(ptr: *const u8) -> Result<Option<String>, isize> {
    if ptr.is_null() {
        Ok(None)
    } else {
        read_key_string(ptr).map(Some)
    }
}

fn read_payload(ptr: *const u8, len: usize) -> Result<Vec<u8>, isize> {
    if len == 0 {
        return Ok(Vec::new());
    }
    if ptr.is_null() {
        return Err(EFAULT);
    }
    UserBufferReader::new(current_user_token(), ptr, len)?.read_to_vec(BIG_KEY_MAX + 1)
}

fn validate_payload(kind: KeyKind, payload: *const u8, plen: usize) -> Result<Vec<u8>, isize> {
    if plen > kind.payload_limit() {
        return Err(EINVAL);
    }
    if kind == KeyKind::Keyring && plen != 0 {
        return Err(EINVAL);
    }
    read_payload(payload, plen)
}

fn encode_i32_list(ids: &[i32]) -> Vec<u8> {
    let mut out = Vec::new();
    if out.try_reserve(ids.len() * core::mem::size_of::<i32>()).is_err() {
        return out;
    }
    for id in ids {
        out.extend_from_slice(&id.to_ne_bytes());
    }
    out
}

fn write_key_bytes(buf: usize, buflen: usize, data: &[u8]) -> Result<isize, isize> {
    let copy_len = buflen.min(data.len());
    if copy_len != 0 {
        let mut writer = UserBufferWriter::new(current_user_token(), buf as *mut u8, copy_len)?;
        writer.write_from(&data[..copy_len])?;
    }
    Ok(data.len() as isize)
}

pub fn sys_add_key(
    type_ptr: *const u8,
    desc_ptr: *const u8,
    payload_ptr: *const u8,
    plen: usize,
    ringid: i32,
) -> isize {
    let type_name = match read_key_string(type_ptr) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    let desc = match read_key_string(desc_ptr) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    let kind = match KeyKind::from_name(type_name.as_str()) {
        Some(kind) => kind,
        None => return ENODEV,
    };
    let payload = match validate_payload(kind, payload_ptr, plen) {
        Ok(payload) => payload,
        Err(errno) => return errno,
    };
    let (pid, euid) = current_key_context();
    let mut registry = KEY_REGISTRY.lock();
    let ring_id = match registry.resolve_keyring(ringid, pid, euid) {
        Ok(id) => id,
        Err(errno) => return errno,
    };
    let key_id = match registry.find_key_in_ring(ring_id, kind, desc.as_str()) {
        Some(id) => {
            if let Some(key) = registry.keys.get_mut(&id) {
                key.payload = payload;
                key.revoked = false;
                key.expired = false;
                key.negative = false;
            }
            id
        }
        None => registry.alloc_key(kind, desc, payload),
    };
    registry.link_key(ring_id, key_id);
    key_id as isize
}

pub fn sys_request_key(
    type_ptr: *const u8,
    desc_ptr: *const u8,
    callout_ptr: *const u8,
    destringid: i32,
) -> isize {
    let type_name = match read_key_string(type_ptr) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    let desc = match read_key_string(desc_ptr) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    if let Err(errno) = read_optional_key_string(callout_ptr) {
        return errno;
    }
    let kind = match KeyKind::from_name(type_name.as_str()) {
        Some(kind) => kind,
        None => return ENODEV,
    };

    let (pid, euid) = current_key_context();
    let mut registry = KEY_REGISTRY.lock();
    let search_rings = registry.search_rings(pid, euid, destringid);
    if let Some(id) = registry.find_key_in_rings(&search_rings, kind, desc.as_str()) {
        if let Some(key) = registry.keys.get(&id) {
            if key.revoked {
                return EKEYREVOKED;
            }
            if key.expired {
                return EKEYEXPIRED;
            }
            if key.negative {
                return ENOKEY;
            }
        }
        return id as isize;
    }

    let ring_id = match registry.request_destination(destringid, pid, euid) {
        Ok(id) => id,
        Err(errno) => return errno,
    };
    if registry
        .keys
        .get(&ring_id)
        .map(|ring| ring.perm & KEY_POS_WRITE == 0)
        .unwrap_or(true)
    {
        return EACCES;
    }

    let key_id = registry.alloc_key(kind, desc, Vec::new());
    if let Some(key) = registry.keys.get_mut(&key_id) {
        key.negative = true;
    }
    registry.link_key(ring_id, key_id);
    ENOKEY
}

pub fn sys_keyctl(cmd: u32, arg2: usize, arg3: usize, arg4: usize, _arg5: usize) -> isize {
    let (pid, euid) = current_key_context();
    match cmd {
        KEYCTL_GET_KEYRING_ID => {
            let mut registry = KEY_REGISTRY.lock();
            match registry.resolve_keyring(arg2 as i32, pid, euid) {
                Ok(id) => id as isize,
                Err(errno) => errno,
            }
        }
        KEYCTL_JOIN_SESSION_KEYRING => {
            let name = match read_optional_key_string(arg2 as *const u8) {
                Ok(name) => name,
                Err(errno) => return errno,
            };
            if name.as_deref().map(|s| s.starts_with('.')).unwrap_or(false) {
                return EPERM;
            }
            let mut registry = KEY_REGISTRY.lock();
            let mut rings = registry.ensure_process_keyrings(pid);
            let desc = name.unwrap_or_else(|| format_keyring_name("session", pid));
            rings.session = registry.alloc_keyring(desc);
            registry.processes.insert(pid, rings);
            rings.session as isize
        }
        KEYCTL_REVOKE => {
            let mut registry = KEY_REGISTRY.lock();
            let key_id = match registry.resolve_key(arg2 as i32, pid, euid) {
                Ok(id) => id,
                Err(errno) => return errno,
            };
            if let Some(key) = registry.keys.get_mut(&key_id) {
                key.revoked = true;
                SUCCESS
            } else {
                ENOKEY
            }
        }
        KEYCTL_SETPERM => {
            let mut registry = KEY_REGISTRY.lock();
            let key_id = match registry.resolve_key(arg2 as i32, pid, euid) {
                Ok(id) => id,
                Err(errno) => return errno,
            };
            if let Some(key) = registry.keys.get_mut(&key_id) {
                key.perm = arg3 as u32;
                SUCCESS
            } else {
                ENOKEY
            }
        }
        KEYCTL_CLEAR => {
            let mut registry = KEY_REGISTRY.lock();
            let ring_id = match registry.resolve_keyring(arg2 as i32, pid, euid) {
                Ok(id) => id,
                Err(errno) => return errno,
            };
            registry.clear_keyring(ring_id).map(|_| SUCCESS).unwrap_or_else(|errno| errno)
        }
        KEYCTL_UNLINK => {
            let mut registry = KEY_REGISTRY.lock();
            let ring_id = match registry.resolve_keyring(arg3 as i32, pid, euid) {
                Ok(id) => id,
                Err(errno) => return errno,
            };
            registry
                .unlink_key(ring_id, arg2 as i32)
                .map(|_| SUCCESS)
                .unwrap_or_else(|errno| errno)
        }
        KEYCTL_READ => {
            let data = {
                let mut registry = KEY_REGISTRY.lock();
                let key_id = match registry.resolve_key(arg2 as i32, pid, euid) {
                    Ok(id) => id,
                    Err(errno) => return errno,
                };
                let key = match registry.keys.get(&key_id) {
                    Some(key) => key,
                    None => return ENOKEY,
                };
                if key.revoked {
                    return EKEYREVOKED;
                }
                if key.expired {
                    return EKEYEXPIRED;
                }
                if key.negative {
                    return ENOKEY;
                }
                if key.kind == KeyKind::Keyring {
                    encode_i32_list(&key.children)
                } else {
                    key.payload.clone()
                }
            };
            match write_key_bytes(arg3, arg4, data.as_slice()) {
                Ok(ret) => ret,
                Err(errno) => errno,
            }
        }
        KEYCTL_SET_REQKEY_KEYRING => {
            let mut registry = KEY_REGISTRY.lock();
            match registry.set_request_default(pid, arg2 as i32) {
                Ok(old) => old as isize,
                Err(errno) => errno,
            }
        }
        KEYCTL_SET_TIMEOUT => {
            let mut registry = KEY_REGISTRY.lock();
            let key_id = match registry.resolve_key(arg2 as i32, pid, euid) {
                Ok(id) => id,
                Err(errno) => return errno,
            };
            if let Some(key) = registry.keys.get_mut(&key_id) {
                key.expired = true;
                SUCCESS
            } else {
                ENOKEY
            }
        }
        _ => EOPNOTSUPP,
    }
}
