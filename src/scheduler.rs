use alloy::primitives::{FixedBytes, U256};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleData<T> {
    pub last_checked_block: Option<u64>,
    pub pending: Vec<T>,
}

impl<T> Default for ScheduleData<T> {
    fn default() -> Self {
        Self {
            last_checked_block: None,
            pending: Vec::new(),
        }
    }
}

pub struct ScheduleFile<T> {
    path: PathBuf,
    _marker: std::marker::PhantomData<T>,
}

impl<T> ScheduleFile<T>
where
    T: Serialize + for<'de> Deserialize<'de>,
{
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            _marker: std::marker::PhantomData,
        }
    }

    pub fn load(&self) -> ScheduleData<T> {
        match fs::read_to_string(&self.path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => ScheduleData::default(),
        }
    }

    pub fn save(&self, data: &ScheduleData<T>) {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).expect("Failed to create schedules directory");
        }
        let contents = serde_json::to_string_pretty(data).expect("Failed to serialize schedule");
        fs::write(&self.path, contents).expect("Failed to write schedule file");
    }
}

use alloy::primitives::{Address, Bytes};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbToL1Task {
    pub epoch: u64,
    #[serde(with = "u256_hex")]
    pub position: U256,
    pub execute_after: u64,
    pub l2_sender: Address,
    pub dest_addr: Address,
    pub l2_block: u64,
    pub l1_block: u64,
    pub l2_timestamp: u64,
    #[serde(with = "u256_hex")]
    pub amount: U256,
    #[serde(with = "bytes_hex")]
    pub data: Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmbTask {
    pub epoch: u64,
    pub ticket_id: FixedBytes<32>,
    pub execute_after: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VerificationPhase {
    StartVerification,
    VerifySnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationTask {
    pub epoch: u64,
    pub execute_after: u64,
    pub phase: VerificationPhase,
    pub state_root: FixedBytes<32>,
    pub claimer: Address,
    pub timestamp_claimed: u32,
    pub timestamp_verification: u32,
    pub blocknumber_verification: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimedData {
    pub epoch: u64,
    pub state_root: FixedBytes<32>,
    pub claimer: Address,
    pub timestamp_claimed: u32,
}

mod u256_hex {
    use alloy::primitives::U256;
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &U256, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format!("{:#x}", value))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<U256, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        U256::from_str_radix(s.trim_start_matches("0x"), 16).map_err(serde::de::Error::custom)
    }
}

mod bytes_hex {
    use alloy::hex;
    use alloy::primitives::Bytes;
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &Bytes, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format!("0x{}", hex::encode(value)))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Bytes, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let bytes = hex::decode(s.trim_start_matches("0x")).map_err(serde::de::Error::custom)?;
        Ok(Bytes::from(bytes))
    }
}
