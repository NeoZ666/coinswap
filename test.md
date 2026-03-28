
### Discussed in https://github.com/orgs/citadel-tech/discussions/787

<div type='discussions-op-text'>

<sup>Originally posted by **NeoZ666** March  3, 2026</sup>

# Part A : Electrum Sync & Address-Pool Privacy Layer for Coinswap Taker

> Proposal for wallet sync via Electrum servers with query obfuscation through randomized address pools, enabling mobile (no full-node) operation while preserving privacy.

---

## 1. Problem Statement

The current `Wallet` implementation is tightly coupled to Bitcoin Core RPC. Every sync, UTXO lookup, descriptor import, and transaction broadcast flows through `bitcoincore_rpc::Client`. This requires:

- A **fully synced, non-pruned** Bitcoin Core node.
- Local or trusted-remote RPC access.
- ~600 GB+ disk for mainnet.

This makes running a **Taker on a mobile device impossible**, the entire coinswap privacy model breaks down if Takers can't easily participate.

### The Privacy Sub-Problem

Natively querying an Electrum server for wallet addresses leaks the **exact address set** to the server operator. For a coinswap wallet, this is catastrophic: the server can trivially link pre-swap and post-swap UTXOs, defeating the protocol's purpose. We need a mechanism to **obfuscate which addresses belong to the querying wallet** during sync.


## 2. Architecture Overview

### 2.1 `BlockchainBackend` Trait — Abstracting the Sync Layer

Introduce a trait that captures every blockchain interaction the wallet requires. The existing RPC path becomes one implementation; Electrum becomes another.

```rust
// src/wallet/electrum_backend.rs

pub trait BlockchainBackend: Send + Sync {
  . . .
}
```
From : https://docs.rs/electrum-client/

### 2.2 Electrum Backend with Address-Pool Privacy

```rust
// src/wallet/electrum_backend.rs

pub struct ElectrumBackend {
    /// Pool of Electrum server URLs
    servers: Vec<String>,
    /// Range for decoy addresses per batch — each batch picks a random
    /// count in [min, max] so batch sizes are never uniform.
    decoy_pool_range: (usize, usize),  // (min, max): (34, 67)
    /// Range for real addresses per batch — randomized per-batch within
    /// this window to prevent fingerprinting by fixed batch cadence.
    real_batch_range: (usize, usize),  // (min, max): (3, 9)
    /// Rotate server connection per batch.
    rotate_servers: bool,             
    /// Persistent decoy cache — survives across syncs, rotated gradually.
    /// See Section 7.2 for lifecycle details.
    decoy_cache: DecoyCache,
}

/// Cached decoy scripts with usage metadata for rotation decisions.
pub struct DecoyCache {
    /// All currently cached decoys with per-entry metadata.
    entries: Vec<DecoyEntry>,
    /// Max total decoys to keep in the cache at any time.
    max_cache_size: usize,             // 500
    /// Fraction of cache to retire (replace with fresh) on each sync.
    rotation_range: (f64, f64),    // (min, max) : (0.27, 0.53)
    /// Maximum number of syncs a single decoy can survive before forced retirement.
    max_uses: u32,                     // default: rand 5–15 per entry
    /// Maximum age (in seconds) before a decoy is force-retired.
    max_age_secs: u64,                 // default: 259200 (3 days)
}

pub struct DecoyEntry {
    pub script: ScriptBuf,
    pub hd_index: u32,
    pub keychain: KeychainKind,
    pub addr_type: AddressType,
    /// How many sync batches this decoy has appeared in.
    pub times_used: u32,
    /// Timestamp when the decoy was first created.
    pub created_at: u64,
    /// Per-entry max-use limit (randomized at creation to avoid uniform retirement).
    pub retire_after_uses: u32,
}
```

#### 2.2.1 The Address Randomization Pool

This is the core privacy mechanism. When the wallet needs to query the Electrum server for its addresses, it does **not** send them directly. Instead:

