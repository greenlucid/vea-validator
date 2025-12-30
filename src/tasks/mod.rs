pub mod dispatcher;
pub mod save_snapshot;
pub mod claim;
pub mod validate_claim;
pub mod challenge;
pub mod send_snapshot;
pub mod start_verification;
pub mod verify_snapshot;
pub mod execute_relay;
pub mod withdraw_deposit;

use alloy::primitives::{Address, Bytes, FixedBytes, U256};
use alloy::contract::Error as ContractError;
use alloy::network::Ethereum;
use alloy::providers::{DynProvider, PendingTransactionBuilder, Provider};
use alloy::rpc::types::Filter;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::contracts::{Claim, Party};

fn decode_revert_reason(err_msg: &str) -> Option<String> {
    let data_prefix = "data: \"0x";
    let start = err_msg.find(data_prefix)? + data_prefix.len();
    let end = err_msg[start..].find('"')? + start;
    let hex_data = &err_msg[start..end];

    if hex_data.is_empty() {
        return Some("(empty)".to_string());
    }

    if hex_data.len() < 8 {
        return Some(format!("0x{}", hex_data));
    }

    let bytes = alloy::hex::decode(hex_data).ok()?;

    if bytes.len() >= 4 && bytes[0..4] == [0x08, 0xc3, 0x79, 0xa0] {
        if bytes.len() >= 68 {
            let offset = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
            if bytes.len() >= 36 + offset {
                let len_start = 4 + offset;
                let len = u32::from_be_bytes([bytes[len_start + 28], bytes[len_start + 29], bytes[len_start + 30], bytes[len_start + 31]]) as usize;
                let str_start = len_start + 32;
                if bytes.len() >= str_start + len {
                    return String::from_utf8(bytes[str_start..str_start + len].to_vec()).ok();
                }
            }
        }
    }

    if bytes.len() >= 4 && bytes[0..4] == [0x4e, 0x48, 0x7b, 0x71] {
        if bytes.len() >= 36 {
            let code = U256::from_be_slice(&bytes[4..36]);
            return Some(format!("Panic(0x{:02x})", code));
        }
    }

    if bytes.len() >= 4 {
        let selector: [u8; 4] = bytes[0..4].try_into().unwrap();
        let known_errors: &[(&str, usize)] = &[
            ("UnknownRoot(bytes32)", 32),
            ("AlreadySpent()", 0),
            ("ProofTooLong(uint256)", 32),
            ("NotOutbox(address)", 32),
        ];
        for (sig, param_len) in known_errors {
            let expected = &alloy::primitives::keccak256(sig)[..4];
            if selector == expected {
                if *param_len == 0 {
                    return Some(sig.to_string());
                } else if bytes.len() >= 4 + param_len {
                    return Some(format!("{}(0x{})", sig.split('(').next().unwrap(), alloy::hex::encode(&bytes[4..4 + param_len])));
                }
            }
        }
    }

    Some(format!("0x{}", hex_data))
}

pub async fn was_event_emitted(
    provider: &DynProvider<Ethereum>,
    address: Address,
    event_sig: &str,
    epoch: u64,
) -> bool {
    let current_block = match provider.get_block_number().await {
        Ok(b) => b,
        Err(_) => return false,
    };
    let from_block = current_block.saturating_sub(500);

    let sig = alloy::primitives::keccak256(event_sig);
    let filter = Filter::new()
        .address(address)
        .event_signature(sig)
        .from_block(from_block)
        .to_block(from_block + 1000);

    match provider.get_logs(&filter).await {
        Ok(logs) => logs.iter().any(|log| {
            log.topics().get(1)
                .map(|t| U256::from_be_bytes(t.0).to::<u64>() == epoch)
                .unwrap_or(false)
        }),
        Err(_) => false,
    }
}

