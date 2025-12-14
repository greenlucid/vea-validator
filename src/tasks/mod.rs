pub mod dispatcher;
pub mod save_snapshot;
pub mod claim;
pub mod verify_claim;
pub mod challenge;
pub mod send_snapshot;
pub mod start_verification;
pub mod verify_snapshot;
pub mod execute_relay;

use alloy::primitives::{Address, Bytes, FixedBytes, U256};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Task {
    SaveSnapshot {
        epoch: u64,
        execute_after: u64,
    },
    Claim {
        epoch: u64,
        execute_after: u64,
        state_root: FixedBytes<32>,
    },
    VerifyClaim {
        epoch: u64,
        execute_after: u64,
        state_root: FixedBytes<32>,
        claimer: Address,
        timestamp_claimed: u32,
    },
    Challenge {
        epoch: u64,
        execute_after: u64,
        state_root: FixedBytes<32>,
        claimer: Address,
        timestamp_claimed: u32,
    },
    SendSnapshot {
        epoch: u64,
        execute_after: u64,
        state_root: FixedBytes<32>,
        claimer: Address,
        timestamp_claimed: u32,
        challenger: Address,
    },
    StartVerification {
        epoch: u64,
        execute_after: u64,
        state_root: FixedBytes<32>,
        claimer: Address,
        timestamp_claimed: u32,
    },
    VerifySnapshot {
        epoch: u64,
        execute_after: u64,
        state_root: FixedBytes<32>,
        claimer: Address,
        timestamp_claimed: u32,
        timestamp_verification: u32,
        blocknumber_verification: u32,
    },
    ExecuteRelay {
        epoch: u64,
        execute_after: u64,
        #[serde(with = "u256_hex")]
        position: U256,
        l2_sender: Address,
        dest_addr: Address,
        l2_block: u64,
        l1_block: u64,
        l2_timestamp: u64,
        #[serde(with = "u256_hex")]
        amount: U256,
        #[serde(with = "bytes_hex")]
        data: Bytes,
    },
}

impl Task {
    pub fn epoch(&self) -> u64 {
        match self {
            Task::SaveSnapshot { epoch, .. } => *epoch,
            Task::Claim { epoch, .. } => *epoch,
            Task::VerifyClaim { epoch, .. } => *epoch,
            Task::Challenge { epoch, .. } => *epoch,
            Task::SendSnapshot { epoch, .. } => *epoch,
            Task::StartVerification { epoch, .. } => *epoch,
            Task::VerifySnapshot { epoch, .. } => *epoch,
            Task::ExecuteRelay { epoch, .. } => *epoch,
        }
    }

    pub fn execute_after(&self) -> u64 {
        match self {
            Task::SaveSnapshot { execute_after, .. } => *execute_after,
            Task::Claim { execute_after, .. } => *execute_after,
            Task::VerifyClaim { execute_after, .. } => *execute_after,
            Task::Challenge { execute_after, .. } => *execute_after,
            Task::SendSnapshot { execute_after, .. } => *execute_after,
            Task::StartVerification { execute_after, .. } => *execute_after,
            Task::VerifySnapshot { execute_after, .. } => *execute_after,
            Task::ExecuteRelay { execute_after, .. } => *execute_after,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Task::SaveSnapshot { .. } => "SaveSnapshot",
            Task::Claim { .. } => "Claim",
            Task::VerifyClaim { .. } => "VerifyClaim",
            Task::Challenge { .. } => "Challenge",
            Task::SendSnapshot { .. } => "SendSnapshot",
            Task::StartVerification { .. } => "StartVerification",
            Task::VerifySnapshot { .. } => "VerifySnapshot",
            Task::ExecuteRelay { .. } => "ExecuteRelay",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RouteState {
    pub inbox_last_block: Option<u64>,
    pub outbox_last_block: Option<u64>,
    pub tasks: Vec<Task>,
}

pub struct TaskStore {
    path: PathBuf,
}

impl TaskStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self) -> RouteState {
        match fs::read_to_string(&self.path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => RouteState::default(),
        }
    }

    pub fn save(&self, state: &RouteState) {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).expect("Failed to create schedules directory");
        }
        let contents = serde_json::to_string_pretty(state).expect("Failed to serialize state");
        fs::write(&self.path, contents).expect("Failed to write state file");
    }

    pub fn add_task(&self, task: Task) {
        let mut state = self.load();
        state.tasks.push(task);
        self.save(&state);
    }

    pub fn remove_task(&self, task: &Task) {
        let mut state = self.load();
        let epoch = task.epoch();
        let name = task.name();
        state.tasks.retain(|t| !(t.epoch() == epoch && t.name() == name));
        self.save(&state);
    }

    pub fn update_inbox_block(&self, block: u64) {
        let mut state = self.load();
        state.inbox_last_block = Some(block);
        self.save(&state);
    }

    pub fn update_outbox_block(&self, block: u64) {
        let mut state = self.load();
        state.outbox_last_block = Some(block);
        self.save(&state);
    }
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
