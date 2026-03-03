# Coinswap Android App — Architecture via coinswap-kotlin FFI

> Proposal for a native Android application built on `citadel-tech/coinswap-ffi/coinswap-kotlin` (UniFFI bindings), leveraging the Electrum sync backend for full-node-free mobile operation.

---

## 1. Scope

Build an Android Taker wallet that:
- Performs coinswaps (Legacy V1 + Taproot V2) entirely from a phone.
- Syncs via Electrum servers (no local Bitcoin Core node).
- Routes all traffic through Tor.
- Uses the existing `coinswap-kotlin` UniFFI bindings as the sole interface to the Rust core.

This document covers only the Android-specific architecture. The Electrum backend and address-pool privacy layer are specified in [electrum-sync-address-pool-privacy.md](electrum-sync-address-pool-privacy.md).

---

## 2. Dependency Chain

```
┌──────────────────────────────────────────┐
│            Android App (Kotlin)           │
│  Jetpack Compose UI + ViewModel layer     │
├──────────────────────────────────────────┤
│         coinswap-kotlin bindings          │
│  UniFFI-generated Kotlin classes          │
│  Taker, TaprootTaker, SwapParams, etc.   │
├──────────────────────────────────────────┤
│          libcoinswap_ffi.so               │
│  Native library (arm64-v8a, armeabi-v7a,  │
│  x86_64) built from ffi-commons crate    │
├──────────────────────────────────────────┤
│            coinswap (Rust core)           │
│  Wallet, Protocol, Taker logic            │
│  + ElectrumBackend (new)                  │
└──────────────────────────────────────────┘
```

The app **never** calls Rust directly. All interaction goes through the UniFFI-generated Kotlin classes in `coinswap-kotlin/lib/src/main/kotlin/org/coinswap/coinswap.kt`.

---

## 3. FFI Surface — What's Available Today

From `ffi-commons/src/taker.rs` and `ffi-commons/src/taproot_taker.rs`, the current UniFFI exports:

```kotlin
// Constructor
Taker.init(dataDir, walletFileName, rpcConfig, controlPort, torAuthPassword, zmqAddr, password): Taker

// Wallet ops
taker.syncAndSave()
taker.getBalances(): Balances
taker.getNextExternalAddress(addressType): Address
taker.getNextInternalAddresses(count, addressType): List<Address>
taker.listAllUtxoSpendInfo(): List<TotalUtxoInfo>
taker.sendToAddress(address, amount, feeRate, manuallySelectedOutpoints): Txid
taker.getTransactions(count, skip): List<ListTransactionResult>
taker.backup(destinationPath, password)
taker.lockUnspendableUtxos()
taker.getWalletName(): String

// Swap ops
taker.doCoinswap(swapParams): SwapReport?
taker.fetchOffers(): OfferBook
taker.isOfferbookSyncing(): Boolean
taker.recoverFromSwap()

// Logging
taker.setupLogging(dataDir, logLevel)
```

### 3.1 Standalone Functions

```kotlin
fetchMempoolFees(): FeeRates
restoreWalletGuiApp(dataDir, walletFileName, rpcConfig, backupFilePath, password)
isWalletEncrypted(walletPath): Boolean
createDefaultRpcConfig(): RPCConfig
setupLogging(dataDir)
```

---

## 4. FFI Changes Required

### 4.1 New: `ElectrumConfig` Type

Add to `ffi-commons/src/types.rs`:

```rust
#[derive(Debug, Clone, uniffi::Record)]
pub struct ElectrumConfig {
    /// Electrum server URLs (prefer .onion)
    pub servers: Vec<String>,
    /// SOCKS5 proxy address for Tor (e.g. "127.0.0.1:9050")
    pub tor_proxy: String,
    /// Number of decoy addresses per query batch
    pub decoy_pool_size: u32,
    /// Number of real addresses per query batch
    pub real_batch_size: u32,
    /// Network: "signet", "testnet", "mainnet"
    pub network: String,
}
```

### 4.2 New: `BackendConfig` Enum

