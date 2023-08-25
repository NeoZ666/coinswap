#![doc = include_str!("../README.md")]

extern crate bitcoin;
extern crate bitcoincore_rpc;

pub mod error;
pub mod maker;
pub mod market;
pub mod protocol;
pub mod scripts;
pub mod taker;
mod utill;
pub mod wallet;
// Diasable watchtower for now. Handle contract watching
// individually for maker and Taker.
//pub mod watchtower;