```
┌───────────────────────────────────────────────────────────────────┐
│              Address Pool Subscribe Flow (per sync)               │
│                                                                   │
│  Wallet has N real scripts to monitor (e.g. 15)                   │
│                                                                   │
│  1. For each batch, draw random sizes:                            │
│     real_count  ← rand(real_batch_range.0 .. real_batch_range.1)  │
│     decoy_count ← rand(decoy_pool_range.0 .. decoy_pool_range.1)  │
│     → Batch 1: 4 real + 52 decoy  (total 56)                      │
│     → Batch 2: 6 real + 38 decoy  (total 44)                      │
│     → Batch 3: 5 real + 65 decoy  (total 70)                      │
│     Varying sizes prevent fingerprinting by fixed batch cadence.  │
│                                                                   │
│  2. For each batch, generate `decoy_count` decoy scripts          │
│     → Source: DecoyCache selects a MIX of reused + fresh decoys   │
│       ~70% from cache (seen before), ~30% brand-new HD derivations│
│                                                                   │
│  3. Shuffle real + decoy into single array                        │
│     → [decoy_12, real_2, decoy_7, real_0, decoy_41, …]            │
│                                                                   │
│  4. SUBSCRIBE via `batch_script_subscribe(shuffled_scripts)`      │
│     → Server returns `Vec<Option<ScriptStatus>>` — one per script │
│     → ScriptStatus = None  means script has NO history yet        │
│     → ScriptStatus = Some  means script HAS on-chain activity     │
│                                                                   │
│  5. For scripts with Some(status), fetch UTXOs on-demand:         │
│     → `batch_script_list_unspent(active_scripts)` (real + decoy)  │
│     → Keeps the decoy cover even for the UTXO detail query        │
│                                                                   │
│  6. ROTATE server connection for next batch                       │
│     → Each batch hits a DIFFERENT Electrum server via new         │
│       Tor circuit — no single server sees all real scripts        │
│                                                                   │
│  7. After initial sync, keep real subscriptions alive:            │
│     → `ping()` periodically to trigger notification processing    │
│     → `script_pop(script)` to consume queued status changes       │
│     → On status change → `script_list_unspent` to refresh UTXOs   │
│                                                                   │
└───────────────────────────────────────────────────────────────────┘
```

**Privacy properties:**
- **Per-batch dilution**: Batch sizes vary randomly (e.g. 37-76 total, 3-9 real) → the real-to-decoy ratio fluctuates between ~4% and ~24%, preventing statistical fingerprinting by fixed-size patterns.
- **Cross-batch unlinkability**: Different servers per batch → no server sees the full wallet.
- **Decoy realism**: Decoys are derived from the wallet's own HD key at unused indices, so they're valid addresses with no on-chain history, indistinguishable from real unused addresses from the server's perspective.
- **Server rotation via Tor**: Each batch routes through a different Tor circuit to a different Electrum server, preventing IP-based correlation.


#### 2.2.2 Transaction Broadcast Privacy

Transaction broadcast gets the same pool treatment, we broadcast through a **randomly selected** Electrum server over a **fresh Tor circuit** using `transaction_broadcast(tx)`. This prevents the server from correlating “who subscribed to these scripts” with “who broadcast this transaction.”

For additional broadcast privacy, the wallet can optionally relay through **multiple** Electrum servers simultaneously (broadcast to 3-5 servers via separate `transaction_broadcast` calls on different connections) at different time periods, making it harder to identify the originating node and do time correlation chainanalysis.

---

## 3. Wallet Struct Refactoring

### 3.1 Initialization Changes

```rust
/// Backend configuration — determines how the wallet talks to the blockchain.
pub enum BackendConfig {
    /// Full Bitcoin Core RPC (for makers, or takers with a local node).
    Rpc(RPCConfig),
    /// Electrum server pool with privacy parameters (for mobile takers).
    Electrum(ElectrumConfig),
}

pub struct ElectrumConfig {
    pub servers: Vec<String>,
    pub tor_proxy: Option<String>,
    /// Range for decoy addresses per batch — random count drawn each batch.
    pub decoy_pool_range: (usize, usize),   // 34 - 67
    /// Range for real addresses per batch — random count drawn each batch.
    pub real_batch_range: (usize, usize),  // 3 - 9
    /// Max decoys to keep in persistent cache.
    pub max_decoy_cache_size: usize,       // 500
    /// Fraction of decoys to replace with fresh ones per sync.
    pub decoy_rotation_range: (f64, f64), // (min, max) : (0.27, 0.53)
    pub network: Network,
}
```


