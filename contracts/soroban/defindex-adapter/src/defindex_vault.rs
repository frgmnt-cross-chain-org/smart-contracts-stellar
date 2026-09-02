// Minimal deFindex vault interface, hand-written against the verified public contract
// source rather than `contractimport!`'d from a vendored WASM binary — see
// blend-adapter/src/blend_pool.rs for why this crate uses this pattern throughout.
//
// Verified 2026-09-02 against github.com/defindex-io/stellar-contracts
// (vault/src/interface.rs, vault/src/models.rs, common/src/models.rs).
//
// A deFindex vault is multi-asset (it takes/returns one amount per configured asset,
// even for a single-asset vault), and is itself an SEP-41 fungible token representing
// vault shares — so this adapter reads its own share balance via the standard
// `token::Client` rather than a bespoke `shares()` call. This adapter only ever talks to
// a *single-asset* USDC vault, so every `Vec<i128>` used here is length 1.
//
// Like xycLoans, deFindex's real functions return `Result<T, ContractError>` where
// `ContractError` derives `#[contracterror]`; see xycloans_pool.rs for why this
// interface declares the success type only (errors become call-level traps).

use soroban_sdk::{contractclient, contracttype, Address, Env, Vec};

/// A single strategy's allocation within one asset (vault/src/models.rs).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct StrategyAllocation {
    pub strategy_address: Address,
    pub amount: i128,
    pub paused: bool,
}

/// One asset's current total/idle/invested breakdown (vault/src/models.rs).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CurrentAssetInvestmentAllocation {
    pub asset: Address,
    pub total_amount: i128,
    pub idle_amount: i128,
    pub invested_amount: i128,
    pub strategy_allocations: Vec<StrategyAllocation>,
}

/// A deposit's post-investment allocation plan (vault/src/models.rs) — only appears in
/// `deposit`'s return value when `invest = true`; this adapter always passes `invest =
/// false` (investment timing is the vault's own manager/rebalancer's call, not a
/// depositor's), so in practice this type is only ever decoded as `None`.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct AssetInvestmentAllocation {
    pub asset: Address,
    pub strategy_allocations: Vec<Option<StrategyAllocation>>,
}

#[allow(dead_code)]
#[contractclient(name = "VaultClient")]
pub trait VaultInterface {
    /// Deposit `amounts_desired[i]` (subject to `amounts_min[i]`) of each configured
    /// asset from `from`, minting vault shares. `invest = false` leaves the deposit as
    /// idle funds in the vault rather than immediately deploying it.
    fn deposit(
        e: Env,
        amounts_desired: Vec<i128>,
        amounts_min: Vec<i128>,
        from: Address,
        invest: bool,
    ) -> (Vec<i128>, i128, Option<Vec<Option<AssetInvestmentAllocation>>>);

    /// Burn `df_amount` vault shares from `from`, returning at least `min_amounts_out[i]`
    /// of each asset (one withdrawn amount per configured asset).
    fn withdraw(e: Env, df_amount: i128, min_amounts_out: Vec<i128>, from: Address) -> Vec<i128>;

    /// Current total/idle/invested breakdown for every configured asset.
    fn fetch_total_managed_funds(e: Env) -> Vec<CurrentAssetInvestmentAllocation>;

    /// For `vault_shares` worth of this vault's shares, the corresponding amount of
    /// each configured asset at the current share price.
    fn get_asset_amounts_per_shares(e: Env, vault_shares: i128) -> Vec<i128>;
}
