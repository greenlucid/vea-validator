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
1. **Save snapshot** ~60s before epoch ends (if messages exist and count changed)
2. **Make claim** ~15min after epoch starts (if `MAKE_CLAIMS=true` and synced)

**Snapshot saving logic:** The indexer tracks `last_saved_count` from `SnapshotSaved` events. Before saving, we compare `inbox.count()` to `last_saved_count`:
- If `count == 0`: no messages, skip
- If `count <= last_saved_count`: nothing new, skip
- If `last_saved_count` is unknown (None): treat as 0, save if messages exist

**Cold-start behavior:** If no `SnapshotSaved` events exist in the sync window (fresh chain or long hiatus), `last_saved_count` defaults to 0. This ensures the validator will save a snapshot if any messages exist, preventing bridge staleness when this is the only validator running.

**15min buffer edge case:** Since `SnapshotSaved` events are processed with the same 15min finality buffer as other events, if another validator saves a snapshot within the last 15min of an epoch, this validator won't see it yet and may call `saveSnapshot()` redundantly. This is acceptable, since L2 gas is cheap.

**Why claims are optional:** To challenge fraud, the validator needs ETH/WETH for deposits. Making claims locks funds on the outbox. During an attack, a conservative validator should preserve capital for challenges rather than tie it up in claims.

### EventIndexer
Scans inbox/outbox logs in chunks. Only processes events from blocks older than 15min (finality buffer).

Reacts to events:
- `outbox.Claimed` → schedules `task::validate_claim`
  - if valid → schedules `task::start_verification` (after `start_verification_delay`)
  - if invalid → schedules `task::challenge`
- `outbox.VerificationStarted` → schedules `task::verify_snapshot` (after minChallengePeriod)
- `outbox.Challenged` → schedules `task::send_snapshot`
- `outbox.Verified` → schedules `task::withdraw_deposit`
- `inbox.SnapshotSent` → schedules `task::execute_relay` (after `relay_delay`), **only if emitted by this validator**
- `inbox.SnapshotSaved` → updates `last_saved_count` (used by save_snapshot logic)

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
| `start_verification` | valid claim (after `start_verification_delay`) | starts verification period |
| `verify_snapshot` | VerificationStarted + minChallengePeriod | finalizes verification |
| `execute_relay` | SnapshotSent (after `relay_delay`) | executes L2→L1 message on Arbitrum Outbox |
| `withdraw_deposit` | Verified event | withdraws to honest party |

## Sync Behavior

On startup, indexer initializes from `now - sync_lookback_secs`. The lookback is computed dynamically from contract parameters: `relay_delay + start_verification_delay + min_challenge_period + buffer`. Events older than sync window are dropped gracefully. Tasks only execute when `on_sync=true`.

## Error Handling

### General Errors
- **RPC failures during indexing**: logged, retry next poll
- **Task failures**: task stays in queue, retried next poll
- **`Insufficient funds` on Challenge**: rescheduled +15min
- **`Invalid claim` reverts**: rescheduled +30min (see below)

### Proactive Task Invalidation

When the indexer processes certain events, it proactively removes tasks that are no longer possible:

| Event | Tasks Invalidated |
|-------|-------------------|
| `Claimed` | (none) |
| `VerificationStarted` | `StartVerification` |
| `Challenged` | `ValidateClaim`, `Challenge`, `StartVerification`, `VerifySnapshot` |
| `Verified` | `ValidateClaim`, `Challenge`, `StartVerification`, `VerifySnapshot`, `ExecuteRelay` |

This prevents wasted execution attempts and keeps the task queue clean.

### Invalid Claim Rescheduling

Due to the 15min finality buffer, someone can change claim state right before we execute a task. When `challenge`, `start_verification`, or `verify_snapshot` reverts with "Invalid claim" (claimHash mismatch), we reschedule +30min to give the indexer time to catch up and update ClaimStore with fresh claim data.

### Race Conditions

Each task has its own way of detecting and handling race conditions (another validator did the job first):

| Task | Detection | Handling |
|------|-----------|----------|
| `save_snapshot` | snapshot count unchanged | skip (no-op) |
| `claim` | `claimHashes[epoch] != 0` (pre-check) | drop task |
| `claim` | `stateRoot` already matches | drop task |
| `claim` | state root in pending claim | drop task |
| `claim` | on revert: `claimHashes[epoch] != 0` | drop task |
| `challenge` | on revert: `Challenged` event emitted | drop task |
| `challenge` | on revert: `VerificationStarted` event emitted | reschedule +15min |
| `send_snapshot` | (none - cheap, idempotent) | always try |
| `start_verification` | `challenger != 0` | drop task |
| `start_verification` | on revert: `VerificationStarted` event emitted | drop task |
| `start_verification` | on revert: `Challenged` event emitted | drop task |
| `verify_snapshot` | `challenger != 0` | drop task |
| `verify_snapshot` | on revert: `Verified` event emitted | drop task |
| `verify_snapshot` | on revert: `Challenged` event emitted | drop task |
| `execute_relay` | `isSpent(position) == true` | drop task |
| `execute_relay` | `roots(root) == 0` | reschedule +1hr |
| `execute_relay` | any revert | drop task (unreliable error messages from Arbitrum outbox) |
| `withdraw_deposit` | `claimHashes[epoch] == 0` (pre-check) | drop task |
| `withdraw_deposit` | on revert: `claimHashes[epoch] == 0` | drop task |