```rust
#[derive(Debug, Clone, uniffi::Enum)]
pub enum BackendConfig {
    Rpc { config: RPCConfig },
    Electrum { config: ElectrumConfig },
}
```

### 4.3 Updated Taker Constructor

```rust
// Replace rpc_config: Option<RPCConfig> with backend_config: Option<BackendConfig>
pub fn init(
    data_dir: Option<String>,
    wallet_file_name: Option<String>,
    backend_config: Option<BackendConfig>,  // NEW
    control_port: Option<u16>,
    tor_auth_password: Option<String>,
    password: Option<String>,
) -> Result<Arc<Self>, TakerError>
```

The `zmq_addr` parameter is dropped for Electrum mode (no ZMQ needed). For RPC mode, it remains as part of a separate watch-tower config.

### 4.4 New: Privacy Config Adjustment

```rust
#[derive(Debug, Clone, uniffi::Record)]
pub struct PrivacyConfig {
    /// Decoy pool size for address queries
    pub decoy_pool_size: u32,
    /// Real addresses per batch
    pub real_batch_size: u32,
    /// Whether to rotate Tor circuits per batch
    pub rotate_circuits: bool,
    /// Number of servers to broadcast transactions through
    pub broadcast_redundancy: u32,
}

// Method on Taker
pub fn update_privacy_config(&self, config: PrivacyConfig) -> Result<(), TakerError>;
```

---

## 5. Android App Architecture

### 5.1 Module Structure

```
app/
├── src/main/
│   ├── java/org/coinswap/
│   │   ├── coinswap.kt                    # UniFFI generated bindings
│   │   └── app/
│   │       ├── CoinswapApp.kt             # Application class
│   │       ├── di/
│   │       │   └── AppModule.kt           # Hilt DI module
│   │       ├── data/
│   │       │   ├── TakerRepository.kt     # Bridge: FFI ↔ ViewModel
│   │       │   ├── TorManager.kt          # Tor lifecycle (Arti/Orbot)
│   │       │   └── ElectrumServerList.kt  # Hardcoded + dynamic server list
│   │       ├── domain/
│   │       │   ├── SyncWalletUseCase.kt
│   │       │   ├── DoCoinswapUseCase.kt
│   │       │   ├── RecoverSwapUseCase.kt
│   │       │   └── SendBitcoinUseCase.kt
│   │       ├── ui/
│   │       │   ├── theme/
│   │       │   │   └── Theme.kt
│   │       │   ├── navigation/
│   │       │   │   └── NavGraph.kt
│   │       │   ├── onboarding/
│   │       │   │   ├── CreateWalletScreen.kt
│   │       │   │   ├── RestoreWalletScreen.kt
│   │       │   │   └── OnboardingViewModel.kt
│   │       │   ├── home/
│   │       │   │   ├── HomeScreen.kt
│   │       │   │   ├── HomeViewModel.kt
│   │       │   │   └── BalanceCard.kt
│   │       │   ├── swap/
│   │       │   │   ├── SwapScreen.kt
│   │       │   │   ├── SwapViewModel.kt
│   │       │   │   ├── OfferBookSheet.kt
│   │       │   │   └── SwapProgressIndicator.kt
│   │       │   ├── send/
│   │       │   │   ├── SendScreen.kt
│   │       │   │   └── SendViewModel.kt
│   │       │   ├── receive/
│   │       │   │   ├── ReceiveScreen.kt
│   │       │   │   └── ReceiveViewModel.kt
│   │       │   ├── utxo/
│   │       │   │   ├── UtxoListScreen.kt
│   │       │   │   └── UtxoViewModel.kt
│   │       │   ├── settings/
│   │       │   │   ├── SettingsScreen.kt
│   │       │   │   ├── SettingsViewModel.kt
│   │       │   │   ├── PrivacySettingsScreen.kt
│   │       │   │   └── BackupRestoreScreen.kt
│   │       │   └── transactions/
│   │       │       ├── TransactionListScreen.kt
│   │       │       └── TransactionsViewModel.kt
│   │       └── service/
│   │           ├── SyncWorker.kt           # WorkManager periodic sync
│   │           └── SwapForegroundService.kt # Keep alive during swap
│   ├── jniLibs/
│   │   ├── arm64-v8a/libcoinswap_ffi.so
│   │   ├── armeabi-v7a/libcoinswap_ffi.so
│   │   └── x86_64/libcoinswap_ffi.so
│   └── AndroidManifest.xml
├── build.gradle.kts
└── proguard-rules.pro
```

