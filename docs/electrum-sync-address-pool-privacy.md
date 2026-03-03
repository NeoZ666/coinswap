# Electrum Sync & Address-Pool Privacy Layer for Coinswap Taker

> Proposal for light-client wallet sync via Electrum servers with query obfuscation through randomized address pools, enabling mobile (no full-node) operation while preserving privacy.

---

## 1. Problem Statement

The current `Wallet` implementation is tightly coupled to Bitcoin Core RPC. Every sync, UTXO lookup, descriptor import, and transaction broadcast flows through `bitcoincore_rpc::Client`. This requires:

- A **fully synced, non-pruned** Bitcoin Core node with `-txindex=1`.
- Local or trusted-remote RPC access.
- ~600 GB+ disk for mainnet.

This makes running a **Taker on a mobile device impossible** — the entire coinswap privacy model breaks down if Takers can't easily participate.

### The Privacy Sub-Problem

Naively querying an Electrum server for wallet addresses leaks the **exact address set** to the server operator. For a coinswap wallet, this is catastrophic: the server can trivially link pre-swap and post-swap UTXOs, defeating the protocol's purpose. We need a mechanism to **obfuscate which addresses belong to the querying wallet** during sync.

---

## 2. Architecture Overview

### 2.1 `BlockchainBackend` Trait — Abstracting the Sync Layer

Introduce a trait that captures every blockchain interaction the wallet requires. The existing RPC path becomes one implementation; Electrum becomes another.

```rust
// src/wallet/backend.rs

pub trait BlockchainBackend: Send + Sync {
    fn get_block_count(&self) -> Result<u64, WalletError>;

    fn get_block_header(&self, height: u64) -> Result<BlockHeader, WalletError>;

    fn script_list_unspent(&self, scripts: &[ScriptBuf]) -> Result<Vec<ListUnspentEntry>, WalletError>;

    fn get_raw_transaction(&self, txid: &Txid) -> Result<Transaction, WalletError>;

    fn get_tx_out(&self, outpoint: &OutPoint) -> Result<Option<TxOut>, WalletError>;

    fn send_raw_transaction(&self, tx: &Transaction) -> Result<Txid, WalletError>;

    fn estimate_fee(&self, target_blocks: u16) -> Result<f64, WalletError>;

    fn get_tx_confirmations(&self, txid: &Txid) -> Result<u32, WalletError>;
}
```

**Key design constraint:** The trait surface must be minimal — only operations the wallet actually uses. The ~20 distinct RPC calls currently scattered across `rpc.rs`, `api.rs`, `fidelity.rs`, `spend.rs`, and `funding.rs` collapse into these primitives.

No behavioral change for existing maker setups. Makers **always** use RPC (they need a full node for contract monitoring and ZMQ).

### 2.3 Electrum Backend with Address-Pool Privacy