**Frontrun detection via event checks:** When a task reverts, we check if the expected event was emitted in recent blocks (last 500 blocks, capped at 1000 block range for RPC compatibility). This detects frontruns where another validator's tx landed between our pre-check and tx execution. If the event exists, the job was done - we drop the task. For `challenge`, if `VerificationStarted` was emitted (rare edge case during resync), we reschedule +15min to wait for finality before retrying with updated claim data.

### Missing Claim Data

If we see `Verified`, `VerificationStarted`, or `Challenged` for an epoch whose `Claimed` event was emitted before our sync window, we drop it - we can't reconstruct claim data we never saw.

**Starting mid-bridge:** The validator can be started at any time. On first run (or after a hiatus longer than `sync_lookback_secs`), `indexing_since` is set to `now - sync_lookback_secs` and persisted. Events for epochs whose `Claimed` was before this window are dropped - the contract only stores a claim hash, so the full `Claim` struct can only be reconstructed from witnessing the original `Claimed` event.

## SnapshotSent Filtering

The indexer only processes `SnapshotSent` events from transactions it sent itself (checked via `tx.signer == wallet_address`).

**Why?** `sendSnapshot()` is cheap to call with arbitrary data. An attacker could spam bogus snapshots with invalid `Claim` parameters. If we processed all `SnapshotSent` events, we'd waste gas relaying L2→L1 messages that would revert on Arbitrum's outbox (wrong proof/claim data).

**How it works:** When the validator sees a `Challenged` event, it calls `sendSnapshot` itself. By only indexing our own `SnapshotSent` events, we guarantee we're relaying messages we know are correct.

**Alternative approach (not implemented):** Parse the `L2ToL1Tx` calldata, compare the embedded `Claim` against our `ClaimStore`, verify no prior `SnapshotSent` exists for that epoch, then schedule the relay. This would let us piggyback on other validators' snapshots but adds complexity. The `isSpent` check in `execute_relay` would then serve as the final guard against duplicate relays. For v1, self-filtering is simpler and sufficient.

## Route-Specific Logic

Two types of route-specific branching:

**Deposit logic** (`weth_address.is_some()`): Used when the difference is ETH vs WETH deposits.

| Task | WETH route | ETH route |
|------|------------|-----------|
| `claim` | no `.value(deposit)` | `.value(deposit)` |
| `challenge` | check WETH balance, no `.value(deposit)` | check ETH balance, `.value(deposit)` |

**Contract signatures** (`match route.name`): Used when contracts have different function signatures.

| Task | ARB_TO_ETH | ARB_TO_GNOSIS |
|------|------------|---------------|
| `send_snapshot` | `sendSnapshot(epoch, claim)` | `sendSnapshot(epoch, gas_limit, claim)` |

## L2 Finality Verification

Before executing critical tasks (`validate_claim`, `claim`, `challenge`), the validator verifies the epoch's L2 data is finalized on L1. This prevents acting on data that could theoretically be reorged.

**How it works:**
1. Convert epoch → end timestamp: `(epoch + 1) * epoch_period`
2. Binary search to find the L2 block at that timestamp
3. Query `NodeInterface.findBatchContainingBlock(l2_block)` on Arbitrum to get the batch number
4. Search for `SequencerInbox.SequencerBatchDelivered(batchNum)` event on L1 to find which L1 block contains the batch
5. Compare that L1 block against `eth_getBlockByNumber("finalized")` - if finalized block >= batch block, the epoch is finalized

**Batch event query strategy:** Batches are posted to L1 every ~1-3 minutes after L2 transactions. To find the `SequencerBatchDelivered` event without querying from genesis (which would hit RPC block range limits), we:
1. Binary search L1 to find the block at `epoch_end_ts + 5 minutes` (accounting for batch posting delay)
2. Query a 2000-block range centered on that block (±1000 blocks = ~3.3 hours each direction)
3. If the event isn't in that range, return "not finalized" - either the batch hasn't been posted yet, or something is very wrong

**Configuration:**
- `SEQUENCER_INBOX`: Address of Arbitrum's SequencerInbox on Ethereum L1. **Required in production** - `startup.rs` panics if not set.
- For tests (local Anvil chains), `SEQUENCER_INBOX` is not set. When `sequencer_inbox` is `None`, finality checks return `Ok(true)` immediately, bypassing the check.

**Why finality matters:**
- The 15-minute buffer after epoch end is a heuristic, not a guarantee
- Real finality depends on batch posting to L1 and L1 block finalization (~15-20 min on mainnet)
- Without finality checks, a validator could challenge a valid claim if L2 data reorged after they read it

**Task behavior when not finalized:**
- Tasks return `Err("EpochNotFinalized")` and get rescheduled
- This is expected during normal operation when tasks are scheduled close to epoch boundaries

## Known Issues

### Execute Relay

L2→L1 message relay requires the merkle root to be confirmed on the L1 Arbitrum Outbox before `executeTransaction` can succeed. Roots are posted when rollup assertions are confirmed (after `confirmPeriodBlocks`). If the root isn't confirmed yet, `execute_relay` detects this via `outbox.roots(root) == 0` and reschedules +1 hour.

However, the reason the root is not included may be a computing error, this is being looked into.