---

## 6. Core Components

### 6.1 TakerRepository — The FFI Bridge

All FFI calls happen through a single repository, off the main thread via `Dispatchers.IO`:

```kotlin
@Singleton
class TakerRepository @Inject constructor(
    private val torManager: TorManager,
) {
    private var taker: Taker? = null
    private var taprootTaker: TaprootTaker? = null

    private val _syncState = MutableStateFlow<SyncState>(SyncState.Idle)
    val syncState: StateFlow<SyncState> = _syncState

    suspend fun initialize(
        dataDir: String,
        walletName: String?,
        electrumConfig: ElectrumConfig,
        password: String?
    ) = withContext(Dispatchers.IO) {
        torManager.ensureRunning()

        val backendConfig = BackendConfig.Electrum(electrumConfig)

        taker = Taker.init(
            dataDir = dataDir,
            walletFileName = walletName,
            backendConfig = backendConfig,
            controlPort = torManager.controlPort,
            torAuthPassword = torManager.authPassword,
            password = password
        )

        taprootTaker = TaprootTaker.init(
            dataDir = dataDir,
            walletFileName = walletName,
            backendConfig = backendConfig,
            controlPort = torManager.controlPort,
            torAuthPassword = torManager.authPassword,
            password = password
        )
    }

    suspend fun syncWallet() = withContext(Dispatchers.IO) {
        _syncState.value = SyncState.Syncing
        try {
            taker?.syncAndSave()
            _syncState.value = SyncState.Synced(System.currentTimeMillis())
        } catch (e: TakerError) {
            _syncState.value = SyncState.Error(e.message ?: "Sync failed")
        }
    }

    suspend fun getBalances(): Balances = withContext(Dispatchers.IO) {
        taker?.getBalances() ?: throw IllegalStateException("Taker not initialized")
    }

    suspend fun doCoinswap(
        amount: Long,
        makerCount: Int,
        selectedUtxos: List<OutPoint>?,
        protocol: SwapProtocol
    ): SwapReport? = withContext(Dispatchers.IO) {
        when (protocol) {
            SwapProtocol.LEGACY -> {
                val params = SwapParams(
                    sendAmount = amount.toULong(),
                    makerCount = makerCount.toUInt(),
                    manuallySelectedOutpoints = selectedUtxos
                )
                taker?.doCoinswap(params)
            }
            SwapProtocol.TAPROOT -> {
                val params = TaprootSwapParams(
                    sendAmount = amount.toULong(),
                    makerCount = makerCount.toUInt(),
                    txCount = 1u,
                    requiredConfirms = 1u,
                    manuallySelectedOutpoints = selectedUtxos
                )
                taprootTaker?.doCoinswap(params)
            }
        }
    }

    // ... sendToAddress, getUtxos, getTransactions, backup, restore, etc.
}

enum class SwapProtocol { LEGACY, TAPROOT }

sealed class SyncState {
    object Idle : SyncState()
    object Syncing : SyncState()
    data class Synced(val timestamp: Long) : SyncState()
    data class Error(val message: String) : SyncState()
}
```

### 6.2 Tor Integration

The app embeds a Tor client for all network traffic. Two options:

**Option A: Arti (Rust Tor, embedded)**
- Ship `libarti.so` alongside `libcoinswap_ffi.so`.
- Start/stop programmatically. Provides SOCKS5 on `127.0.0.1:9050`.
- Advantage: no external dependency. Disadvantage: larger APK (~8 MB).

**Option B: Orbot integration**
- Check if Orbot is installed. If not, prompt user to install.
- Use Orbot's SOCKS5 proxy.
- Advantage: shared Tor instance. Disadvantage: external dependency.

