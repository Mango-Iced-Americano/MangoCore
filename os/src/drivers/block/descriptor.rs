use super::BlockDevice;
use alloc::string::String;
use alloc::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct BlockDeviceNumber {
    major: u64,
    minor: u64,
}

impl BlockDeviceNumber {
    pub const fn new(major: u64, minor: u64) -> Self {
        Self { major, minor }
    }

    pub const fn major(self) -> u64 {
        self.major
    }

    pub const fn minor(self) -> u64 {
        self.minor
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BlockDeviceNameError {
    Empty,
    Reserved,
    InvalidCharacter,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct BlockDeviceName(String);

impl BlockDeviceName {
    pub fn new(name: &str) -> Result<Self, BlockDeviceNameError> {
        if name.is_empty() {
            return Err(BlockDeviceNameError::Empty);
        }
        if name == "." || name == ".." {
            return Err(BlockDeviceNameError::Reserved);
        }
        if name.bytes().any(|byte| byte == b'/' || byte == 0) {
            return Err(BlockDeviceNameError::InvalidCharacter);
        }
        Ok(Self(String::from(name)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockDeviceNode {
    name: BlockDeviceName,
    number: BlockDeviceNumber,
}

impl BlockDeviceNode {
    pub fn new(name: &str, number: BlockDeviceNumber) -> Result<Self, BlockDeviceNameError> {
        Ok(Self {
            name: BlockDeviceName::new(name)?,
            number,
        })
    }

    pub fn name(&self) -> &BlockDeviceName {
        &self.name
    }

    pub const fn number(&self) -> BlockDeviceNumber {
        self.number
    }
}

pub struct BlockDeviceDescriptor {
    node: BlockDeviceNode,
    device: Arc<dyn BlockDevice>,
}

impl Clone for BlockDeviceDescriptor {
    fn clone(&self) -> Self {
        Self {
            node: self.node.clone(),
            device: self.device.clone(),
        }
    }
}

impl BlockDeviceDescriptor {
    pub fn new(node: BlockDeviceNode, device: Arc<dyn BlockDevice>) -> Self {
        Self { node, device }
    }

    pub fn node(&self) -> &BlockDeviceNode {
        &self.node
    }

    pub fn device(&self) -> &Arc<dyn BlockDevice> {
        &self.device
    }
}
