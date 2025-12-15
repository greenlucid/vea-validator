pub mod dispatcher;
pub mod save_snapshot;
pub mod claim;
pub mod verify_claim;
pub mod challenge;
pub mod send_snapshot;
pub mod start_verification;
pub mod verify_snapshot;
pub mod execute_relay;
pub mod withdraw_deposit;

use alloy::primitives::{Address, Bytes, FixedBytes, U256};
use alloy::contract::Error as ContractError;
use alloy::network::Ethereum;
use alloy::providers::PendingTransactionBuilder;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::contracts::{Claim, Party};

pub async fn send_tx(
    result: Result<PendingTransactionBuilder<Ethereum>, ContractError>,
    action: &str,
    route_name: &str,
    race_ok: &[&str],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match result {
        Ok(pending) => {
            let receipt = pending.get_receipt().await?;
            if !receipt.status() {
                return Err(format!("[{}] {} reverted", route_name, action).into());
            }
            println!("[{}] {} succeeded", route_name, action);
            Ok(())
        }
        Err(e) => {
            let err_msg = e.to_string();
            for pattern in race_ok {
                if err_msg.contains(pattern) {
                    println!("[{}] {} already done", route_name, action);
                    return Ok(());
                }
            }
            Err(e.into())
        }
    }
}

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
    },
    Challenge {
        epoch: u64,
        execute_after: u64,
    },
    SendSnapshot {
        epoch: u64,
        execute_after: u64,
    },
    StartVerification {
        epoch: u64,
        execute_after: u64,
    },
    VerifySnapshot {
        epoch: u64,
        execute_after: u64,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimData {
    pub epoch: u64,
    pub state_root: FixedBytes<32>,
    pub claimer: Address,
    pub timestamp_claimed: u32,
    pub timestamp_verification: u32,
    pub blocknumber_verification: u32,
    pub honest: String,
    pub challenger: Address,
}

pub struct ClaimStore {
    path: PathBuf,
}

impl ClaimStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn load_all(&self) -> Vec<ClaimData> {
        match fs::read_to_string(&self.path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    fn save_all(&self, claims: &[ClaimData]) {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).expect("Failed to create claims directory");
        }
        let contents = serde_json::to_string_pretty(claims).expect("Failed to serialize claims");
        fs::write(&self.path, contents).expect("Failed to write claims file");
    }

    pub fn store(&self, claim: ClaimData) {
        let mut claims = self.load_all();
        let existing = claims.iter().filter(|c| c.epoch == claim.epoch).count();
        if existing > 0 {
            panic!("Duplicate claim for epoch {} - this should never happen", claim.epoch);
        }
        claims.push(claim);
        self.save_all(&claims);
    }

    pub fn update<F>(&self, epoch: u64, f: F)
    where
        F: FnOnce(&mut ClaimData),
    {
        let mut claims = self.load_all();
        let claim = claims.iter_mut().find(|c| c.epoch == epoch);
        match claim {
            Some(c) => {
                f(c);
                self.save_all(&claims);
            }
            None => panic!("Cannot update claim for epoch {} - not found", epoch),
        }
    }

    pub fn get(&self, epoch: u64) -> ClaimData {
        let claims = self.load_all();
        let matches: Vec<_> = claims.into_iter().filter(|c| c.epoch == epoch).collect();
        match matches.len() {
            0 => panic!("No claim found for epoch {}", epoch),
            1 => matches.into_iter().next().unwrap(),
            n => panic!("Multiple claims ({}) for epoch {} - impossible state", n, epoch),
        }
    }

    pub fn get_claim(&self, epoch: u64) -> Claim {
        let c = self.get(epoch);
        Claim {
            stateRoot: c.state_root,
            claimer: c.claimer,
            timestampClaimed: c.timestamp_claimed,
            timestampVerification: c.timestamp_verification,
            blocknumberVerification: c.blocknumber_verification,
            honest: match c.honest.as_str() {
                "Claimer" => Party::Claimer,
                "Challenger" => Party::Challenger,
                _ => Party::None,
            },
            challenger: c.challenger,
        }
    }

    pub fn remove(&self, epoch: u64) {
        let mut claims = self.load_all();
        claims.retain(|c| c.epoch != epoch);
        self.save_all(&claims);
    }
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