```kotlin
@Singleton
class TorManager @Inject constructor(@ApplicationContext private val context: Context) {
    var controlPort: UShort = 9051u
    var authPassword: String = ""
    private var socksPort: Int = 9050

    fun getSocksProxy(): String = "127.0.0.1:$socksPort"

    suspend fun ensureRunning() {
        // Check Orbot, or start embedded Arti
        // Block until SOCKS5 port is accepting connections
    }
}
```

### 6.3 Background Sync (WorkManager)

Periodic sync keeps the wallet state fresh even when the app isn't in the foreground:

```kotlin
class SyncWorker(
    context: Context,
    params: WorkerParameters,
    private val takerRepository: TakerRepository
) : CoroutineWorker(context, params) {

    override suspend fun doWork(): Result {
        return try {
            takerRepository.syncWallet()
            Result.success()
        } catch (e: Exception) {
            if (runAttemptCount < 3) Result.retry() else Result.failure()
        }
    }

    companion object {
        fun enqueuePeriodicSync(context: Context) {
            val request = PeriodicWorkRequestBuilder<SyncWorker>(
                repeatInterval = 15, TimeUnit.MINUTES
            ).setConstraints(
                Constraints.Builder()
                    .setRequiredNetworkType(NetworkType.CONNECTED)
                    .build()
            ).build()

            WorkManager.getInstance(context)
                .enqueueUniquePeriodicWork("wallet_sync", ExistingPeriodicWorkPolicy.KEEP, request)
        }
    }
}
```

### 6.4 Swap Foreground Service

Coinswaps take 30-120 seconds and involve multi-round communication. The process must not be killed by the OS:

```kotlin
class SwapForegroundService : LifecycleService() {

    @Inject lateinit var takerRepository: TakerRepository

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        super.onStartCommand(intent, flags, startId)

        val notification = createNotification("Coinswap in progress...")
        startForeground(SWAP_NOTIFICATION_ID, notification)

        lifecycleScope.launch(Dispatchers.IO) {
            try {
                val params = intent?.getParcelableExtra<SwapIntentParams>("params")
                    ?: return@launch

                val report = takerRepository.doCoinswap(
                    amount = params.amount,
                    makerCount = params.makerCount,
                    selectedUtxos = params.selectedUtxos,
                    protocol = params.protocol
                )

                updateNotification("Swap complete! Fee: ${report?.totalFee} sats")
            } catch (e: TakerError) {
                updateNotification("Swap failed: ${e.message}")
                // Trigger recovery flow
            } finally {
                stopForeground(STOP_FOREGROUND_REMOVE)
                stopSelf()
            }
        }

        return START_NOT_STICKY
    }
}
```

---

## 7. UI Screens

### 7.1 Screen Flow

```
┌─────────────┐     ┌──────────────┐     ┌──────────────┐
│  Onboarding  │────▶│  Create or   │────▶│    Home      │
│   (first     │     │  Restore     │     │   Screen     │
│    launch)   │     │  Wallet      │     │              │
└─────────────┘     └──────────────┘     └──────┬───────┘
                                                │
                    ┌───────────────────────────┼────────────────────┐
                    │               │           │          │         │
              ┌─────▼──┐    ┌──────▼───┐  ┌────▼────┐ ┌──▼───┐ ┌──▼──────┐
              │  Send   │    │ Receive  │  │  Swap   │ │ UTXO │ │ Settings│
              │ Screen  │    │ Screen   │  │ Screen  │ │ List │ │  Screen │
              └─────────┘    └──────────┘  └────┬────┘ └──────┘ └─────────┘
                                                │
                                          ┌─────▼──────┐
                                          │ Offerbook  │
                                          │  Browser   │
                                          └────────────┘
```

### 7.2 Home Screen

