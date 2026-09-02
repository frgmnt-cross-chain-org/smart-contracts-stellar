// Minimal xycLoans pool interface, hand-written against the verified public contract
// source rather than `contractimport!`'d from a vendored WASM binary — see
// blend-adapter/src/blend_pool.rs for why this crate uses this pattern throughout.
//
// Verified 2026-09-02 against github.com/xycloo/xycloans (pool/src/contract.rs,
// pool/src/token_utility.rs, pool/src/rewards.rs).
//
// xycLoans is a flash-loan-only liquidity pool: lenders deposit into a share pool where
// 1 share == 1 deposited underlying-token unit (no exchange-rate bookkeeping at all,
// unlike Blend's bToken/b_rate model). Yield is NOT reflected in share value — it accrues
// as a *separate* claim ("matured fees") that must be harvested via
// `update_fee_rewards` + `withdraw_matured`, independent of principal (`shares` /
// `withdraw`). An adapter's total position value is therefore `shares + matured`, and a
// full exit must claim both.
//
// All of the pool's real functions return `Result<T, Error>` where `Error` derives
// `#[contracterror]` — Soroban's host converts an `Err` return into a call-level trap
// automatically, so the *success* type is what a calling contract's typed client sees;
// declaring `Result<..>` return types here would be redundant (and would require mirroring
// the exact `Error` enum for no benefit, since this adapter never inspects which error
// occurred — either the call succeeds or the whole transaction reverts).

use soroban_sdk::{contractclient, Address, Env};

#[allow(dead_code)]
#[contractclient(name = "PoolClient")]
pub trait PoolInterface {
    /// Deposit `amount` of the pool's underlying token from `from`, minting `amount`
    /// shares 1:1.
    fn deposit(e: Env, from: Address, amount: i128);

    /// Recompute `addr`'s accrued-but-unclaimed fee rewards into its "matured" balance.
    /// Callable by anyone; must be called before `withdraw_matured` to include the
    /// latest accrual.
    fn update_fee_rewards(e: Env, addr: Address);

    /// Pay out `addr`'s full matured-fee balance and reset it to zero. Reverts if the
    /// matured balance is zero — callers must check `matured(addr) > 0` first.
    fn withdraw_matured(e: Env, addr: Address);

    /// Burn `amount` of `addr`'s principal shares and pay out `amount` of the
    /// underlying token (1:1, principal only — does not include matured fees).
    fn withdraw(e: Env, addr: Address, amount: i128);

    /// `addr`'s principal share balance (1:1 with deposited underlying).
    fn shares(e: Env, addr: Address) -> i128;

    /// `addr`'s currently accrued, not-yet-withdrawn matured fee balance.
    fn matured(e: Env, addr: Address) -> i128;
}
