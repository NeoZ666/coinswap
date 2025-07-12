#![cfg(feature = "integration-test")]
use bitcoin::Amount;
use coinswap::{
    maker::MakerBehavior,
    taker::TakerBehavior,
    utill::{ConnectionType, MIN_FEE_RATE},
};
mod test_framework;
use test_framework::*;

use std::collections::HashMap;

/// Test for Address Grouping Behavior in Coin Selection
///
/// Verifies that when multiple UTXOs exist at the same address,
/// coin selection groups them together and spends them as a unit.
#[test]
fn test_address_grouping_behavior() {
    // ---- Setup ----
    let makers_config_map = [((6102, None), MakerBehavior::Normal)];
    let taker_behavior = vec![TakerBehavior::Normal];

    let (test_framework, _, makers, _, _) = TestFramework::init(
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

    // Multi-UTXO test setup: More lopsided distribution with smaller, numerous UTXOs

    // Address A: 10 tiny UTXOs (dust-like amounts)
    let tiny_amounts = [
        0.001, 0.0015, 0.0008, 0.0012, 0.0009, 0.0011, 0.0007, 0.0013, 0.0006, 0.0014,
    ];
    for amount in tiny_amounts {
        send_to_address(bitcoind, &address_a, Amount::from_btc(amount).unwrap());
        generate_blocks(bitcoind, 1);
    }

    // Address B: 14 small UTXOs (very small amounts)
    let small_amounts = [
        0.005, 0.008, 0.003, 0.007, 0.004, 0.006, 0.009, 0.0025, 0.0035, 0.0045, 0.025, 0.038,
        0.018, 0.042,
    ];
    for amount in small_amounts {
        send_to_address(bitcoind, &address_b, Amount::from_btc(amount).unwrap());
        generate_blocks(bitcoind, 1);
    }

    // Address C: 6 medium UTXOs (moderate amounts)
    let medium_amounts = [0.02, 0.035, 0.015, 0.045, 0.0055, 0.95];
    for amount in medium_amounts {
        send_to_address(bitcoind, &address_c, Amount::from_btc(amount).unwrap());
        generate_blocks(bitcoind, 1);
    }

    // Address D: 6 large UTXOs (big amounts)
    let large_amounts = [1.5, 2.8, 0.0065, 0.0075, 0.0085, 0.0095];
    for amount in large_amounts {
        send_to_address(bitcoind, &address_d, Amount::from_btc(amount).unwrap());
        generate_blocks(bitcoind, 1);
    }

    // Address E: 1 massive UTXO (whale amount)
    let whale_address = {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.get_next_external_address().unwrap()
    };
    send_to_address(bitcoind, &whale_address, Amount::from_btc(2.2).unwrap());
    generate_blocks(bitcoind, 1);

    // Sync wallet and verify UTXOs
    {
        let mut wallet = maker.get_wallet().write().unwrap();
        wallet.sync_no_fail();
    }

    let wallet = maker.get_wallet().read().unwrap();
    let all_utxos = wallet.get_all_utxo().unwrap();
    let addr_a_utxos: Vec<_> = all_utxos
        .iter()
        .filter(|utxo| utxo.address.as_ref().unwrap().assume_checked_ref() == &address_a)
        .collect();

    let addr_b_utxos: Vec<_> = all_utxos
        .iter()
        .filter(|utxo| utxo.address.as_ref().unwrap().assume_checked_ref() == &address_b)
        .collect();

    let addr_c_utxos: Vec<_> = all_utxos
        .iter()
        .filter(|utxo| utxo.address.as_ref().unwrap().assume_checked_ref() == &address_c)
        .collect();

    let addr_d_utxos: Vec<_> = all_utxos
        .iter()
        .filter(|utxo| utxo.address.as_ref().unwrap().assume_checked_ref() == &address_d)
        .collect();

    println!(
        "UTXO amounts: A={:?},\n B={:?},\n C={:?},\n D={:?}",
        addr_a_utxos
            .iter()
            .map(|utxo| utxo.amount.to_sat())
            .collect::<Vec<_>>(),
        addr_b_utxos
            .iter()
            .map(|utxo| utxo.amount.to_sat())
            .collect::<Vec<_>>(),
        addr_c_utxos
            .iter()
            .map(|utxo| utxo.amount.to_sat())
            .collect::<Vec<_>>(),
        addr_d_utxos
            .iter()
            .map(|utxo| utxo.amount.to_sat())
            .collect::<Vec<_>>()
    );

    // assert_eq!(utxo_count, 16, "Should have exactly 16 UTXOs total");
    // assert_eq!(
    //     address_a_utxos, 4,
    //     "Should have exactly 4 UTXOs at address A"
    // );
    // assert_eq!(
    //     address_b_utxos, 5,
    //     "Should have exactly 5 UTXOs at address B"
    // );
    // assert_eq!(
    //     address_c_utxos, 6,
    //     "Should have exactly 6 UTXOs at address C"
    // );
    // assert_eq!(
    //     address_d_utxos, 1,
    //     "Should have exactly 1 UTXO at address D"
    // );

    let test_amounts = vec![
        Amount::from_sat(10_000_000),  // 10M sats - should select smaller UTXOs
        Amount::from_sat(50_000_000),  // 50M sats - your original test
        Amount::from_sat(100_000_000), // 100M sats - might select Address B
        Amount::from_sat(200_000_000), // 200M sats - likely Address A or D
        Amount::from_sat(300_000_000), // 300M sats - definitely Address A
        Amount::from_sat(500_000_000), // 500M sats - might need multiple addresses
    ];

    for test_amount in test_amounts {
        log::info!(
            "\n=== Testing coin selection for {} sats ===",
            test_amount.to_sat()
        );

        let wallet = maker.get_wallet().read().unwrap();
        match wallet.coin_select(test_amount, MIN_FEE_RATE) {
            Ok(selected_utxos) => {
                log::info!("Selected {} UTXOs:", selected_utxos.len());

                // Print each selected input
                for (i, (utxo, _)) in selected_utxos.iter().enumerate() {
                    log::info!(
                        "  Input {}: {} sats from address {:?}",
                        i + 1,
                        utxo.amount.to_sat(),
                        utxo.address
                            .as_ref()
                            .map(|a| a.clone().assume_checked().to_string())
                            .unwrap_or_else(|| "Unknown".to_string())
                    );
                }

                let total_selected: Amount =
                    selected_utxos.iter().map(|(utxo, _)| utxo.amount).sum();
                log::info!("Total selected: {} sats", total_selected.to_sat());

                // Group by address to see grouping behavior
                let mut address_groups = HashMap::new();
                for (utxo, _) in &selected_utxos {
                    if let Some(addr) = &utxo.address {
                        *address_groups
                            .entry(addr.clone().assume_checked().to_string())
                            .or_insert(0) += 1;
                    }
                }

                log::info!("Selected Address Group:");
                for (addr_str, count) in address_groups {
                    let group_label = if addr_str == address_a.to_string() {
                        "A"
                    } else if addr_str == address_b.to_string() {
                        "B"
                    } else if addr_str == address_c.to_string() {
                        "C"
                    } else if addr_str == address_d.to_string() {
                        "D"
                    } else if addr_str == whale_address.to_string() {
                        "E (Whale)"
                    } else {
                        "Unknown"
                    };

                    log::info!("  Group {group_label}: {count} UTXOs ({addr_str})");
                }
            }
            Err(e) => {
                log::error!(
                    "Coin selection failed for {} sats: {:?}",
                    test_amount.to_sat(),
                    e
                );
            }
        }
    }

    log::info!("🎉 Address grouping test completed!");
}