```kotlin
@Composable
fun HomeScreen(viewModel: HomeViewModel = hiltViewModel()) {
    val balances by viewModel.balances.collectAsState()
    val syncState by viewModel.syncState.collectAsState()

    Column {
        // Sync status indicator
        SyncStatusBar(syncState)

        // Balance card
        BalanceCard(
            spendable = balances.spendable,
            regular = balances.regular,
            swap = balances.swap,
            contract = balances.contract,
            fidelity = balances.fidelity
        )

        // Quick actions
        Row {
            ActionButton("Send", Icons.Send) { navController.navigate("send") }
            ActionButton("Receive", Icons.QrCode) { navController.navigate("receive") }
            ActionButton("Swap", Icons.SwapHoriz) { navController.navigate("swap") }
        }

        // Recent transactions
        TransactionList(viewModel.recentTransactions)
    }
}
```

### 7.3 Swap Screen

```kotlin
@Composable
fun SwapScreen(viewModel: SwapViewModel = hiltViewModel()) {
    val swapState by viewModel.swapState.collectAsState()
    val feeRates by viewModel.feeRates.collectAsState()
    val offerbook by viewModel.offerbook.collectAsState()

    Column {
        // Amount input
        AmountInput(
            value = viewModel.amount,
            onValueChange = viewModel::setAmount,
            maxAmount = viewModel.maxSwapAmount
        )

        // Protocol selector
        ProtocolSelector(
            selected = viewModel.protocol,
            onSelect = viewModel::setProtocol
        )

        // Maker count
        MakerCountSlider(
            count = viewModel.makerCount,
            onCountChange = viewModel::setMakerCount,
            range = 2..5
        )

        // Fee estimate
        FeeEstimate(
            amount = viewModel.amount,
            makerCount = viewModel.makerCount,
            feeRates = feeRates,
            offerbook = offerbook
        )

        // UTXO selection (optional)
        if (viewModel.showUtxoSelector) {
            UtxoSelector(
                utxos = viewModel.availableUtxos,
                selected = viewModel.selectedUtxos,
                onToggle = viewModel::toggleUtxo
            )
        }

        // Swap button
        SwapButton(
            enabled = viewModel.canSwap,
            state = swapState,
            onClick = viewModel::startSwap
        )

        // Progress during swap
        when (swapState) {
            is SwapState.InProgress -> SwapProgressIndicator(swapState.phase)
            is SwapState.Completed -> SwapReportCard(swapState.report)
            is SwapState.Failed -> SwapErrorCard(swapState.error, onRecover = viewModel::recover)
            else -> {}
        }
    }
}
```

### 7.4 Privacy Settings Screen

```kotlin
@Composable
fun PrivacySettingsScreen(viewModel: SettingsViewModel = hiltViewModel()) {
    Column {
        SectionHeader("Electrum Sync Privacy")

        SliderSetting(
            label = "Decoy Pool Size",
            description = "Random addresses mixed with real queries. Higher = more private, more bandwidth.",
            value = viewModel.decoyPoolSize,
            range = 20f..200f,
            onValueChange = viewModel::setDecoyPoolSize
        )

        SliderSetting(
            label = "Batch Size",
            description = "Real addresses per query batch. Lower = more private, slower sync.",
            value = viewModel.realBatchSize,
            range = 1f..20f,
            onValueChange = viewModel::setRealBatchSize
        )

        SwitchSetting(
            label = "Rotate Tor Circuits",
            description = "Use a different Tor circuit for each batch query.",
            checked = viewModel.rotateCircuits,
            onCheckedChange = viewModel::setRotateCircuits
        )

        SectionHeader("Electrum Servers")

        ServerList(
            servers = viewModel.electrumServers,
            onAdd = viewModel::addServer,
            onRemove = viewModel::removeServer
        )
    }
}
```

---

## 8. Build Pipeline

### 8.1 Native Library Compilation

The `libcoinswap_ffi.so` must be compiled with `electrum-backend` feature:

```bash
# In coinswap-ffi/ffi-commons/
cd ffi-commons

# Android targets
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android

# Build with electrum feature
CARGO_FEATURE_FLAGS="--features electrum-backend"

cargo build --release --target aarch64-linux-android $CARGO_FEATURE_FLAGS
cargo build --release --target armv7-linux-androideabi $CARGO_FEATURE_FLAGS
cargo build --release --target x86_64-linux-android $CARGO_FEATURE_FLAGS

# Generate Kotlin bindings
./create_bindings.sh
```