## 4. Decoy Address Generation Details

### 4.1 Decoy Lifecycle: Cache & Rotation

Decoys cannot be all-fresh every sync (a server seeing 50 brand-new never-before-queried scripts each time is itself a fingerprint). They also cannot be all-reused every time (the stable set becomes clearly identifiable as decoys). The solution is a **persistent decoy cache** with gradual rotation.

#### 4.1.1 Cache Structure

The cache is stored in the encrypted wallet file (same `WalletStore` serialization path). On first-ever sync, the cache starts empty and is fully populated with fresh decoys. On subsequent syncs, the cache is loaded from disk and partially rotated.

#### 4.1.2 The Rotation Algorithm

Each sync goes through this lifecycle:

```
┌───────────────────────────────────────────────────────────────────┐
│               Decoy Rotation Per-Sync                             │
│                                                                   │
│  Cache has C decoys (e.g. 500).   This sync needs D (e.g. 50).    │
│                                                                   │
│  1. RETIRE expired decoys from cache:                             │
│     - times_used >= retire_after_uses  (per-entry random limit)   │
│     - age > max_age_secs  (e.g. 3 days)                           │
│     → Remove them. They are never queried again.                  │
│                                                                   │
│  2. Draw rotation ← rand(rotation_range)  (0.29–0.53)             │
│     fresh_count  = floor(D × rotation_pct)     (e.g. 10)          │
│     reused_count = D - fresh_count             (e.g. 40)          │
│                                                                   │
│  3. REUSE: sample `reused_count` from surviving cache entries     │
│     → Increment their `times_used`                                │
│     → These are decoys the server has seen before — looks like    │
│       a real wallet that keeps checking its own addresses.        │
│                                                                   │
│  4. FRESH: generate `fresh_count` brand-new decoy entries         │
│     → Add them to the cache                                       │
│                                                                   │
│  5. TRIM: if cache exceeds max_cache_size, drop oldest entries    │
│     by (created_at + times_used weight) — preferring to drop      │
│     heavily-used decoys that have served their purpose.           │
│                                                                   │
│  6. Combine: batch = real_scripts + reused_decoys + fresh_decoys  │
│     → Shuffle, subscribe, proceed as normal                       │
│                                                                   │
│  7. SAVE cache to disk after sync completes                       │
│                                                                   │
└───────────────────────────────────────────────────────────────────┘
```

```rust
impl DecoyCache {
    /// Select decoys for a single batch: mix of cached + fresh.
    /// Returns the scripts to use and mutates the cache state.
    fn select_decoys_for_batch(
        &mut self,
        needed: usize,
    ) -> Vec<ScriptBuf> {
        let mut rng = rand::thread_rng();
        let now = now_unix_secs();

        // 1. Retire expired entries
        self.entries.retain(|e| {
            e.times_used < e.retire_after_uses
                && (now - e.created_at) < self.max_age_secs
        });

        // 2. Determine fresh vs reused split
        let rotation_pct = rng.gen_range(self.rotation_range.0..=self.rotation_range.1);
        let fresh_count = (needed as f64 * rotation_pct).floor() as usize;
        let reused_count = needed.saturating_sub(fresh_count);

        // 3. Sample reused decoys from cache (random subset)
        let reused_count = reused_count.min(self.entries.len());
        let reused_indices: Vec<usize> = (0..self.entries.len())
            .choose_multiple(&mut rng, reused_count);
        let mut result = Vec::with_capacity(needed);

        for &idx in &reused_indices {
            self.entries[idx].times_used += 1;
            result.push(self.entries[idx].script.clone());
        }

        // 4. Generate fresh decoys (pure HD derivation, no network)
        let actual_fresh = needed - result.len();
        for _ in 0..actual_fresh {
            let entry = self.derive_fresh_decoy();
            result.push(entry.script.clone());
            self.entries.push(entry);
        }

        // 5. Trim cache if over max size
        while self.entries.len() > self.max_cache_size {
            // Drop the most-used + oldest entry
            if let Some(idx) = self.entries.iter().enumerate()
                .max_by_key(|(_, e)| e.times_used as u64 * 1000 + (now - e.created_at))
                .map(|(i, _)| i)
            {
                self.entries.swap_remove(idx);
            }
        }

        result
    }
}
```

