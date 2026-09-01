// Minimal Blend Protocol lending-pool interface, hand-written against the verified
// public contract traits rather than `contractimport!`'d from a vendored WASM binary —
// this lets BlendAdapter talk to any deployed Blend pool (V1 or V2) by address, without
// baking a specific Blend release into this crate's build.
//
// Verified 2026-09-01 against:
//   - V1: github.com/blend-capital/blend-contracts   (pool/src/contract.rs, pool/src/pool/actions.rs)
//   - V2: github.com/blend-capital/blend-contracts-v2 (pool/src/contract.rs, pool/src/pool/actions.rs)
//
// `submit` / `get_positions` have identical signatures on both V1 and V2 pool contracts.
// `get_reserve` / `get_reserve_list` exist ONLY on V2's public trait — V1 pools do not
// expose a reserve-rate getter. BlendAdapter only calls them when configured for a V2
// pool (`BlendConfig.pool_version == 2`); see `value_usdc_6` in lib.rs.

use soroban_sdk::{contractclient, contracttype, Address, Env, Map, Vec};

/// A single action to submit to the pool (see `RequestType`).
#[contracttype]
#[derive(Clone, Debug)]
pub struct Request {
    pub request_type: u32,
    /// The underlying asset address for Supply/Withdraw/SupplyCollateral/WithdrawCollateral/
    /// Borrow/Repay requests.
    pub address: Address,
    pub amount: i128,
}

/// Uncollateralized, interest-earning deposit — what BlendAdapter uses.
pub const REQUEST_TYPE_SUPPLY: u32 = 0;
/// Withdraw a `Supply` position. If `amount` exceeds the caller's available balance,
/// the pool caps the withdrawal to what is actually available rather than reverting.
pub const REQUEST_TYPE_WITHDRAW: u32 = 1;

/// A user/contract's positions in the pool, keyed by reserve index (not asset address).
#[contracttype]
#[derive(Clone, Debug)]
pub struct Positions {
    pub liabilities: Map<u32, i128>,
    pub collateral: Map<u32, i128>,
    pub supply: Map<u32, i128>,
}

/// A reserve's current interest-accrual state (V2 `get_reserve` return type).
#[contracttype]
#[derive(Clone, Debug)]
pub struct Reserve {
    pub asset: Address,
    pub index: u32,
    pub l_factor: u32,
    pub c_factor: u32,
    pub max_util: u32,
    pub last_time: u64,
    pub scalar: i128,
    /// dToken -> underlying conversion rate, 9 decimals.
    pub d_rate: i128,
    /// bToken -> underlying conversion rate, 9 decimals.
    pub b_rate: i128,
    pub ir_mod: i128,
    pub b_supply: i128,
    pub d_supply: i128,
    pub backstop_credit: i128,
}

#[allow(dead_code)]
#[contractclient(name = "PoolClient")]
pub trait PoolInterface {
    fn get_positions(e: Env, address: Address) -> Positions;

    /// `from` takes on the resulting position, `spender` sends any tokens the pool
    /// requires, and `to` receives any tokens the pool sends out.
    fn submit(e: Env, from: Address, spender: Address, to: Address, requests: Vec<Request>) -> Positions;

    /// V2 pools only.
    fn get_reserve(e: Env, asset: Address) -> Reserve;

    /// V2 pools only.
    fn get_reserve_list(e: Env) -> Vec<Address>;
}
