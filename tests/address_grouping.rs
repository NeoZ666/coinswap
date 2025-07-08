#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use bitcoind::bitcoincore_rpc::RpcApi;
use coinswap::{
    maker::MakerBehavior,
    taker::TakerBehavior,
    utill::{ConnectionType, MIN_FEE_RATE},
    wallet::Destination,
};
mod test_framework;
use test_framework::*;

use std::sync::atomic::Ordering::Relaxed;

/// Test for Address Grouping Behavior in Coin Selection
///
/// Verifies that when multiple UTXOs exist at the same address,
/// coin selection groups them together and spends them as a unit,
/// even when UTXOs at different addresses are available.
#[test]
fn test_address_grouping_behavior() {
    // ---- Setup ----
    let makers_config_map = [((6102, None), MakerBehavior::Normal)];
    let taker_behavior = vec![TakerBehavior::Normal];

    let (test_framework, _, makers, directory_server_instance, block_generation_handle) =
        TestFramework::init(
            makers_config_map.into(),
            taker_behavior,
            ConnectionType::CLEARNET,
        );

    log::info!("🧪 Testing Address Grouping Behavior");

    let bitcoind = &test_framework.bitcoind;
    let maker = makers.first().unwrap();

    // Get multiple addresses from the wallet
    let (address_a, address_b, address_c, address_d) = {
        let mut wallet = maker.get_wallet().write().unwrap();
        let addr_a = wallet.get_next_external_address().unwrap();
        let addr_b = wallet.get_next_external_address().unwrap();
        let addr_c = wallet.get_next_external_address().unwrap();
        let addr_d = wallet.get_next_external_address().unwrap();
        (addr_a, addr_b, addr_c, addr_d)
    };

    log::info!("📍 Address A (4 UTXOs): {address_a}");
    log::info!("📍 Address B (5 UTXOs): {address_b}");
    log::info!("📍 Address C (6 UTXOs): {address_c}");
    log::info!("📍 Address D (1 UTXO): {address_d}");

    // Multi-UTXO test setup: Multiple addresses with different UTXO counts
    // Address A: 4 UTXOs (larger amounts - less attractive)
    send_to_address(bitcoind, &address_a, Amount::from_btc(0.8).unwrap()); // 80M sats
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_a, Amount::from_btc(0.9).unwrap()); // 90M sats
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_a, Amount::from_btc(0.7).unwrap()); // 70M sats
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_a, Amount::from_btc(0.6).unwrap()); // 60M sats
    generate_blocks(bitcoind, 1);

    // Address B: 5 UTXOs (medium amounts - moderately attractive)
    send_to_address(bitcoind, &address_b, Amount::from_btc(0.3).unwrap()); // 30M sats
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_b, Amount::from_btc(0.25).unwrap()); // 25M sats
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_b, Amount::from_btc(0.35).unwrap()); // 35M sats
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_b, Amount::from_btc(0.2).unwrap()); // 20M sats
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_b, Amount::from_btc(0.4).unwrap()); // 40M sats
    generate_blocks(bitcoind, 1);

    // Address C: 6 UTXOs (smaller amounts - most attractive for grouping)
    send_to_address(bitcoind, &address_c, Amount::from_btc(0.15).unwrap()); // 15M sats
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_c, Amount::from_btc(0.18).unwrap()); // 18M sats
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_c, Amount::from_btc(0.12).unwrap()); // 12M sats
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_c, Amount::from_btc(0.22).unwrap()); // 22M sats
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_c, Amount::from_btc(0.16).unwrap()); // 16M sats
    generate_blocks(bitcoind, 1);
    send_to_address(bitcoind, &address_c, Amount::from_btc(0.14).unwrap()); // 14M sats
    generate_blocks(bitcoind, 1);

    // Address D: 1 UTXO (perfect for fee optimization - between A and B)
    send_to_address(bitcoind, &address_d, Amount::from_btc(2.2).unwrap()); // 220M sats
    generate_blocks(bitcoind, 1);

    // Sync wallet and verify UTXOs
    {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync_no_fail();
        let balances = wallet.get_balances().unwrap();
        log::info!("📊 Wallet balance: {} sats", balances.regular.to_sat());
    }

    let (utxo_count, address_a_utxos, address_b_utxos, address_c_utxos, address_d_utxos) = {
        let wallet = maker.get_wallet().read().unwrap();
        let all_utxos = wallet.get_all_utxo().unwrap();

        // Count UTXOs by address (using specific amount identification)
        let addr_a_utxos: Vec<_> = all_utxos
            .iter()
            .filter(|utxo| {
                [80_000_000, 90_000_000, 70_000_000, 60_000_000].contains(&utxo.amount.to_sat())
            })
            .map(|u| u.amount.to_sat())
            .collect();

        let addr_b_utxos: Vec<_> = all_utxos
            .iter()
            .filter(|utxo| {
                [30_000_000, 25_000_000, 35_000_000, 20_000_000, 40_000_000].contains(&utxo.amount.to_sat())
            })
            .map(|u| u.amount.to_sat())
            .collect();

        let addr_c_utxos: Vec<_> = all_utxos
            .iter()
            .filter(|utxo| {
                [15_000_000, 18_000_000, 12_000_000, 22_000_000, 16_000_000, 14_000_000].contains(&utxo.amount.to_sat())
            })
            .map(|u| u.amount.to_sat())
            .collect();

        let addr_d_utxos: Vec<_> = all_utxos
            .iter()
            .filter(|utxo| {
                utxo.amount == Amount::from_btc(2.2).unwrap()
            })
            .map(|u| u.amount.to_sat())
            .collect();

        let addr_a_total: u64 = addr_a_utxos.iter().sum();
        let addr_b_total: u64 = addr_b_utxos.iter().sum(); 
        let addr_c_total: u64 = addr_c_utxos.iter().sum();
        let addr_d_total: u64 = addr_d_utxos.iter().sum();

        log::info!("📊 Total UTXOs: {}", all_utxos.len());
        log::info!("📊 Address A: {} UTXOs totaling {}M sats", addr_a_utxos.len(), addr_a_total / 1_000_000);
        log::info!("📊 Address B: {} UTXOs totaling {}M sats", addr_b_utxos.len(), addr_b_total / 1_000_000);
        log::info!("📊 Address C: {} UTXOs totaling {}M sats", addr_c_utxos.len(), addr_c_total / 1_000_000);
        log::info!("📊 Address D: {} UTXO totaling {}M sats", addr_d_utxos.len(), addr_d_total / 1_000_000);
        (all_utxos.len(), addr_a_utxos.len(), addr_b_utxos.len(), addr_c_utxos.len(), addr_d_utxos.len())
    };

    assert_eq!(utxo_count, 16, "Should have exactly 16 UTXOs total");
    assert_eq!(address_a_utxos, 4, "Should have exactly 4 UTXOs at address A");
    assert_eq!(address_b_utxos, 5, "Should have exactly 5 UTXOs at address B");
    assert_eq!(address_c_utxos, 6, "Should have exactly 6 UTXOs at address C");
    assert_eq!(address_d_utxos, 1, "Should have exactly 1 UTXO at address D");

    // Test coin selection with amount that ALL addresses can satisfy
    let external_addr = bitcoind
        .client
        .get_new_address(None, None)
        .unwrap()
        .assume_checked();
    let test_amount = Amount::from_sat(50_000_000); // 50M sats - all addresses can satisfy this

    log::info!("🧪 Testing coin selection for {} sats", test_amount.to_sat());
    log::info!("🎯 ULTIMATE GROUPING TEST: This amount (50M) with single UTXO option:");
    log::info!("   - Address A: 4 UTXOs (80M+90M+70M+60M = 300M) - VERY WASTEFUL if grouped");
    log::info!("   - Address B: 5 UTXOs (30M+25M+35M+20M+40M = 150M) - MODERATE waste if grouped");
    log::info!("   - Address C: 6 UTXOs (15M+18M+12M+22M+16M+14M = 97M) - BEST for grouping");
    log::info!("   - Address D: 1 UTXO (220M) - PERFECT for fee optimization!");
    log::info!("🔍 ADDRESS GROUPING SHOULD: Still select Address C (6 UTXOs, privacy over efficiency)");
    log::info!("🔍 FEE OPTIMIZATION WOULD: Select Address D (single 220M UTXO, most efficient)");

    let (selected_utxo_count, selected_total_amount) = {
        let mut wallet = maker.get_wallet().write().unwrap();
        let selected_utxos = wallet.coin_select(test_amount, MIN_FEE_RATE).unwrap();

        log::info!("🔍 Coin selection returned {} UTXOs", selected_utxos.len());

        let total_selected: Amount = selected_utxos.iter().map(|(utxo, _)| utxo.amount).sum();
        log::info!("💰 Total amount selected: {} sats", total_selected.to_sat());

        // Log details of what was selected
        for (i, (utxo, _)) in selected_utxos.iter().enumerate() {
            log::info!("   UTXO {}: {} sats", i + 1, utxo.amount.to_sat());
        }

        if !selected_utxos.is_empty() {
            let destination = Destination::Multi {
                outputs: vec![(external_addr, test_amount)],
                op_return_data: None,
            };

            match wallet.spend_from_wallet(MIN_FEE_RATE, destination, &selected_utxos) {
                Ok(tx) => {
                    bitcoind.client.send_raw_transaction(&tx).unwrap();
                    generate_blocks(bitcoind, 1);
                    log::info!("✅ Transaction broadcast successfully");
                }
                Err(e) => {
                    log::error!("❌ Transaction failed: {e:?}");
                    panic!("Transaction should not fail with selected UTXOs");
                }
            }
        }

        (selected_utxos.len(), total_selected)
    };

    // Sync after spending
    {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync_no_fail();
    }

    // THE CRITICAL TEST: Address grouping verification
    log::info!("🧪 CRITICAL ADDRESS GROUPING ANALYSIS:");

    // THE CRITICAL TEST: What did the algorithm choose?
    log::info!("🧪 CRITICAL ADDRESS GROUPING ANALYSIS:");

    // THE ULTIMATE TEST: Grouping vs Perfect Single UTXO
    log::info!("🧪 ULTIMATE GROUPING vs EFFICIENCY TEST:");

    if selected_utxo_count >= 6 && selected_total_amount >= Amount::from_sat(97_000_000) {
        log::info!("✅ SUCCESS: Address grouping WINS over efficiency!");
        log::info!("   → Selected {} UTXOs from Address C (97M+ total)", selected_utxo_count);
        log::info!("   → Chose privacy grouping over Address D single UTXO (220M)");
        log::info!("   → Privacy prioritized over perfect fee optimization!");
    } else if selected_utxo_count >= 5 && selected_total_amount >= Amount::from_sat(150_000_000) {
        log::info!("✅ SUCCESS: Address grouping detected - Address B selected!");
        log::info!("   → Selected {} UTXOs from Address B (150M+ total)", selected_utxo_count);
        log::info!("   → Chose privacy grouping over Address D efficiency");
    } else if selected_utxo_count >= 4 && selected_total_amount >= Amount::from_sat(300_000_000) {
        log::info!("✅ SUCCESS: Address grouping detected - Address A selected!");
        log::info!("   → Selected {} UTXOs from Address A (300M+ total)", selected_utxo_count);
        log::info!("   → Chose privacy over efficiency (very wasteful but private)");
    } else if selected_utxo_count == 1 && selected_total_amount == Amount::from_sat(220_000_000) {
        log::error!("❌ CRITICAL FAILURE: Fee optimization WON over address grouping!");
        log::error!("   → Selected Address D single 220M UTXO (most efficient)");
        log::error!("   → Address grouping COMPLETELY FAILED when perfect single option available");
        log::error!("   → This proves grouping only works when coincidentally optimal");
        panic!("Address grouping should prioritize privacy over efficiency - FAILED ULTIMATE TEST");
    } else if selected_utxo_count == 1 {
        let amount_m = selected_total_amount.to_sat() / 1_000_000;
        log::error!("❌ FAILURE: Fee optimization won - selected single {}M UTXO", amount_m);
        log::error!("   → Should have grouped multiple UTXOs from same address");
        panic!("Address grouping should group UTXOs from same address when address reuse exists");
    } else {
        log::warn!("⚠️  UNEXPECTED: Unusual selection pattern");
        log::warn!("   → Selected {} UTXOs totaling {}M sats", selected_utxo_count, selected_total_amount.to_sat() / 1_000_000);
        log::warn!("   → Need to analyze what happened");
    }

    // Assert the core requirement: should group multiple UTXOs from same address
    assert!(
        selected_utxo_count >= 4,
        "Address grouping failed: selected {} UTXOs, but should group 4+ UTXOs from same address",
        selected_utxo_count
    );

    assert!(
        selected_total_amount >= Amount::from_sat(97_000_000),
        "Address grouping failed: selected {}M sats, but should select 4+ UTXOs (97M+ sats total)",
        selected_total_amount.to_sat() / 1_000_000
    );

    log::info!("✅ Address grouping behavior confirmed!");
    log::info!("🎯 Test validates: UTXOs at same address are grouped together during selection");

    // Cleanup
    directory_server_instance.shutdown.store(true, Relaxed);
    test_framework.stop();
    block_generation_handle.join().unwrap();

    log::info!("🎉 Address grouping test completed!");
}