pub async fn send_tx(
    result: Result<PendingTransactionBuilder<Ethereum>, ContractError>,
    action: &str,
    route_name: &str,
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
            match decode_revert_reason(&err_msg) {
                Some(reason) => {
                    eprintln!("[{}] {} reverted: {}", route_name, action, reason);
                    Err(format!("[{}] {} reverted: {}", route_name, action, reason).into())
                }
                None => Err(e.into()),
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub epoch: u64,
    pub execute_after: u64,
    pub kind: TaskKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TaskKind {
    SaveSnapshot,
    Claim { state_root: FixedBytes<32> },
    ValidateClaim,
    Challenge,
    SendSnapshot,
    StartVerification,
    VerifySnapshot,
    ExecuteRelay {
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
    WithdrawDeposit,
}

impl TaskKind {
    pub fn name(&self) -> &'static str {
        match self {
            TaskKind::SaveSnapshot => "SaveSnapshot",
            TaskKind::Claim { .. } => "Claim",
            TaskKind::ValidateClaim => "ValidateClaim",
            TaskKind::Challenge => "Challenge",
            TaskKind::SendSnapshot => "SendSnapshot",
            TaskKind::StartVerification => "StartVerification",
            TaskKind::VerifySnapshot => "VerifySnapshot",
            TaskKind::ExecuteRelay { .. } => "ExecuteRelay",
            TaskKind::WithdrawDeposit => "WithdrawDeposit",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RouteState {
    pub inbox_last_block: Option<u64>,
    pub outbox_last_block: Option<u64>,
    pub tasks: Vec<Task>,
    pub indexing_since: Option<u64>,
    #[serde(default)]
    pub on_sync: bool,
    pub last_saved_count: Option<u64>,
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
            Ok(contents) => serde_json::from_str(&contents).expect("Failed to parse claims file - data corrupted"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => panic!("Failed to read claims file: {}", e),
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

    pub fn has_state_root_in_recent_claims(&self, state_root: FixedBytes<32>, since_timestamp: u32) -> bool {
        let claims = self.load_all();
        claims.iter().any(|c| c.state_root == state_root && c.timestamp_claimed >= since_timestamp)
    }

    pub fn exists(&self, epoch: u64) -> bool {
        self.load_all().iter().any(|c| c.epoch == epoch)
    }
}

pub struct TaskStore {
    path: PathBuf,
}

impl TaskStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn label(&self) -> String {
        self.path.file_stem()
            .map(|s| s.to_string_lossy().to_uppercase().replace("-", "_"))
            .unwrap_or_default()
    }

    pub fn load(&self) -> RouteState {
        match fs::read_to_string(&self.path) {
            Ok(contents) => serde_json::from_str(&contents).expect("Failed to parse schedule file - data corrupted"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => RouteState::default(),
            Err(e) => panic!("Failed to read schedule file: {}", e),
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
        println!("[{}][TaskStore] Scheduling {} for epoch {} at {}", self.label(), task.kind.name(), task.epoch, task.execute_after);
        let mut state = self.load();
        state.tasks.push(task);
        self.save(&state);
    }

    pub fn remove_task(&self, task: &Task) {
        let mut state = self.load();
        state.tasks.retain(|t| !(t.epoch == task.epoch && t.kind.name() == task.kind.name()));
        self.save(&state);
    }

    pub fn reschedule_task(&self, task: &Task, execute_after: u64) {
        println!("[{}][TaskStore] Rescheduling {} for epoch {} to {}", self.label(), task.kind.name(), task.epoch, execute_after);
        let mut state = self.load();
        for t in &mut state.tasks {
            if t.epoch == task.epoch && t.kind.name() == task.kind.name() {
                t.execute_after = execute_after;
                break;
            }
        }
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

    pub fn set_indexing_since(&self, ts: u64) {
        let mut state = self.load();
        state.indexing_since = Some(ts);
        self.save(&state);
    }

    pub fn initialize_sync(&self, indexing_since: u64, inbox_block: u64, outbox_block: u64) {
        let mut state = self.load();
        state.indexing_since = Some(indexing_since);
        state.inbox_last_block = Some(inbox_block);
        state.outbox_last_block = Some(outbox_block);
        self.save(&state);
    }

    pub fn set_on_sync(&self, value: bool) {
        let mut state = self.load();
        state.on_sync = value;
        self.save(&state);
    }

    pub fn is_on_sync(&self) -> bool {
        self.load().on_sync
    }

    pub fn invalidate_tasks(&self, epoch: u64, kinds: &[&str]) {
        let mut state = self.load();
        let before = state.tasks.len();
        state.tasks.retain(|t| !(t.epoch == epoch && kinds.contains(&t.kind.name())));
        let removed = before - state.tasks.len();
        if removed > 0 {
            println!("[{}][TaskStore] Invalidated {} tasks for epoch {}", self.label(), removed, epoch);
        }
        self.save(&state);
    }

    pub fn set_last_saved_count(&self, count: u64) {
        let mut state = self.load();
        state.last_saved_count = Some(count);
        self.save(&state);
    }

    pub fn get_last_saved_count(&self) -> Option<u64> {
        self.load().last_saved_count
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
