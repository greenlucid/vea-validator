# VEA Validator Design & Rationale

Rust validator for the VEA (Verified Ethereum Attestation) bridge. Monitors claims, challenges fraud, and relays messages between Arbitrum and destination chains (Ethereum, Gnosis).

## Architecture

```
main.rs
  ├── startup checks (RPC health, balances, WETH approval)
  └── per route:
        ├── EpochWatcher  - tracks epoch timing, saves snapshots at epoch end, optionally makes claims
        ├── EventIndexer  - scans inbox/outbox events, schedules tasks
        └── TaskDispatcher - executes scheduled tasks
```

## Routes

- `ARB_TO_ETH`: Arbitrum → Ethereum (native ETH deposits)
- `ARB_TO_GNOSIS`: Arbitrum → Gnosis (WETH deposits)

## Core Components

### EpochWatcher
Polls every 10s. Two responsibilities:
1. **Save snapshot** ~60s before epoch ends (if messages exist)
2. **Make claim** ~15min after epoch starts (if `MAKE_CLAIMS=true` and synced)

**Why claims are optional:** To challenge fraud, the validator needs ETH/WETH for deposits. Making claims locks funds on the outbox. During an attack, a conservative validator should preserve capital for challenges rather than tie it up in claims.

### EventIndexer
Scans inbox/outbox logs in chunks. Only processes events from blocks older than 15min (finality buffer).

Reacts to events:
- `outbox.Claimed` → schedules `task::validate_claim`
  - if valid → schedules `task::start_verification` (25h delay)
  - if invalid → schedules `task::challenge`
- `outbox.VerificationStarted` → schedules `task::verify_snapshot` (after minChallengePeriod)
- `outbox.Challenged` → schedules `task::send_snapshot`
- `outbox.Verified` → schedules `task::withdraw_deposit`
- `inbox.SnapshotSent` → schedules `task::execute_relay` (7d + 1h delay), **only if emitted by this validator**

### TaskDispatcher
Polls every 15s. Executes tasks when `execute_after` timestamp reached.

### TaskStore / ClaimStore
Each route has its own JSON files for persistence:
- **TaskStore**: scheduled tasks, indexer block cursors, `indexing_since` timestamp, `on_sync` flag
- **ClaimStore**: claim data (state root, claimer, timestamps, challenger, honest party) needed to reconstruct `Claim` structs for contract calls

## Task Types

| Task | Trigger | Action |
|------|---------|--------|
| `save_snapshot` | epoch end | calls `inbox.saveSnapshot()` |
| `claim` | epoch start | claims epoch on outbox |
| `validate_claim` | Claimed event | compares state roots, schedules challenge or start_verification |
| `challenge` | invalid claim | challenges with deposit |
| `send_snapshot` | Challenged event | sends snapshot via native bridge |
| `start_verification` | valid claim (25h delay) | starts verification period |
| `verify_snapshot` | VerificationStarted + minChallengePeriod | finalizes verification |
| `execute_relay` | SnapshotSent (7d+1h delay) | executes L2→L1 message on Arbitrum outbox |
| `withdraw_deposit` | Verified event | withdraws to honest party |

## Sync Behavior

On startup, indexer initializes from `now - 8d12h`. Events older than sync window are dropped gracefully. Tasks only execute when `on_sync=true`.

## Error Handling

### General Errors
- **RPC failures during indexing**: logged, retry next poll
- **Task failures**: task stays in queue, retried next poll
- **`Insufficient funds` on Challenge**: rescheduled +15min

### Race Conditions

Each task has its own way of detecting and handling race conditions (another validator did the job first):

| Task | Detection | Handling |
|------|-----------|----------|
| `save_snapshot` | snapshot count unchanged | skip (no-op) |
| `claim` | `claimHashes[epoch] != 0` | drop task |
| `claim` | `stateRoot` already matches | drop task |
| `claim` | state root in pending claim | drop task |
| `claim` | revert contains "already" | drop task |
| `challenge` | revert contains "already" | drop task |
| `challenge` | on revert: `Challenged` event emitted | drop task |
| `challenge` | on revert: `VerificationStarted` event emitted | reschedule +15min |
| `send_snapshot` | (none - cheap, idempotent) | always try |
| `start_verification` | `challenger != 0` | drop task |
| `start_verification` | revert contains "already" | drop task |
| `start_verification` | on revert: `VerificationStarted` event emitted | drop task |
| `start_verification` | on revert: `Challenged` event emitted | drop task |
| `verify_snapshot` | `challenger != 0` | drop task |
| `verify_snapshot` | revert contains "already" | drop task |
| `verify_snapshot` | on revert: `Verified` event emitted | drop task |
| `verify_snapshot` | on revert: `Challenged` event emitted | drop task |
| `execute_relay` | `isSpent(position) == true` | drop task |
| `execute_relay` | any revert | drop task (unreliable error messages from Arbitrum outbox) |
| `withdraw_deposit` | `claimHashes[epoch] == 0` (pre-check) | drop task |
| `withdraw_deposit` | on revert: `claimHashes[epoch] == 0` | drop task |

**Why "already" reverts are acceptable:** The indexer updates the ClaimStore when it sees events, but doesn't proactively clean up stale tasks from the scheduler. Due to our conservative finality buffer, the indexer may process an event (e.g., `Challenged`) and update the ClaimStore before the dispatcher attempts a now-stale task (e.g., `StartVerification`). The "already" revert safely handles this case. A future optimization could have the indexer drop stale tasks when processing events.

**Frontrun detection via event checks:** When a task reverts with "Invalid claim" (claimHash mismatch), we check if the expected event was emitted in recent blocks (last 500 blocks, capped at 1000 block range for RPC compatibility). This detects frontruns where another validator's tx landed between our pre-check and tx execution, changing the claimHash. If the event exists, the job was done - we drop the task. For `challenge`, if `VerificationStarted` was emitted (rare edge case during resync), we reschedule +15min to wait for finality before retrying with updated claim data.

### Missing Claim Data

If we see `Verified`, `VerificationStarted`, or `Challenged` for an epoch whose `Claimed` event was emitted before our sync window, we drop it - we can't reconstruct claim data we never saw.

**Starting mid-bridge:** The validator can be started at any time. On first run (or after a hiatus longer than 8d12h), `indexing_since` is set to `now - 8d12h` and persisted. Events for epochs whose `Claimed` was before this window are dropped - the contract only stores a claim hash, so the full `Claim` struct can only be reconstructed from witnessing the original `Claimed` event.

## SnapshotSent Filtering

The indexer only processes `SnapshotSent` events from transactions it sent itself (checked via `tx.signer == wallet_address`).

**Why?** `sendSnapshot()` is cheap to call with arbitrary data. An attacker could spam bogus snapshots with invalid `Claim` parameters. If we processed all `SnapshotSent` events, we'd waste gas relaying L2→L1 messages that would revert on Arbitrum's outbox (wrong proof/claim data).

**How it works:** When the validator sees a `Challenged` event, it calls `sendSnapshot` itself. By only indexing our own `SnapshotSent` events, we guarantee we're relaying messages we know are correct.

**Alternative approach (not implemented):** Parse the `L2ToL1Tx` calldata, compare the embedded `Claim` against our `ClaimStore`, verify no prior `SnapshotSent` exists for that epoch, then schedule the relay. This would let us piggyback on other validators' snapshots but adds complexity. The `isSpent` check in `execute_relay` would then serve as the final guard against duplicate relays. For v1, self-filtering is simpler and sufficient.