### 8.2 Android build.gradle.kts

```kotlin
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("dagger.hilt.android.plugin")
    id("kotlin-kapt")
}

android {
    namespace = "org.coinswap.app"
    compileSdk = 34

    defaultConfig {
        applicationId = "org.coinswap.app"
        minSdk = 24
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0-alpha"
    }

    sourceSets {
        getByName("main") {
            jniLibs.srcDirs("src/main/jniLibs")
        }
    }

    buildFeatures {
        compose = true
    }
}

dependencies {
    // Compose
    implementation(platform("androidx.compose:compose-bom:2024.10.00"))
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.navigation:navigation-compose:2.7.7")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.7.0")

    // Hilt
    implementation("com.google.dagger:hilt-android:2.50")
    kapt("com.google.dagger:hilt-compiler:2.50")
    implementation("androidx.hilt:hilt-navigation-compose:1.1.0")
    implementation("androidx.hilt:hilt-work:1.1.0")

    // WorkManager
    implementation("androidx.work:work-runtime-ktx:2.9.0")

    // Tor (Arti)
    implementation("org.AnonTech:AnonLib:0.4.8")

    // QR code
    implementation("com.journeyapps:zxing-android-embedded:4.3.0")

    // Net
    implementation("net.zetetic:android-database-sqlcipher:4.5.4")
}
```

### 8.3 CI Workflow

```yaml
# .github/workflows/android-build.yml
name: Android Build
on:
  push:
    paths:
      - 'app/**'
      - 'coinswap-kotlin/**'
      - 'ffi-commons/**'

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
          targets: aarch64-linux-android,armv7-linux-androideabi,x86_64-linux-android

      - name: Setup Android SDK
        uses: android-actions/setup-android@v3

      - name: Setup NDK
        uses: nttld/setup-ndk@v1
        with:
          ndk-version: r26b

      - name: Build native libraries
        run: |
          cd ffi-commons
          cargo build --release --target aarch64-linux-android --features electrum-backend
          cargo build --release --target x86_64-linux-android --features electrum-backend

      - name: Generate bindings
        run: |
          cd ffi-commons
          ./create_bindings.sh

      - name: Build APK
        run: |
          cd app
          ./gradlew assembleDebug
```

---

## 9. Data Flow for Key Operations

### 9.1 Wallet Sync

```
User taps "Refresh"
    → HomeViewModel.refresh()
    → TakerRepository.syncWallet()          [Dispatchers.IO]
    → taker.syncAndSave()                   [FFI call → Rust]
    → Wallet.sync()                         [Rust: ElectrumBackend]
        → derive all scriptPubKeys
        → batch + pad with decoys
        → query N Electrum servers via Tor
        → update UTXO cache
        → save to CBOR on disk
    → FFI returns                           [Kotlin]
    → _syncState.emit(Synced)
    → UI recomposes with new balances
```

### 9.2 Coinswap Execution

```
User configures swap and taps "Swap"
    → SwapViewModel.startSwap()
    → Start SwapForegroundService
    → TakerRepository.doCoinswap(amount, makerCount, protocol)
    → taker.doCoinswap(swapParams)          [FFI → Rust]
    → Taker.do_coinswap()                   [Rust: full protocol]
        → fetch_offers() via Tor
        → select makers
        → initalize_coinswap() → funding txs
        → send_tx() via ElectrumBackend.send_raw_transaction()
        → exchange messages with makers (multi-round)
        → receive incoming swapcoins
        → sweep_incoming_swapcoins()
        → sync_and_save()
    → SwapReport returned via FFI
    → SwapForegroundService posts notification
    → UI shows SwapReportCard
```

### 9.3 Swap Recovery

```
App detects unfinished swap (on startup or manual trigger)
    → RecoverSwapUseCase.execute()
    → taker.recoverFromSwap()               [FFI → Rust]
    → find_unfinished_swapcoins()
    → For incoming: spend_from_hashlock_contract() or wait for timelock
    → For outgoing: spend_from_timelock_contract()
    → All via ElectrumBackend (broadcast + confirmation polling)
    → sync_and_save()
```