```rust
// src/wallet/electrum_backend.rs

pub struct ElectrumBackend {
    /// Pool of Electrum server URLs (Tor .onion preferred).
    servers: Vec<String>,
    /// Range for decoy addresses per batch — each batch picks a random
    /// count in [min, max] so batch sizes are never uniform.
    decoy_pool_range: (usize, usize),  // default: (30, 70)
    /// Range for real addresses per batch — randomized per-batch within
    /// this window to prevent fingerprinting by fixed batch cadence.
    real_batch_range: (usize, usize),  // default: (3, 8)
    /// Range for gap-limit probing depth — randomized per-sync so an
    /// observer cannot infer wallet size from a fixed scan length.
    gap_limit_range: (u32, u32),       // default: (15, 30)
    /// Rotate server connection per batch.
    rotate_servers: bool,              // default: true
    /// Persistent decoy cache — survives across syncs, rotated gradually.
    /// See Section 7.2 for lifecycle details.
    decoy_cache: DecoyCache,
}

/// Cached decoy scripts with usage metadata for rotation decisions.
pub struct DecoyCache {
    /// All currently cached decoys with per-entry metadata.
    entries: Vec<DecoyEntry>,
    /// Max total decoys to keep in the cache at any time.
    max_cache_size: usize,             // default: 200
    /// Fraction of cache to retire (replace with fresh) on each sync.
    rotation_pct_range: (f64, f64),    // default: (0.15, 0.35)
    /// Maximum number of syncs a single decoy can survive before forced retirement.
    max_uses: u32,                     // default: rand 5–15 per entry
    /// Maximum age (in seconds) before a decoy is force-retired.
    max_age_secs: u64,                 // default: 259200 (3 days)
}

pub struct DecoyEntry {
    pub script: ScriptBuf,
    /// HD derivation index used to produce this script.
    pub hd_index: u32,
    /// Keychain + address type used (for avoiding collisions).
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

#### 2.3.1 The Address Randomization Pool

This is the core privacy mechanism. When the wallet needs to query the Electrum server for its addresses, it does **not** send them directly. Instead:

```
┌───────────────────────────────────────────────────────────────────┐
│              Address Pool Subscribe Flow (per sync)               │
│                                                                   │
│  Wallet has N real scripts to monitor (e.g. 15)                   │
│                                                                   │
│  1. For each batch, draw random sizes:                            │
│     real_count  ← rand(real_batch_range.0 .. real_batch_range.1)  │
│     decoy_count ← rand(decoy_pool_range.0 .. decoy_pool_range.1) │
│     → Batch 1: 4 real + 52 decoy  (total 56)                     │
│     → Batch 2: 6 real + 38 decoy  (total 44)                     │
│     → Batch 3: 5 real + 65 decoy  (total 70)                     │
│     Varying sizes prevent fingerprinting by fixed batch cadence.  │
│                                                                   │
│  2. For each batch, generate `decoy_count` decoy scripts          │
│     → Source: DecoyCache selects a MIX of reused + fresh decoys    │
│       ~70% from cache (seen before), ~30% brand-new HD derivations │
│       (m/84'/1'/0'/0/100000..999999) — valid scripts that         │
│       look identical to real wallet addresses on the wire.         │
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
│  6. Client filters: keep only results for real script set          │
│     → Discard all decoy responses                                  │
│     → Unsubscribe decoys: `script_unsubscribe(decoy)`              │
│                                                                   │
│  7. ROTATE server connection for next batch                        │
│     → Each batch hits a DIFFERENT Electrum server via new          │
│       Tor circuit — no single server sees all real scripts          │
│                                                                   │
│  8. After initial sync, keep real subscriptions alive:             │
│     → `ping()` periodically to trigger notification processing     │
│     → `script_pop(script)` to consume queued status changes        │
│     → On status change → `script_list_unspent` to refresh UTXOs    │
│                                                                   │
└───────────────────────────────────────────────────────────────────┘
```

**Privacy properties:**
- **Per-batch dilution**: Batch sizes vary randomly (e.g. 38-70 total, 3-8 real) → the real-to-decoy ratio fluctuates between ~4% and ~17%, preventing statistical fingerprinting by fixed-size patterns.
- **Cross-batch unlinkability**: Different servers per batch → no server sees the full wallet.
- **Decoy realism**: Decoys are derived from the wallet's own HD key at unused indices, so they're valid addresses with (mostly) no on-chain history — indistinguishable from real unused addresses from the server's perspective.
- **Server rotation via Tor**: Each batch routes through a different Tor circuit to a different Electrum server, preventing IP-based correlation.
- **Subscribe, not poll**: Using `batch_script_subscribe` means the server pushes status changes asynchronously — the client does not repeatedly poll, reducing the traffic fingerprint compared to periodic `listunspent` loops.

#### 2.3.2 Electrum Protocol Mapping (`electrum-client` crate API)

All methods below reference the [`ElectrumApi`](https://docs.rs/electrum-client/latest/electrum_client/trait.ElectrumApi.html) trait from the `electrum-client` crate.

| Wallet Operation | `ElectrumApi` Method | Stratum RPC | Notes |
|---|---|---|---|
| `register_scripts` | `batch_script_subscribe(scripts)` | `blockchain.scripthash.subscribe` | Returns `Vec<Option<ScriptStatus>>`; `Some` = has history. Use `script_unsubscribe` to clean up decoys. |
| `list_unspent` | `batch_script_list_unspent(scripts)` | `blockchain.scripthash.listunspent` | Called only for scripts with `Some(status)`. Returns `Vec<Vec<ListUnspentRes>>`. |
| `get_raw_transaction` | `transaction_get(txid)` | `blockchain.transaction.get` | Provided method; deserializes raw bytes into `Transaction`. Batch: `batch_transaction_get`. |
| `send_raw_transaction` | `transaction_broadcast(tx)` | `blockchain.transaction.broadcast` | Provided method wrapping `transaction_broadcast_raw`. |
| `get_block_count` | `block_headers_subscribe()` | `blockchain.headers.subscribe` | Returns `HeaderNotification` with tip height. Pop new blocks via `block_headers_pop()`. |
| `get_block_header` | `block_header(height)` | `blockchain.block.header` | Provided method. Batch: `batch_block_header`. |
| `estimate_fee` | `estimate_fee(target_blocks)` | `blockchain.estimatefee` | Returns BTC/kB — convert to sat/vB. Batch: `batch_estimate_fee`. |
| `get_tx_confirmations` | `transaction_get_merkle(txid, height)` | `blockchain.transaction.get_merkle` | Verify inclusion via merkle proof + `block_header`. |
| `get_tx_out` | `script_list_unspent(script)` | `blockchain.scripthash.listunspent` | Filter result for specific `OutPoint`. |
| *live monitoring* | `ping()` + `script_pop(script)` | *(notification queue)* | `ping()` triggers notification processing; `script_pop` dequeues status changes for subscribed scripts. |

#### 2.3.3 Transaction Broadcast Privacy

Transaction broadcast gets the same pool treatment but in reverse — we broadcast through a **randomly selected** Electrum server over a **fresh Tor circuit** using `transaction_broadcast(tx)`. This prevents the server from correlating “who subscribed to these scripts” with “who broadcast this transaction.”

For additional broadcast privacy, the wallet can optionally relay through **multiple** Electrum servers simultaneously (broadcast to 3-5 servers via separate `transaction_broadcast` calls on different connections), making it harder to identify the originating node.

---

## 3. Wallet Struct Refactoring

### 3.1 Current Wallet Struct

```rust
pub struct Wallet {
    pub(crate) rpc: Client,                    
    pub(crate) wallet_file_path: PathBuf,
    pub(crate) store: WalletStore,
    pub(crate) store_enc_material: Option<KeyMaterial>,
}
```

### 3.2 Proposed Wallet Struct

```rust
pub struct Wallet {
    pub(crate) backend: Box<dyn BlockchainBackend>,   // Swappable backend
    ...
}
```

### 3.3 Initialization Changes

```rust
/// Backend configuration — determines how the wallet talks to the blockchain.
pub enum BackendConfig {
    /// Full Bitcoin Core RPC (for makers, or takers with a local node).
    Rpc(RPCConfig),
    /// Electrum server pool with privacy parameters (for mobile takers).
    Electrum(ElectrumConfig),
}

pub struct ElectrumConfig {
    /// List of Electrum server URLs. .onion addresses preferred.
    pub servers: Vec<String>,
    /// SOCKS5 proxy for Tor (e.g. "127.0.0.1:9050").
    pub tor_proxy: Option<String>,
    /// Range for decoy addresses per batch — random count drawn each batch.
    pub decoy_pool_range: (usize, usize),  // default: (30, 70)
    /// Range for real addresses per batch — random count drawn each batch.
    pub real_batch_range: (usize, usize),  // default: (3, 8)
    /// Range for gap-limit scan depth — random value drawn each sync.
    pub gap_limit_range: (u32, u32),       // default: (15, 30)
    /// Max decoys to keep in persistent cache.
    pub max_decoy_cache_size: usize,       // default: 200
    /// Fraction of decoys to replace with fresh ones per sync.
    pub decoy_rotation_pct_range: (f64, f64), // default: (0.15, 0.35)
    /// Network (signet/testnet/mainnet).
    pub network: Network,
}

impl Wallet {
    pub fn init(config: BackendConfig, ...) -> Result<Self, WalletError> {
        let backend: Box<dyn BlockchainBackend> = match config {
            BackendConfig::Rpc(rpc_cfg) => Box::new(RpcBackend::new(rpc_cfg)?),
            BackendConfig::Electrum(elec_cfg) => Box::new(ElectrumBackend::new(elec_cfg)?),
        };
        // ... rest of init unchanged
    }
}
```

---

## 4. Sync Flow: Electrum Path

```
┌───────────────────────────────────────────────────────────────────┐
│          sync() — Electrum Subscribe-First Backend                 │
│                                                                   │
│  1. Derive all wallet scriptPubKeys                               │
│     - HD external: m/84'/0'/0'/0/0..external_index+gap            │
│     - HD internal: m/84'/0'/0'/1/0..internal_index+gap            │
│     - P2TR external: m/86'/0'/0'/0/0..external_index+gap          │
│     - P2TR internal: m/86'/0'/0'/1/0..internal_index+gap          │
│     - All incoming_swapcoin multisig scripts                      │
│     - All outgoing_swapcoin multisig scripts                      │
│     - All contract scripts (hashlock/timelock)                    │
│     - Fidelity bond scripts                                       │
│                                                                   │
│  2. For each batch, draw RANDOM sizes:                            │
│     real_count  ← rand(real_batch_range)    e.g. 3–8              │
│     decoy_count ← rand(decoy_pool_range)   e.g. 30–70            │
│     gap_limit   ← rand(gap_limit_range)    e.g. 15–30            │
│     (each batch has a different total size)                       │
│                                                                   │
│  3. For each batch:                                               │
│     a. Select random Electrum server from pool                    │
│     b. Connect via Tor SOCKS5 proxy (new circuit)                 │
│     c. Select decoys from DecoyCache (mix of reused + fresh)      │
│     d. Shuffle real + decoy                                       │
│     e. SUBSCRIBE: `batch_script_subscribe(shuffled_scripts)`      │
│        → Returns Vec<Option<ScriptStatus>>                        │
│        → Some(status) = has on-chain activity                     │
│        → None          = no history (most decoys land here)       │
│     f. For scripts with Some(status):                             │
│        `batch_script_list_unspent(active_scripts + some_decoys)` │
│        → Fetch UTXOs, still mixing in decoys for cover            │
│     g. Filter results → keep only real script entries              │
│     h. Unsubscribe decoys: `script_unsubscribe(decoy)`            │
│     i. Disconnect / rotate to next server                         │
│                                                                   │
│  4. Aggregate all UTXO results                                    │
│     - Map to ListUnspentEntry-equivalent structs                  │
│     - Derive UTXOSpendInfo for each (same logic as RPC path)      │
│                                                                   │
│  5. Update WalletStore                                            │
│     - update_utxo_cache(aggregated_utxos)                         │
│     - Update external_index via randomized gap-limit scanning     │
│     - Refresh offer_maxsize                                       │
│     - Set last_synced_height from tip (via block_headers_subscribe)│
│                                                                   │
│  6. save_to_disk()                                                │
└───────────────────────────────────────────────────────────────────┘
```

### 4.1 Gap Limit Scanning (Electrum-Specific)

With RPC, the wallet relies on `importdescriptors` with a range and then Core's internal scanning. With Electrum, we implement gap-limit scanning ourselves — using a **randomized gap limit** per sync so an observer cannot infer wallet size from a fixed scan length:

```rust
/// Range for gap-limit depth.  Each sync picks a random value in this
/// window so the total number of scripts queried is never constant.
const GAP_LIMIT_MIN: u32 = 15;
const GAP_LIMIT_MAX: u32 = 30;

fn scan_keychain(
    &self,
    keychain: KeychainKind,
    address_type: AddressType,
) -> Result<u32, WalletError> {
    let mut rng = rand::thread_rng();
    // Draw a fresh random gap limit for THIS scan
    let gap_limit = rng.gen_range(GAP_LIMIT_MIN..=GAP_LIMIT_MAX);

    let mut last_used_index = 0u32;
    let mut consecutive_unused = 0u32;
    let mut index = 0u32;

    // Collect scripts in randomized-size batches, subscribe + check
    while consecutive_unused < gap_limit {
        // Draw a random batch size for this round
        let batch_size = rng.gen_range(self.real_batch_range.0..=self.real_batch_range.1) as u32;
        let mut batch_scripts = Vec::new();
        let start = index;

        for _ in 0..batch_size {
            if consecutive_unused >= gap_limit {
                break;
            }
            let script = self.derive_script_at_index(keychain, address_type, index)?;
            batch_scripts.push(script);
            index += 1;
        }

        // Pad batch with decoys from cache (mix of reused + fresh)
        let decoy_count = rng.gen_range(self.decoy_pool_range.0..=self.decoy_pool_range.1);
        let decoys = self.decoy_cache.select_decoys_for_batch(decoy_count);
        let mut shuffled = batch_scripts.clone();
        shuffled.extend(decoys);
        shuffled.shuffle(&mut rng);

        // Subscribe to the mixed batch
        let statuses = client.batch_script_subscribe(&shuffled)?;

        // Check real scripts only
        for (i, script) in batch_scripts.iter().enumerate() {
            let real_idx = shuffled.iter().position(|s| s == script).unwrap();
            if statuses[real_idx].is_some() {
                // Script has on-chain history
                last_used_index = start + i as u32;
                consecutive_unused = 0;
            } else {
                consecutive_unused += 1;
            }
        }

        // Unsubscribe decoys, keep real subscriptions alive
        for decoy in &decoys {
            let _ = client.script_unsubscribe(decoy);
        }
    }
    Ok(last_used_index + 1)
}
```

**Why randomize the gap limit?** A fixed gap limit (e.g. always 20) leaks a fingerprint: every sync probes exactly 20 consecutive empty indices. By drawing from `[15, 30]` each time, the scan depth varies unpredictably, making it harder for a server to correlate repeat syncs from the same wallet.

---

## 5. Contract Monitoring Without ZMQ

On a full node, the Watch Tower uses ZMQ subscriptions (`zmqpubrawtx`, `zmqpubrawblock`) for real-time contract breach detection. On Electrum, contract monitoring is **subscription-first** via the `electrum-client` crate’s subscribe/pop notification model — no polling loop required:

```rust
impl ElectrumBackend {
    /// Subscribe to all contract scripts (mixed with decoys) and watch
    /// for spend events via push notifications.
    fn monitor_contract_scripts(
        &self,
        contract_scripts: &[ScriptBuf],
    ) -> Result<Vec<ContractSpendEvent>, WalletError> {
        let mut rng = rand::thread_rng();

        // 1. Pad contract scripts with decoys from cache (varied count)
        let decoy_count = rng.gen_range(self.decoy_pool_range.0..=self.decoy_pool_range.1);
        let decoys = self.decoy_cache.select_decoys_for_batch(decoy_count);
        let mut all_scripts = contract_scripts.to_vec();
        all_scripts.extend(decoys.clone());
        all_scripts.shuffle(&mut rng);

        // 2. Subscribe to the mixed batch
        //    batch_script_subscribe returns Vec<Option<ScriptStatus>>
        let _statuses = client.batch_script_subscribe(&all_scripts)?;

        // 3. Event loop: ping() triggers notification processing,
        //    then script_pop() drains queued status changes.
        loop {
            client.ping()?;  // nudge the connection to process inbound msgs

            for script in contract_scripts {
                if let Some(_new_status) = client.script_pop(script)? {
                    // Status changed → script was spent or received new tx
                    let utxos = client.script_list_unspent(script)?;
                    let history = client.script_get_history(script)?;
                    let event = self.classify_spend(script, &utxos, &history)?;
                    // Classify: hashlock / timelock / keypath spend
                    events.push(event);
                }
            }

            if !events.is_empty() {
                // Clean up: unsubscribe decoys
                for decoy in &decoys {
                    let _ = client.script_unsubscribe(decoy);
                }
                return Ok(events);
            }

            // Sleep briefly before next ping to avoid busy-loop
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    /// Fallback: reconnect and reconcile state after a dropped connection.
    /// Only used when the subscription channel is lost (mobile network switch,
    /// server timeout, etc.).  NOT a periodic polling loop.
    fn reconcile_contract_scripts_fallback(
        &self,
        contract_scripts: &[ScriptBuf],
        last_known: &HashMap<ScriptBuf, Option<ScriptStatus>>,
    ) -> Result<Vec<ContractSpendEvent>, WalletError> {
        // Re-subscribe and compare returned ScriptStatus against
        // last_known to detect any changes that happened while disconnected.
        let statuses = client.batch_script_subscribe(contract_scripts)?;
        let mut events = Vec::new();
        for (script, new_status) in contract_scripts.iter().zip(statuses.iter()) {
            if new_status != last_known.get(script).unwrap_or(&None) {
                let utxos = client.script_list_unspent(script)?;
                let history = client.script_get_history(script)?;
                events.push(self.classify_spend(script, &utxos, &history)?);
            }
        }
        Ok(events)
    }
}
```

**Latency trade-off**: ZMQ gives sub-second notification on a local full node. Electrum subscriptions via `script_subscribe` + `script_pop` are also near-real-time over Stratum — the server pushes a status hash whenever the script’s history changes. On mobile networks, the `ping()` → `script_pop()` cycle adds ~1-2s latency. During disconnect/reconnect windows, the `reconcile_contract_scripts_fallback` re-subscribes and diffs against last-known state, adding 10s-60s detection delay. For a Taker (active-swap monitoring, not 24/7 daemon monitoring), this is an acceptable trade-off.

---

## 6. Migration Path & Compatibility

### 6.1 Feature Flags

```toml
# Cargo.toml
[features]
default = ["rpc-backend"]
rpc-backend = ["bitcoincore-rpc"]
electrum-backend = ["electrum-client"]
```

Makers always compile with `rpc-backend`. Mobile takers compile with `electrum-backend` only.

### 6.2 What Changes Per Module

| Module | Change Required | Scope |
|---|---|---|
| `src/wallet/rpc.rs` | Refactor into `RpcBackend` implementing `BlockchainBackend` | Medium |
| `src/wallet/api.rs` | Replace `self.rpc.*` calls with `self.backend.*` | Large (2772 lines, ~40 call sites) |
| `src/wallet/fidelity.rs` | `get_block_count`, `get_block_header_info` → trait methods | Small |
| `src/wallet/funding.rs` | `list_unspent`, `lock_unspent` → trait methods | Small |
| `src/wallet/spend.rs` | `get_block_count`, `send_raw_transaction` → trait methods | Small |
| `src/wallet/ffi.rs` | Add `ElectrumConfig` to FFI types | Small |
| `src/taker/api.rs` | Accept `BackendConfig` instead of `RPCConfig` | Small |
| `src/taker/api2.rs` | Same as above | Small |
| `src/maker/api.rs` | No change (always RPC) | None |
| `src/watch_tower/` | Add Electrum polling path alongside ZMQ | Medium |

### 6.3 `lock_unspent` / `unlock_unspent_all` Handling

These are RPC-only concepts (soft-locking UTXOs in Core's wallet). On Electrum, the wallet manages locks **locally** in `WalletStore`:

```rust
// Added to WalletStore
pub locked_outpoints: HashSet<OutPoint>,
```

The `BlockchainBackend` trait does NOT include lock/unlock — it's handled at the `Wallet` layer above.

---

## 7. Decoy Address Generation Details

### 7.1 Derivation Strategy

Decoys must be indistinguishable from real wallet addresses to the server.

All decoys are derived from the wallet's own HD master key at high indices that the wallet will never reach organically. This is a **local-only** operation — no blockchain fetch, no network call. The derivation itself is just BIP-32 key derivation + script construction, which is CPU-only and instant (~1µs per address).

```rust
impl ElectrumBackend {
    /// Create a single fresh decoy entry. Pure HD derivation, no network.
    fn derive_fresh_decoy(&self) -> DecoyEntry {
        let mut rng = rand::thread_rng();
        let index = rng.gen_range(100_000..1_000_000u32);
        let keychain = if rng.gen_bool(0.5) {
            KeychainKind::External
        } else {
            KeychainKind::Internal
        };
        let addr_type = if rng.gen_bool(0.5) {
            AddressType::P2WPKH
        } else {
            AddressType::P2TR
        };
        let script = self.derive_script_at_index(keychain, addr_type, index);
        DecoyEntry {
            script,
            hd_index: index,
            keychain,
            addr_type,
            times_used: 0,
            created_at: now_unix_secs(),
            // Each decoy gets its own random max-use limit
            retire_after_uses: rng.gen_range(5..=15),
        }
    }
}
```

### 7.2 Decoy Lifecycle: Cache & Rotation

Decoys cannot be all-fresh every sync (a server seeing 50 brand-new never-before-queried scripts each time is itself a fingerprint). They also cannot be all-reused every time (the stable set becomes clearly identifiable as decoys). The solution is a **persistent decoy cache** with gradual rotation.

#### 7.2.1 Cache Structure

```rust
impl DecoyCache {
    /// Persisted to wallet file alongside WalletStore.
    /// Loaded on startup, saved after every sync.
}
```

The cache is stored in the encrypted wallet file (same `WalletStore` serialization path). On first-ever sync, the cache starts empty and is fully populated with fresh decoys. On subsequent syncs, the cache is loaded from disk and partially rotated.

#### 7.2.2 The Rotation Algorithm

Each sync goes through this lifecycle:

```
┌───────────────────────────────────────────────────────────────────┐
│               Decoy Rotation Per-Sync                             │
│                                                                   │
│  Cache has C decoys (e.g. 200).   This sync needs D (e.g. 50).   │
│                                                                   │
│  1. RETIRE expired decoys from cache:                             │
│     - times_used >= retire_after_uses  (per-entry random limit)   │
│     - age > max_age_secs  (e.g. 3 days)                           │
│     → Remove them. They are never queried again.                  │
│                                                                   │
│  2. Draw rotation_pct ← rand(rotation_pct_range)  (e.g. 0.15–0.35)│
│     fresh_count  = floor(D × rotation_pct)     (e.g. 10)         │
│     reused_count = D - fresh_count             (e.g. 40)         │
│                                                                   │
│  3. REUSE: sample `reused_count` from surviving cache entries     │
│     → Increment their `times_used`                                │
│     → These are decoys the server has seen before — looks like    │
│       a real wallet that keeps checking its own addresses.         │
│                                                                   │
│  4. FRESH: generate `fresh_count` brand-new decoy entries         │
│     → Pure HD derivation, no network calls                        │
│     → Add them to the cache                                       │
│                                                                   │
│  5. TRIM: if cache exceeds max_cache_size, drop oldest entries    │
│     by (created_at + times_used weight) — preferring to drop      │
│     heavily-used decoys that have served their purpose.            │
│                                                                   │
│  6. Combine: batch = real_scripts + reused_decoys + fresh_decoys  │
│     → Shuffle, subscribe, proceed as normal                       │
│                                                                   │
│  7. SAVE cache to disk after sync completes                       │
│                                                                   │
└───────────────────────────────────────────────────────────────────┘
```

#### 7.2.3 Implementation

```rust
impl DecoyCache {
    /// Select decoys for a single batch: mix of cached + fresh.
    /// Returns the scripts to use AND mutates the cache state.
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
        let rotation_pct = rng.gen_range(self.rotation_pct_range.0..=self.rotation_pct_range.1);
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
            let entry = self.backend.derive_fresh_decoy();
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

#### 7.2.4 Why This Works

| Adversary observation | All-fresh decoys | All-reused decoys | Cache + rotation (our approach) |
|---|---|---|---|
| "50 never-seen scripts every sync" | Obvious decoy pattern | — | ~35 known + ~15 new → looks like a growing wallet |
| "Same 50 scripts every sync" | — | Stable set = clearly decoys | Set drifts naturally over days |
| "Scripts that never transact" | All 50 are empty → suspicious | Same empty set forever → suspicious | Cache retires old empties, adds new → normal churn |
| Cross-sync correlation | No overlap to correlate | Perfect overlap → trivial linkage | Partial overlap (~70%) → ambiguous linkage |

The partial overlap mimics how real wallets behave: most addresses persist across syncs (your receiving addresses don't change every time you open the app), but new addresses appear as you use the wallet and old ones eventually drop off.

#### 7.2.5 Cache Persistence

The `DecoyCache` is serialized into the wallet file alongside `WalletStore`, protected by the same AES-256-GCM encryption.
It adds ~30 KB to the wallet file (200 entries × ~150 bytes each).
On first launch with a fresh wallet, the cache starts empty — the first sync is 100% fresh decoys, then subsequent syncs gradually introduce reuse.

### 7.3 Tunable Parameters

All batch/pool sizes are **ranges** — each batch draws a random value within the window so no two syncs or batches look identical on the wire.

| Parameter | Default Range | Trade-off |
|---|---|---|
| `decoy_pool_range` | (30, 70) | Wider/higher = more privacy, more bandwidth per batch |
| `real_batch_range` | (3, 8) | Narrower/lower = more privacy, more round-trips |
| `gap_limit_range` | (15, 30) | Wider = harder for server to fingerprint scan depth |
| `server_pool_size` | 8+ | More servers = harder to correlate batches |
| `tor_circuit_rotation` | Per-batch | Per-batch is strongest; per-sync is faster |
| `max_cache_size` | 200 | Larger = more decoy diversity, slightly more disk |
| `rotation_pct_range` | (0.15, 0.35) | Higher = faster churn, less cross-sync overlap |
| `max_uses` | 5–15 (per entry) | Lower = faster retirement, more fresh decoys |
| `max_age_secs` | 259200 (3 days) | Shorter = less reuse risk, more derivation work |

### 7.4 Bandwidth Analysis

For a wallet with 30 real addresses (using midpoint of default ranges):
- Average real per batch: ~5.5, average decoy per batch: ~50
- Batches: `ceil(30 / 5.5) ≈ 6`
- Average subscribe calls per batch: `5.5 + 50 = 55.5`
- Total subscribe calls: `6 × 55.5 ≈ 333`
- Each `script_subscribe` response: ~50 bytes (status hash or null)
- Follow-up `script_list_unspent` for active scripts: ~200 bytes (empty) to ~2KB (with history)
- **Subscribe phase**: 333 × 50B = **~17 KB**
- **UTXO detail phase** (only active scripts): ~30 × 2KB = **~60 KB** worst case
- **Total worst case**: **~77 KB per sync** — negligible on mobile.

The subscribe-first approach is significantly lighter than polling `list_unspent` for all scripts, since most decoys return `None` (no history) and never need a follow-up UTXO query.
---