#### 4.1.3 Cache Persistence

The `DecoyCache` is serialized into the wallet file alongside `WalletStore`, protected by the same AES-256-GCM encryption.
It adds ~75 KB to the wallet file (500 entries × ~150 bytes each).
On first launch with a fresh wallet, the cache starts empty — the first sync is 100% fresh decoys, then subsequent syncs gradually introduce reuse.

### 4.2 Tunable Parameters

All batch/pool sizes are **ranges** — each batch draws a random value within the window so no two syncs or batches look identical on the wire.

| Parameter | Default Range | Trade-off |
|---|---|---|
| `decoy_pool_range` | (34, 67) | Higher = more privacy, more bandwidth per batch |
| `real_batch_range` | (3, 9) | Lower = more privacy, more round-trips |
| `server_pool_size` | 8+ | More servers = harder to correlate batches |
| `tor_circuit_rotation` | Per-batch | Per-batch is strongest; per-sync is faster |
| `max_cache_size` | 500 | Larger = more decoy diversity, slightly more disk |
| `rotation_range` | (0.27, 0.53) | Higher = faster churn, less cross-sync overlap |
| `max_uses` | 5–15 (per entry) | Lower = faster retirement, more fresh decoys |
| `max_age_secs` | 259200 (3 days) | Shorter = less reuse risk, more derivation work |

### 4.3 Bandwidth Analysis (Estimates)

For a wallet with 30 real addresses :
- Average real per batch: ~6, average decoy per batch: ~50.5
- Batches: `ceil(30 / 6) ≈ 5`
- Average subscribe calls per batch: `5 + 50.5 = 56.5`
- Total subscribe calls: `5 × 56.5 ≈ 283`
- Each `script_subscribe` response: ~50 bytes (status hash or null)
- Follow-up `script_list_unspent` for active scripts: ~500 bytes to ~2KB (with history)
- **Subscribed**: 283 × 50B = **~14 KB**
- **UTXO details** (only active scripts): **~30 × 2KB = ~60 KB** 
- **Total**: **~77 KB per sync** — negligible on mobile.
 
---

# Part B: Android Kotlin App via UniFFI (coinswap-kotlin)

## Summary

Build a production-grade Android Taker app that consumes Coinswap only through `coinswap-kotlin` UniFFI bindings, using the new backend API (`BackendConfig`, `ElectrumConfig`, `PrivacyConfig`) introduced in Part A. The app should support both the new and updated unified flow, operate over Tor by default, and provide a mobile-first UX that remains faithful to Coinswap design and architectural cypherpunk philosophy.

The app should follow a design style guide aligned with `citadel-tech/taker-app`: clean information hierarchy, explicit privacy-state indicators, low-cognitive-load swap progression, and consistent transaction/UTXO visual semantics across screens. One-to-one in terms of functionality. 

## Problem

Even with an Electrum privacy backend, contributors still need an end-user surface that proves the new architecture works reliably and airtight in a real mobile environment.

## Expected Outcome

By the end of the project, contributors should deliver an Android app path where:

- Wallet initialization supports backend selection via `BackendConfig` and defaults to Electrum+Tor for mobile, including encryption/decryption of wallet.
- Sync, send, receive, and swap flows are fully driven through UniFFI (`Unified Taker`).
- Privacy controls (`decoy_pool_size`, `real_batch_size`, `circuit rotation`, `broadcast redundancy`) are user-configurable and persisted.
- Long-running swaps are resilient through a foreground service and restart-safe recovery flow.
- UI/UX patterns are consistent with taker-app design language while remaining native Compose-first.

## Recommended Approach

