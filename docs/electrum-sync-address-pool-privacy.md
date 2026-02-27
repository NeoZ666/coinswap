# Electrum Sync & Address-Pool Privacy Layer for Coinswap Taker

> Proposal for light-client wallet sync via Electrum servers with query obfuscation through randomized address pools, enabling mobile (no full-node) operation while preserving privacy.

---

## 1. Problem Statement

The current `Wallet` implementation is tightly coupled to Bitcoin Core RPC. Every sync, UTXO lookup, descriptor import, and transaction broadcast flows through `bitcoincore_rpc::Client` (see `src/wallet/rpc.rs`). This requires:

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

/// Every blockchain operation the Wallet needs, independent of data source.
pub trait BlockchainBackend: Send + Sync {
    /// Fetch the current best-block height.
    fn get_block_count(&self) -> Result<u64, WalletError>;

    /// Fetch a block header by height.
    fn get_block_header(&self, height: u64) -> Result<BlockHeader, WalletError>;

    /// Register a set of script pubkeys for monitoring.
    /// For RPC this is `importdescriptors`; for Electrum this is
    /// `blockchain.scripthash.subscribe`.
    fn register_scripts(&self, scripts: &[ScriptBuf]) -> Result<(), WalletError>;

    /// Return all unspent outputs whose scriptPubKey matches any registered script.
    fn list_unspent(&self, scripts: &[ScriptBuf]) -> Result<Vec<ListUnspentEntry>, WalletError>;

    /// Fetch raw transaction by txid.
    fn get_raw_transaction(&self, txid: &Txid) -> Result<Transaction, WalletError>;

    /// Check if a specific outpoint is still unspent (for contract monitoring).
    fn get_tx_out(&self, outpoint: &OutPoint) -> Result<Option<TxOut>, WalletError>;

    /// Broadcast a signed transaction. Returns the txid on success.
    fn send_raw_transaction(&self, tx: &Transaction) -> Result<Txid, WalletError>;

    /// Estimate fee rate (sat/vB) for target confirmation blocks.
    fn estimate_fee(&self, target_blocks: u16) -> Result<f64, WalletError>;

    /// Poll transaction confirmation status.
    fn get_tx_confirmations(&self, txid: &Txid) -> Result<u32, WalletError>;
}
```

**Key design constraint:** The trait surface must be minimal — only operations the wallet actually uses. The ~20 distinct RPC calls currently scattered across `rpc.rs`, `api.rs`, `fidelity.rs`, `spend.rs`, and `funding.rs` collapse into these primitives.

### 2.2 RPC Backend (Existing Path, Refactored)

```rust
// src/wallet/rpc_backend.rs

pub struct RpcBackend {
    client: Client,  // bitcoincore_rpc::Client
    wallet_name: String,
}

impl BlockchainBackend for RpcBackend {
    fn list_unspent(&self, _scripts: &[ScriptBuf]) -> Result<Vec<ListUnspentEntry>, WalletError> {
        // Existing logic: unlock_unspent_all() + list_unspent(0, 9999999)
        // Scripts param ignored — Core tracks them via importdescriptors
    }
    fn register_scripts(&self, scripts: &[ScriptBuf]) -> Result<(), WalletError> {
        // Existing importdescriptors logic from rpc.rs:176
    }
    // ... etc
}
```

No behavioral change for existing maker/taker setups. Makers **always** use RPC (they need a full node for contract monitoring and ZMQ).

### 2.3 Electrum Backend with Address-Pool Privacy

```rust
// src/wallet/electrum_backend.rs