---

## 10. Android-Specific Concerns

### 10.1 File Storage

The wallet CBOR file and swap reports are stored in the app's internal storage:

```kotlin
val dataDir = context.filesDir.resolve("coinswap/taker").absolutePath
// Results in: /data/data/org.coinswap.app/files/coinswap/taker/
//   wallets/taker-wallet          (CBOR, optionally encrypted)
//   swap_reports/                 (JSON per swap)
//   debug.log                    (log4rs output)
```

### 10.2 Memory & JNI Considerations

- The `Taker` object holds a `Mutex<CoinswapTaker>` on the Rust side. Kotlin holds an `Arc` pointer via UniFFI.
- On `Activity.onDestroy()`, the Kotlin `Taker` object is dropped, triggering Rust's `Drop` which calls `save_to_disk()`.
- Swap operations can hold the mutex for 30-120 seconds. All other wallet operations block on this. The UI must show appropriate loading states.

### 10.3 Permissions

```xml
<uses-permission android:name="android.permission.INTERNET" />
<uses-permission android:name="android.permission.ACCESS_NETWORK_STATE" />
<uses-permission android:name="android.permission.FOREGROUND_SERVICE" />
<uses-permission android:name="android.permission.FOREGROUND_SERVICE_DATA_SYNC" />
<uses-permission android:name="android.permission.POST_NOTIFICATIONS" />
```

### 10.4 ProGuard Rules

```
# Keep UniFFI generated classes
-keep class org.coinswap.** { *; }
-keep class uniffi.** { *; }

# Keep JNI methods
-keepclasseswithmembernames class * {
    native <methods>;
}
```

---

## 11. Phased Delivery

| Phase | Deliverable | Depends On |
|---|---|---|
| **P0** | `BlockchainBackend` trait + RPC implementation in coinswap core | — |
| **P1** | `ElectrumBackend` with address pool in coinswap core | P0 |
| **P2** | Updated FFI types (`BackendConfig`, `ElectrumConfig`) in ffi-commons | P1 |
| **P3** | Regenerate coinswap-kotlin bindings, integration tests | P2 |
| **P4** | Android app skeleton: Hilt, navigation, Tor setup, wallet init | P3 |
| **P5** | Core screens: Home, Send, Receive, Transactions | P4 |
| **P6** | Swap screen + foreground service + offerbook browser | P5 |
| **P7** | UTXO management, privacy settings, backup/restore | P6 |
| **P8** | Recovery flows, edge-case handling, beta testing | P7 |

---

## 12. Testing Strategy

### 12.1 Kotlin Integration Tests (JVM)

Extend the existing test pattern in `coinswap-kotlin/test/`:

```kotlin
class ElectrumTakerTest {
    @Test
    fun `init with electrum backend and sync`() {
        val config = ElectrumConfig(
            servers = listOf("ssl://electrum.blockstream.info:60002"),
            torProxy = "127.0.0.1:9050",
            decoyPoolSize = 10u,
            realBatchSize = 3u,
            network = "signet"
        )
        val taker = Taker.init(
            dataDir = tempDir.absolutePath,
            walletFileName = "test-wallet",
            backendConfig = BackendConfig.Electrum(config),
            controlPort = null,
            torAuthPassword = null,
            password = null
        )
        taker.syncAndSave()
        val balances = taker.getBalances()
        assert(balances.spendable >= 0)
    }
}
```

### 12.2 Android Instrumented Tests

Signet-based end-to-end tests running on emulator:
- Wallet creation → fund via signet faucet → sync → verify balance.
- Full swap flow with test maker on signet.
- Backup to file → delete wallet → restore → verify balance matches.
- Kill app during swap → relaunch → recovery completes.

### 12.3 Privacy Validation

- Capture Electrum server logs, verify real address ratio ≤ `real_batch_size / (real_batch_size + decoy_pool_size)`.
- Verify different batches go to different servers (Tor circuit fingerprinting in test mode).
- Verify no single server receives more than `real_batch_size` real addresses.