1. Build a strict repository boundary as the only FFI callsite.
2. Add backend-aware init APIs in Kotlin first, then wire viewmodels and screens.
3. Integrate Tor lifecycle management before implementing swap execution UX.
4. Implement sync/send/receive primitives before swap orchestration screens.
5. Add swap foreground service and recovery pipeline before beta hardening.
6. Validate UX and state transitions against taker-app style patterns (information density, states, and flow consistency).
7. Add Android specific tests to validate the new sync api is working.

## Scope

In scope:

- Android app architecture using `coinswap-kotlin` UniFFI bindings.
- Electrum+Tor operational mode as first-class mobile path.
- Compose UI screens for onboarding, home, send, receive, swap, utxo, transactions, settings.
- Privacy settings surface backed by `update_privacy_config`.
- Background sync and foreground swap execution.
- Backup/restore and startup recovery hooks.

Out of scope for this project issue:

- Rewriting Rust swap protocol state machines.
- Replacing UniFFI with custom JNI.
- Building a maker node Android experience.
- Pixel-perfect cloning of taker-app visual assets.

## Key Requirements

- All Rust interactions must go through generated UniFFI Kotlin APIs.
- Swap execution must survive app backgrounding while running.
- Recovery must be accessible on startup(implicit) and from user action(explicit).
- Privacy defaults should be conservative and clearly explain trade-offs.
- UI states must expose sync, swap, failure, and recovery progression transparently.

## Major Things To Keep In Mind

- Android lifecycle can drop process state at any time; persist enough metadata to resume or recover safely.
- FFI mutex-heavy operations can block; never perform wallet/swap calls on the main thread.
- Long operations need cancellation semantics that do not corrupt wallet state.
- Notification UX is part of protocol reliability for mobile swaps.
- Privacy settings should be understandable; advanced knobs must not become foot-guns.
- Keep Kotlin architecture modular so protocol/API evolution does not force a UI rewrite.

## Design Style Guide (Aligned with taker-app)

- Use clear, utility-first layouts: top-level status, primary balance context, and task actions above fold.
- Keep privacy posture visible: Tor status, backend mode, last sync health, and swap protocol selection should always be legible.
- Prefer progressive disclosure: simple defaults first, advanced controls in dedicated settings sections.
- Use deterministic component states: loading, success, partial failure, and recovery-needed must have distinct visuals and copy.
- Keep swap flow linear and staged: amount -> route/fee context -> confirmation -> live progress -> result/recovery.
- Maintain visual consistency for monetary units and risk labels across send/swap/recovery.
- Minimize decorative complexity in critical flows; prioritize readability and operator confidence.

## Deliverables

- Updated FFI-facing Kotlin integration using new backend/privacy configuration types.
- Android app module skeleton with DI, navigation, and repository/use-case layering.
- Core operational screens (dashboard/market/send/receive/transactions/swap/settings).
- Foreground swap service and periodic sync worker integration.
- Recovery entrypoints (automatic at launch + manual trigger).
- Developer docs for build/run/test pipeline and native library packaging.

## Open Technical Discussions

- Tor does a default periodic circuit churn(by default its' 10 minutes), but this is implicit and isn't configurable by the user at this moment. Changing network path between every batched query for addrs and broadcast Txns can reduce timing and network-path correlations. Adv Privacy wallets like Wasabi pursue this aggressively.

- What is the most robust strategy for combining Tor circuit rotation policies with Android background execution constraints (WorkManager + foreground service) without degrading swap reliability or leaking stable network fingerprints?

- Allow Orbot or Arti for Tor integration?
    **Option A: Arti (Rust Tor, embedded)**
    - Ship `libarti.so` alongside `libcoinswap_ffi.so`.
    - Start/stop programmatically. Provides SOCKS5 on `127.0.0.1:9050`.
    - Advantage: no external dependency. Disadvantage: larger APK (~8 MB).

    **Option B: Orbot integration**
    - Check if Orbot is installed. If not, prompt user to install.
    - Use Orbot's SOCKS5 proxy.
    - Advantage: shared Tor instance. Disadvantage: external dependency.

---
</div>