pub struct ElectrumBackend {
    /// Pool of Electrum server URLs (Tor .onion preferred).
    servers: Vec<String>,
    /// How many decoy addresses to mix per query batch.
    decoy_pool_size: usize,           // default: 50
    /// Max real addresses per batch (controls real:decoy ratio).
    real_batch_size: usize,            // default: 5
    /// Rotate server connection per batch.
    rotate_servers: bool,              // default: true
}
```

#### 2.3.1 The Address Randomization Pool

This is the core privacy mechanism. When the wallet needs to query the Electrum server for its addresses, it does **not** send them directly. Instead:

```
┌──────────────────────────────────────────────────────────┐
│                   Address Pool Query Flow                 │
│                                                          │
│  Wallet has N real addresses to check (e.g. 15)          │
│                                                          │
│  1. Split into batches of `real_batch_size` (5)          │
│     → Batch 1: [real_0..real_4]                          │
│     → Batch 2: [real_5..real_9]                          │
│     → Batch 3: [real_10..real_14]                        │
│                                                          │
│  2. For each batch, generate `decoy_pool_size` (50)      │
│     random valid-looking script hashes                    │
│     → Source: derive from HD path at random high indices  │
│       (m/84'/1'/0'/0/100000..999999) so they look like   │
│       real wallet addresses on the wire                   │
│                                                          │
│  3. Shuffle real + decoy into single request array        │
│     → [decoy_12, real_2, decoy_7, real_0, decoy_41, ...] │
│                                                          │
│  4. Send batch to Electrum server via                     │
│     `blockchain.scripthash.listunspent` (batched JSON)    │
│                                                          │
│  5. Server returns results for ALL 55 script hashes       │
│     → Most decoys return empty (no history)               │
│     → Some decoys may have real history (adds noise)      │
│                                                          │
│  6. Client filters: keep only results matching real set   │
│     → Discard all decoy responses                         │
│                                                          │
│  7. ROTATE server connection for next batch               │
│     → Each batch hits a DIFFERENT Electrum server         │
│     → No single server sees all real addresses            │
│                                                          │
└──────────────────────────────────────────────────────────┘
```

**Privacy properties:**
- **Per-batch dilution**: Server sees 55 queries, only 5 are real → 9% signal-to-noise per batch.
- **Cross-batch unlinkability**: Different servers per batch → no server sees the full wallet.
- **Decoy realism**: Decoys are derived from the wallet's own HD key at unused indices, so they're valid addresses with (mostly) no on-chain history — indistinguishable from real unused addresses from the server's perspective.
- **Server rotation via Tor**: Each batch routes through a different Tor circuit to a different Electrum server, preventing IP-based correlation.

#### 2.3.2 Electrum Protocol Mapping

| Wallet Operation | Electrum Method | Notes |
|---|---|---|
| `register_scripts` | `blockchain.scripthash.subscribe` | Subscribe + initial status check |
| `list_unspent` | `blockchain.scripthash.listunspent` | Batched with decoy pool |
| `get_raw_transaction` | `blockchain.transaction.get` | Returns raw hex |
| `send_raw_transaction` | `blockchain.transaction.broadcast` | Broadcast signed tx |
| `get_block_count` | `blockchain.headers.subscribe` | Returns tip height |
| `get_block_header` | `blockchain.block.header` | Returns raw header |
| `estimate_fee` | `blockchain.estimatefee` | Sat/kB, convert to sat/vB |
| `get_tx_confirmations` | `blockchain.transaction.get` (verbose) | Parse confirmations field |
| `get_tx_out` | `blockchain.scripthash.listunspent` | Check specific outpoint |

#### 2.3.3 Transaction Broadcast Privacy

Transaction broadcast gets the same pool treatment but in reverse — we broadcast through a **randomly selected** Electrum server over a **fresh Tor circuit**. This prevents the server from correlating "who queried these addresses" with "who broadcast this transaction."

For additional broadcast privacy, the wallet can optionally relay through **multiple** Electrum servers simultaneously (broadcast to 3-5 servers), making it harder to identify the originating node.

---

## 3. Wallet Struct Refactoring

### 3.1 Current Wallet Struct

```rust
pub struct Wallet {
    pub(crate) rpc: Client,                    // Hard-coded RPC
    pub(crate) wallet_file_path: PathBuf,
    pub(crate) store: WalletStore,
    pub(crate) store_enc_material: Option<KeyMaterial>,
}
```

### 3.2 Proposed Wallet Struct

```rust
pub struct Wallet {
    pub(crate) backend: Box<dyn BlockchainBackend>,   // Swappable backend
    pub(crate) wallet_file_path: PathBuf,
    pub(crate) store: WalletStore,
    pub(crate) store_enc_material: Option<KeyMaterial>,
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
    /// Number of decoy addresses per query batch.
    pub decoy_pool_size: usize,
    /// Number of real addresses per query batch.
    pub real_batch_size: usize,
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
┌─────────────────────────────────────────────────────────────────┐
│                  sync() — Electrum Backend                       │
│                                                                 │
│  1. Derive all wallet scriptPubKeys                             │
│     - HD external: m/84'/0'/0'/0/0..external_index+gap          │
│     - HD internal: m/84'/0'/0'/1/0..internal_index+gap          │
│     - P2TR external: m/86'/0'/0'/0/0..external_index+gap        │
│     - P2TR internal: m/86'/0'/0'/1/0..internal_index+gap        │
│     - All incoming_swapcoin multisig scripts                    │
│     - All outgoing_swapcoin multisig scripts                    │
│     - All contract scripts (hashlock/timelock)                  │
│     - Fidelity bond scripts                                     │
│                                                                 │
│  2. Split scripts into batches of `real_batch_size`             │
│                                                                 │
│  3. For each batch:                                             │
│     a. Select random Electrum server from pool                  │
│     b. Connect via Tor SOCKS5 proxy (new circuit)               │
│     c. Generate `decoy_pool_size` decoy script hashes           │
│     d. Shuffle real + decoy                                     │
│     e. Query blockchain.scripthash.listunspent (batch RPC)      │
│     f. Filter responses → keep only real script results          │
│     g. Disconnect                                               │
│                                                                 │
│  4. Aggregate all UTXO results                                  │
│     - Map to ListUnspentEntry-equivalent structs                │
│     - Derive UTXOSpendInfo for each (same logic as RPC path)    │
│                                                                 │
│  5. Update WalletStore                                          │
│     - update_utxo_cache(aggregated_utxos)                       │
│     - Update external_index via gap-limit scanning              │
│     - Refresh offer_maxsize                                     │
│     - Set last_synced_height from tip                           │
│                                                                 │
│  6. save_to_disk()                                              │
└─────────────────────────────────────────────────────────────────┘
```

### 4.1 Gap Limit Scanning (Electrum-Specific)

With RPC, the wallet relies on `importdescriptors` with a range and then Core's internal scanning. With Electrum, we implement gap-limit scanning ourselves:

```rust
const GAP_LIMIT: u32 = 20;

fn scan_keychain(&self, keychain: KeychainKind, address_type: AddressType) -> Result<u32, WalletError> {
    let mut last_used_index = 0u32;
    let mut consecutive_unused = 0u32;
    let mut index = 0u32;

    while consecutive_unused < GAP_LIMIT {
        let script = self.derive_script_at_index(keychain, address_type, index)?;
        // Query goes through address pool mechanism
        let history = self.backend.list_unspent(&[script])?;
        if history.is_empty() {
            consecutive_unused += 1;
        } else {
            last_used_index = index;
            consecutive_unused = 0;
        }
        index += 1;
    }
    Ok(last_used_index + 1)
}
```

This scans in batches (not one-by-one), with each batch padded with decoys.

---

## 5. Contract Monitoring Without ZMQ

On a full node, the Watch Tower uses ZMQ subscriptions (`zmqpubrawtx`, `zmqpubrawblock`) for real-time contract breach detection. On Electrum, contract monitoring should be **subscription-first** via Electrum Stratum notifications, with polling only as a fallback:

```rust
impl ElectrumBackend {
    /// Subscribe to all contract script-hashes and return spend events
    /// when status notifications arrive.
    fn monitor_contract_scripts(
        &self,
        contract_scripts: &[ScriptBuf],
    ) -> Result<Vec<ContractSpendEvent>, WalletError> {
        // 1) Subscribe using blockchain.scripthash.subscribe
        // 2) Wait for push notifications from server on status change
        // 3) On notification, fetch details (history/unspents/raw tx)
        // 4) Classify spend path (hashlock/timelock/keypath)
    }

    /// Fallback path for mobile reliability when subscription channel is down:
    /// reconnect and do periodic state reconciliation.
    fn reconcile_contract_scripts_fallback(
        &self,
        contract_scripts: &[ScriptBuf],
    ) -> Result<Vec<ContractSpendEvent>, WalletError> {
        // Re-query state via listunspent/history after reconnect
        // and diff against last-known contract state.
    }
}
```

**Latency trade-off**: ZMQ gives sub-second notification on a local full node. Electrum subscriptions are also near-real-time over Stratum, but can be less deterministic over mobile networks. During disconnect/reconnect windows, fallback reconciliation behaves like polling and can add 10s-60s detection delay. For a Taker (active-swap monitoring, not 24/7 daemon monitoring), this is an acceptable trade-off.

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

Decoys must be indistinguishable from real wallet addresses to the server:

```rust
impl ElectrumBackend {
    fn generate_decoys(&self, wallet: &Wallet, count: usize) -> Vec<ScriptBuf> {
        let mut rng = rand::thread_rng();
        let mut decoys = Vec::with_capacity(count);

        for _ in 0..count {
            // Pick random high index that wallet will never reach organically
            let index = rng.gen_range(100_000..1_000_000);
            // Alternate between external/internal, P2WPKH/P2TR
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
            let script = wallet.derive_script_at_index(keychain, addr_type, index);
            decoys.push(script);
        }
        decoys
    }
}
```

### 7.2 Tunable Parameters

| Parameter | Default | Trade-off |
|---|---|---|
| `decoy_pool_size` | 50 | Higher = more privacy, more bandwidth |
| `real_batch_size` | 5 | Lower = more privacy, more round-trips |
| `server_pool_size` | 8+ | More servers = harder to correlate batches |
| `tor_circuit_rotation` | Per-batch | Per-batch is strongest; per-sync is faster |

### 7.3 Bandwidth Analysis

For a wallet with 30 real addresses:
- Batches: `ceil(30/5) = 6`
- Queries per batch: `5 + 50 = 55`
- Total queries: `6 × 55 = 330`
- Each `blockchain.scripthash.listunspent` response: ~200 bytes (empty) to ~2KB (with history)
- **Worst case**: 330 × 2KB = **660 KB per sync** — negligible on mobile.

---

## 8. Security Considerations

### 8.1 Electrum Server Trust Model

Electrum servers can:
- **See which addresses are queried** → mitigated by decoy pool + server rotation.
- **Lie about UTXO state** (omit UTXOs, fabricate fake ones) → **critical for contract safety.**

Mitigation for UTXO lying:
- Query **multiple independent servers** for contract-critical UTXOs and require **consensus** (e.g., 2-of-3 agree).
- For non-contract UTXOs (regular balance display), single-server is acceptable.

### 8.2 Tor Integration

All Electrum connections MUST go through Tor. The `ElectrumConfig.tor_proxy` field is mandatory on mainnet:

```rust
impl ElectrumBackend {
    fn connect(&self, server: &str) -> Result<ElectrumClient, WalletError> {
        let proxy = self.tor_proxy.as_ref()
            .ok_or(WalletError::General("Tor proxy required for Electrum".into()))?;
        ElectrumClient::new_proxy(server, proxy)
    }
}
```

### 8.3 Swap-Critical Operations

During an active coinswap, certain operations are time-sensitive (contract monitoring, hashlock/timelock recovery). For these:
- Poll at higher frequency (every 10s instead of 30s).
- Query 3 independent servers in parallel.
- If any server reports a contract spend, immediately verify against another server before acting.

---

## 9. Dependency Additions

```toml
[dependencies]
# Electrum client (only with electrum-backend feature)
electrum-client = { version = "0.21", optional = true }
```

The `electrum-client` crate supports:
- SSL/TLS connections
- SOCKS5 proxy (Tor)
- Batched requests
- Both TCP and SSL transports

---

## 10. Appendix: Compact Block Filters / BIP 157

For completeness: **BIP 157 (Neutrino)** is an alternative light-client approach where the client downloads compact block filters and scans locally, never revealing addresses to any server. This provides **strictly better privacy** than Electrum (even with the decoy pool).

However, BIP 157 has practical limitations in this context:
- **No production-ready Rust client library** with Tor support.
- **Higher bandwidth**: downloading all filters for a rescan is ~5 GB for mainnet history.
- **Slower initial sync**: scanning all filters takes minutes vs seconds for Electrum.
- **Requires Bitcoin Core nodes serving filters** (`-blockfilterindex=1 -peerblockfilters=1`), which is less widely deployed than Electrum servers.

BIP 157 could be added as a **third `BlockchainBackend` implementation** in the future without any architectural changes — the trait abstraction supports it cleanly. It would be the recommended backend for privacy-maximizing desktop takers who don't want to run a full node but can tolerate the bandwidth. For mobile, Electrum with the decoy pool remains the pragmatic choice.
