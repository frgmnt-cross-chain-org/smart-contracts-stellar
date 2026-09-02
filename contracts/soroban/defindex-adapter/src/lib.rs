// DefindexAdapter — deploys idle Stellar USDC into a single-asset deFindex vault
// (github.com/defindex-io/stellar-contracts) and reports a live valuation back to
// AllocationManager / VaultAccounting.
//
// deFindex is a multi-strategy vault aggregator, not a lending pool itself — it routes
// deposits into whichever strategy contracts a given vault is configured with (one of
// which may be a Blend strategy, which this protocol does not want; others include
// xycLoans, Soroswap LP, and non-market strategies). This adapter only ever talks to the
// vault's own share-token interface, so it is agnostic to which strategy(ies) the target
// vault actually uses internally — governance is responsible for choosing (via
// `DefindexConfig.vault`) a vault that does not route to Blend.
//
// Only the configured Allocator (AllocationManager) may deposit/withdraw; only the
// adapter admin (governance) may change risk configuration or force an emergency exit.
//
// Decimal boundary rule (see docs/CROSS_CHAIN_FUSD_TECHNICAL_SPEC.md §7.6): every public
// entry point here speaks 6-decimal USDC. Conversion to/from the vault's native asset
// decimals (7 for Stellar SAC USDC) happens entirely inside this adapter.

#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, token, vec, Address, BytesN, Env};

mod defindex_vault;
use defindex_vault::VaultClient;

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct DefindexConfig {
    /// A single-asset deFindex vault. Governance must confirm out-of-band which
    /// strategy(ies) this vault routes to before registering it — this adapter has no
    /// way to inspect that itself, since deFindex's strategy set is a vault-level
    /// configuration choice, not something exposed per-deposit.
    pub vault: Address,
    /// The vault's sole configured asset (e.g. the Stellar USDC SAC).
    pub usdc_token: Address,
    /// Decimals of `usdc_token` (7 for Stellar SAC USDC) — also the vault share token's
    /// own decimals, since deFindex vault shares use the same decimals as the asset.
    pub asset_decimals: u32,
    /// Hard cap on principal this adapter may ever have deployed at once (6 decimals).
    pub max_protocol_exposure_6: i128,
    /// When true, `deposit` is blocked. Withdrawals always remain available.
    pub paused: bool,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Allocator,
    StrategyId,
    Config,
    DeployedPrincipal6,
}

#[contract]
pub struct DefindexAdapter;

#[contractimpl]
impl DefindexAdapter {
    // ── Initialisation ────────────────────────────────────────────────────────

    pub fn initialize(
        e: Env,
        admin: Address,
        allocator: Address,
        strategy_id: BytesN<32>,
        config: DefindexConfig,
    ) {
        assert!(!e.storage().instance().has(&DataKey::Admin), "already initialized");
        admin.require_auth();
        assert!(config.asset_decimals >= 6, "asset_decimals must be >= 6");
        assert!(config.max_protocol_exposure_6 >= 0, "max exposure must be non-negative");

        e.storage().instance().set(&DataKey::Admin, &admin);
        e.storage().instance().set(&DataKey::Allocator, &allocator);
        e.storage().instance().set(&DataKey::StrategyId, &strategy_id);
        e.storage().instance().set(&DataKey::Config, &config);
        e.storage().instance().set(&DataKey::DeployedPrincipal6, &0_i128);
    }

    // ── StrategyAdapter view interface (spec §7.6) ────────────────────────────

    pub fn strategy_id(e: Env) -> BytesN<32> {
        e.storage().instance().get(&DataKey::StrategyId).unwrap()
    }

    pub fn asset(e: Env) -> Address {
        Self::load_config(&e).usdc_token
    }

    /// Always 6 — the 7-decimal Stellar SAC boundary is crossed entirely inside this adapter.
    pub fn underlying_decimals(_e: Env) -> u32 {
        6
    }

    pub fn balance_underlying_6(e: Env) -> i128 {
        Self::value_usdc_6(e)
    }

    /// Live value of the adapter's vault-share position, priced via the vault's own
    /// `get_asset_amounts_per_shares` — this reflects whatever the vault's underlying
    /// strategy(ies) currently report, including accrued yield or loss.
    pub fn value_usdc_6(e: Env) -> i128 {
        let config = Self::load_config(&e);
        let this = e.current_contract_address();
        let shares = Self::my_shares(&e, &config, &this);
        if shares == 0 {
            return 0;
        }
        let underlying_native = Self::shares_to_underlying_native(&e, &config, shares);
        Self::native_to_usdc6(underlying_native, config.asset_decimals)
    }

    pub fn deployed_principal_6(e: Env) -> i128 {
        e.storage().instance().get(&DataKey::DeployedPrincipal6).unwrap_or(0)
    }

    // ── Allocator-gated actions ───────────────────────────────────────────────

    /// Supply `amount_6` USDC into the vault. `min_shares` is enforced against the
    /// adapter's own vault-share balance delta (native decimals) — measured, not
    /// assumed, matching the balance-delta discipline used throughout this protocol.
    /// Always deposits with `invest = false`: whether/when to deploy idle vault funds
    /// into the vault's underlying strategies is the vault's own manager's decision,
    /// not this adapter's.
    pub fn deposit(e: Env, caller: Address, amount_6: i128, min_shares: i128) {
        Self::auth_allocator(&e, &caller);
        assert!(amount_6 > 0, "amount must be positive");

        let config = Self::load_config(&e);
        assert!(!config.paused, "adapter paused");

        let principal_before: i128 = e.storage().instance().get(&DataKey::DeployedPrincipal6).unwrap_or(0);
        let new_principal = principal_before.checked_add(amount_6).expect("overflow");
        assert!(new_principal <= config.max_protocol_exposure_6, "max protocol exposure exceeded");

        let vault = VaultClient::new(&e, &config.vault);
        let this = e.current_contract_address();

        let shares_before = Self::my_shares(&e, &config, &this);

        let amount_native = Self::usdc6_to_native(amount_6, config.asset_decimals);
        let amounts_desired = vec![&e, amount_native];
        // We verify the result ourselves via the post-call share-balance delta rather
        // than trusting the vault's own `amounts_min` slippage gate.
        let amounts_min = vec![&e, 0_i128];
        vault.deposit(&amounts_desired, &amounts_min, &this, &false);

        let shares_after = Self::my_shares(&e, &config, &this);
        let minted = shares_after.saturating_sub(shares_before);
        assert!(minted >= min_shares, "slippage: min_shares not met");

        e.storage().instance().set(&DataKey::DeployedPrincipal6, &new_principal);
        e.events().publish((symbol_short!("Deposit"),), amount_6);
    }

    pub fn withdraw(e: Env, caller: Address, amount_6: i128, min_out_6: i128) -> i128 {
        Self::auth_allocator(&e, &caller);
        assert!(amount_6 > 0, "amount must be positive");
        let out_6 = Self::withdraw_internal(&e, Some(amount_6), min_out_6);
        e.events().publish((symbol_short!("Withdraw"),), out_6);
        out_6
    }

    pub fn withdraw_all(e: Env, caller: Address, min_out_6: i128) -> i128 {
        Self::auth_allocator(&e, &caller);
        let out_6 = Self::withdraw_internal(&e, None, min_out_6);
        e.events().publish((symbol_short!("WdrawAll"),), out_6);
        out_6
    }

    /// Governance-only emergency unwind. Ignores `paused` (paused only blocks new
    /// deposits) and bypasses the exposure cap.
    pub fn emergency_exit(e: Env, caller: Address, min_out_6: i128) -> i128 {
        Self::auth_admin(&e, &caller);
        let out_6 = Self::withdraw_internal(&e, None, min_out_6);
        e.events().publish((symbol_short!("EmerExit"),), out_6);
        out_6
    }

    /// Move `amount_6` of this adapter's own (already-withdrawn) USDC balance to an
    /// arbitrary destination. Allocator-gated — used by AllocationManager to route funds
    /// a prior withdrawal pulled out of the vault back to the hub's idle balance.
    pub fn sweep(e: Env, caller: Address, to: Address, amount_6: i128) {
        Self::auth_allocator(&e, &caller);
        assert!(amount_6 > 0, "amount must be positive");
        let config = Self::load_config(&e);
        let amount_native = Self::usdc6_to_native(amount_6, config.asset_decimals);
        let token = token::Client::new(&e, &config.usdc_token);
        token.transfer(&e.current_contract_address(), &to, &amount_native);
        e.events().publish((symbol_short!("Sweep"),), amount_6);
    }

    // ── Admin configuration ───────────────────────────────────────────────────

    pub fn set_paused(e: Env, caller: Address, paused: bool) {
        Self::auth_admin(&e, &caller);
        let mut config = Self::load_config(&e);
        config.paused = paused;
        e.storage().instance().set(&DataKey::Config, &config);
        e.events().publish((symbol_short!("Paused"),), paused);
    }

    pub fn set_max_exposure(e: Env, caller: Address, max_protocol_exposure_6: i128) {
        Self::auth_admin(&e, &caller);
        assert!(max_protocol_exposure_6 >= 0, "max exposure must be non-negative");
        let mut config = Self::load_config(&e);
        config.max_protocol_exposure_6 = max_protocol_exposure_6;
        e.storage().instance().set(&DataKey::Config, &config);
    }

    pub fn set_allocator(e: Env, caller: Address, allocator: Address) {
        Self::auth_admin(&e, &caller);
        e.storage().instance().set(&DataKey::Allocator, &allocator);
    }

    pub fn config(e: Env) -> DefindexConfig {
        Self::load_config(&e)
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn withdraw_internal(e: &Env, requested_6: Option<i128>, min_out_6: i128) -> i128 {
        let config = Self::load_config(e);
        let vault = VaultClient::new(e, &config.vault);
        let this = e.current_contract_address();
        let token = token::Client::new(e, &config.usdc_token);

        let total_shares = Self::my_shares(e, &config, &this);
        assert!(total_shares > 0, "no position");

        let shares_to_burn = match requested_6 {
            Some(amount_6) => {
                assert!(amount_6 > 0, "amount must be positive");
                let amount_native = Self::usdc6_to_native(amount_6, config.asset_decimals);
                let total_value_native = Self::shares_to_underlying_native(e, &config, total_shares);
                assert!(total_value_native > 0, "vault reports zero value");
                // Floor division: never round in the adapter's favor at the depositor's
                // expense (fewer shares burned than the honest proportional amount would
                // be a rounding error working against whoever is withdrawing next).
                let shares = amount_native
                    .checked_mul(total_shares)
                    .expect("overflow")
                    / total_value_native;
                assert!(shares > 0, "amount too small to redeem any shares");
                assert!(shares <= total_shares, "amount exceeds position value");
                shares
            }
            None => total_shares,
        };

        let balance_before = token.balance(&this);
        let min_amounts_out = vec![e, 0_i128];
        vault.withdraw(&shares_to_burn, &min_amounts_out, &this);
        let balance_after = token.balance(&this);

        let received_native = balance_after.checked_sub(balance_before).expect("overflow");
        assert!(received_native >= 0, "withdraw produced a negative balance delta");
        let out_6 = Self::native_to_usdc6(received_native, config.asset_decimals);
        assert!(out_6 >= min_out_6, "slippage: min_out_6 not met");

        let principal: i128 = e.storage().instance().get(&DataKey::DeployedPrincipal6).unwrap_or(0);
        e.storage().instance().set(&DataKey::DeployedPrincipal6, &(principal - out_6).max(0));

        out_6
    }

    /// This adapter's deFindex vault-share balance. The vault contract is itself an
    /// SEP-41 token representing shares, so this is a plain token balance query.
    fn my_shares(e: &Env, config: &DefindexConfig, holder: &Address) -> i128 {
        token::Client::new(e, &config.vault).balance(holder)
    }

    /// Converts `shares` of this (single-asset) vault into the underlying asset amount,
    /// in the vault's native decimals.
    fn shares_to_underlying_native(e: &Env, config: &DefindexConfig, shares: i128) -> i128 {
        let vault = VaultClient::new(e, &config.vault);
        let amounts = vault.get_asset_amounts_per_shares(&shares);
        amounts.get(0).expect("vault returned no asset amounts")
    }

    fn usdc6_to_native(amount_6: i128, asset_decimals: u32) -> i128 {
        let scale = 10i128.pow(asset_decimals - 6);
        amount_6.checked_mul(scale).expect("overflow")
    }

    fn native_to_usdc6(amount_native: i128, asset_decimals: u32) -> i128 {
        let scale = 10i128.pow(asset_decimals - 6);
        amount_native / scale
    }

    fn load_config(e: &Env) -> DefindexConfig {
        e.storage().instance().get(&DataKey::Config).unwrap()
    }

    fn auth_admin(e: &Env, caller: &Address) {
        let admin: Address = e.storage().instance().get(&DataKey::Admin).unwrap();
        assert!(*caller == admin, "not admin");
        caller.require_auth();
    }

    fn auth_allocator(e: &Env, caller: &Address) {
        let allocator: Address = e.storage().instance().get(&DataKey::Allocator).unwrap();
        assert!(*caller == allocator, "not allocator");
        caller.require_auth();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod test {
    use super::*;
    use crate::defindex_vault::{CurrentAssetInvestmentAllocation, VaultInterface};
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::token::StellarAssetClient;
    use soroban_sdk::Vec;

    // ── Mock deFindex vault ───────────────────────────────────────────────────
    //
    // Single-asset, share-price-based vault (shares appreciate as `simulate_yield`
    // injects real tokens without minting new shares, exactly like a real vault's
    // underlying strategy generating yield). Also exposes a plain `balance(id)`
    // function so `token::Client::new(vault_addr).balance(holder)` — how the real
    // adapter reads its own share balance — works transparently against the mock too.

    #[contracttype]
    enum MockKey {
        Token,
        TotalShares,
        TotalUnderlying,
        Shares(Address),
    }

    #[contract]
    struct MockDefindexVault;

    #[contractimpl]
    impl MockDefindexVault {
        pub fn init(e: Env, token: Address) {
            e.storage().instance().set(&MockKey::Token, &token);
            e.storage().instance().set(&MockKey::TotalShares, &0_i128);
            e.storage().instance().set(&MockKey::TotalUnderlying, &0_i128);
        }

        /// Test-only: simulate the vault's underlying strategy earning `amount` of real
        /// yield — tokens actually arrive in the vault, share count is unchanged, so
        /// existing holders' shares become worth more.
        pub fn simulate_yield(e: Env, from: Address, amount: i128) {
            let token_addr: Address = e.storage().instance().get(&MockKey::Token).unwrap();
            token::Client::new(&e, &token_addr).transfer(&from, &e.current_contract_address(), &amount);
            let total: i128 = e.storage().instance().get(&MockKey::TotalUnderlying).unwrap_or(0);
            e.storage().instance().set(&MockKey::TotalUnderlying, &(total + amount));
        }

        /// Standard SEP-41 `balance` — makes this contract readable via `token::Client`,
        /// exactly like the real deFindex vault (which is itself an SEP-41 token).
        pub fn balance(e: Env, id: Address) -> i128 {
            e.storage().instance().get(&MockKey::Shares(id)).unwrap_or(0)
        }
    }

    #[contractimpl]
    impl VaultInterface for MockDefindexVault {
        fn deposit(
            e: Env,
            amounts_desired: Vec<i128>,
            _amounts_min: Vec<i128>,
            from: Address,
            _invest: bool,
        ) -> (Vec<i128>, i128, Option<Vec<Option<defindex_vault::AssetInvestmentAllocation>>>) {
            let amount = amounts_desired.get(0).unwrap();
            let token_addr: Address = e.storage().instance().get(&MockKey::Token).unwrap();
            token::Client::new(&e, &token_addr).transfer(&from, &e.current_contract_address(), &amount);

            let total_shares: i128 = e.storage().instance().get(&MockKey::TotalShares).unwrap_or(0);
            let total_underlying: i128 = e.storage().instance().get(&MockKey::TotalUnderlying).unwrap_or(0);
            let minted = if total_shares == 0 || total_underlying == 0 {
                amount
            } else {
                amount.checked_mul(total_shares).unwrap() / total_underlying
            };

            let holder_shares: i128 = e.storage().instance().get(&MockKey::Shares(from.clone())).unwrap_or(0);
            e.storage().instance().set(&MockKey::Shares(from), &(holder_shares + minted));
            e.storage().instance().set(&MockKey::TotalShares, &(total_shares + minted));
            e.storage().instance().set(&MockKey::TotalUnderlying, &(total_underlying + amount));

            (soroban_sdk::vec![&e, amount], minted, None)
        }

        fn withdraw(e: Env, df_amount: i128, _min_amounts_out: Vec<i128>, from: Address) -> Vec<i128> {
            let holder_shares: i128 = e.storage().instance().get(&MockKey::Shares(from.clone())).unwrap_or(0);
            assert!(holder_shares >= df_amount, "AmountOverTotalSupply");

            let total_shares: i128 = e.storage().instance().get(&MockKey::TotalShares).unwrap_or(0);
            let total_underlying: i128 = e.storage().instance().get(&MockKey::TotalUnderlying).unwrap_or(0);
            let out = df_amount.checked_mul(total_underlying).unwrap() / total_shares;

            let token_addr: Address = e.storage().instance().get(&MockKey::Token).unwrap();
            token::Client::new(&e, &token_addr).transfer(&e.current_contract_address(), &from, &out);

            e.storage().instance().set(&MockKey::Shares(from), &(holder_shares - df_amount));
            e.storage().instance().set(&MockKey::TotalShares, &(total_shares - df_amount));
            e.storage().instance().set(&MockKey::TotalUnderlying, &(total_underlying - out));

            soroban_sdk::vec![&e, out]
        }

        fn fetch_total_managed_funds(e: Env) -> Vec<CurrentAssetInvestmentAllocation> {
            let token_addr: Address = e.storage().instance().get(&MockKey::Token).unwrap();
            let total_underlying: i128 = e.storage().instance().get(&MockKey::TotalUnderlying).unwrap_or(0);
            soroban_sdk::vec![
                &e,
                CurrentAssetInvestmentAllocation {
                    asset: token_addr,
                    total_amount: total_underlying,
                    idle_amount: total_underlying,
                    invested_amount: 0,
                    strategy_allocations: Vec::new(&e),
                }
            ]
        }

        fn get_asset_amounts_per_shares(e: Env, vault_shares: i128) -> Vec<i128> {
            let total_shares: i128 = e.storage().instance().get(&MockKey::TotalShares).unwrap_or(0);
            if total_shares == 0 {
                return soroban_sdk::vec![&e, 0];
            }
            let total_underlying: i128 = e.storage().instance().get(&MockKey::TotalUnderlying).unwrap_or(0);
            soroban_sdk::vec![&e, vault_shares.checked_mul(total_underlying).unwrap() / total_shares]
        }
    }

    // ── Test harness ───────────────────────────────────────────────────────────

    struct Harness {
        e: Env,
        admin: Address,
        allocator: Address,
        adapter: DefindexAdapterClient<'static>,
        token: token::Client<'static>,
        vault: Address,
    }

    fn setup() -> Harness {
        let e = Env::default();
        // See blend-adapter/xycloans-adapter for why this non-root-auth variant is
        // required: `deposit`/`withdraw` route `require_auth` through a nested call
        // (adapter -> vault -> token.transfer), where the adapter is not the *direct*
        // caller of the token.
        e.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&e);
        let allocator = Address::generate(&e);

        let sac = e.register_stellar_asset_contract_v2(admin.clone());
        let token_addr = sac.address();
        let token = token::Client::new(&e, &token_addr);
        let token_admin = StellarAssetClient::new(&e, &token_addr);

        let vault_id = e.register_contract(None, MockDefindexVault);
        let vault_setup = MockDefindexVaultClient::new(&e, &vault_id);
        vault_setup.init(&token_addr);

        let adapter_id = e.register_contract(None, DefindexAdapter);
        let adapter = DefindexAdapterClient::new(&e, &adapter_id);

        let strategy_id = BytesN::from_array(&e, &[13u8; 32]);
        let config = DefindexConfig {
            vault: vault_id.clone(),
            usdc_token: token_addr.clone(),
            asset_decimals: 7,
            max_protocol_exposure_6: 100_000_000,
            paused: false,
        };
        adapter.initialize(&admin, &allocator, &strategy_id, &config);

        token_admin.mint(&adapter_id, &1_000_000_000); // 100 USDC at 7 decimals

        Harness { e, admin, allocator, adapter, token, vault: vault_id }
    }

    #[test]
    fn deposit_moves_tokens_and_reports_value() {
        let h = setup();
        h.adapter.deposit(&h.allocator, &10_000_000, &1);

        assert_eq!(h.token.balance(&h.adapter.address), 900_000_000, "7-dec balance left the adapter");
        assert_eq!(h.token.balance(&h.vault), 100_000_000, "7-dec balance arrived at the vault");
        assert_eq!(h.adapter.deployed_principal_6(), 10_000_000);
        assert_eq!(h.adapter.value_usdc_6(), 10_000_000, "1:1 share price at first deposit");
    }

    #[test]
    fn value_usdc_6_reflects_vault_yield() {
        let h = setup();
        h.adapter.deposit(&h.allocator, &10_000_000, &1);

        // Vault's underlying strategy earns 10%: 100M -> 110M native, no new shares.
        let yield_payer = Address::generate(&h.e);
        StellarAssetClient::new(&h.e, &h.token.address).mint(&yield_payer, &10_000_000);
        MockDefindexVaultClient::new(&h.e, &h.vault).simulate_yield(&yield_payer, &10_000_000);

        assert_eq!(h.adapter.value_usdc_6(), 11_000_000, "share price appreciated 10%");
        assert_eq!(h.adapter.deployed_principal_6(), 10_000_000, "principal tracking untouched until withdrawn");
    }

    #[test]
    fn withdraw_measures_real_balance_delta_including_yield() {
        let h = setup();
        h.adapter.deposit(&h.allocator, &10_000_000, &1);

        let yield_payer = Address::generate(&h.e);
        StellarAssetClient::new(&h.e, &h.token.address).mint(&yield_payer, &10_000_000);
        MockDefindexVaultClient::new(&h.e, &h.vault).simulate_yield(&yield_payer, &10_000_000);

        // Ask for 5.5 USDC of value (chosen so the 1.1x share-price division lands on a
        // whole number of shares with no rounding dust, keeping the assertion exact —
        // proportional redemption otherwise floors to the pool's benefit, which
        // `min_out_6` protects against without requiring an exact match).
        let out = h.adapter.withdraw(&h.allocator, &5_500_000, &1);
        assert_eq!(out, 5_500_000);
        assert_eq!(h.adapter.value_usdc_6(), 5_500_000, "11M - 5.5M remaining");
    }

    #[test]
    fn proportional_withdraw_rounding_never_favors_the_withdrawer() {
        let h = setup();
        h.adapter.deposit(&h.allocator, &10_000_000, &1);

        let yield_payer = Address::generate(&h.e);
        StellarAssetClient::new(&h.e, &h.token.address).mint(&yield_payer, &10_000_000);
        MockDefindexVaultClient::new(&h.e, &h.vault).simulate_yield(&yield_payer, &10_000_000);

        // 5_000_000 does not divide evenly at a 1.1x share price — the adapter must
        // floor the shares burned (protecting the vault's other depositors) rather than
        // round up, so the withdrawer receives at most what they asked for, never more.
        let out = h.adapter.withdraw(&h.allocator, &5_000_000, &1);
        assert!(out <= 5_000_000, "rounding must never hand out more than requested");
        assert_eq!(out, 4_999_999, "documents the exact floor-rounding outcome");
    }

    #[test]
    fn withdraw_all_returns_full_position_including_yield() {
        let h = setup();
        h.adapter.deposit(&h.allocator, &10_000_000, &1);

        let yield_payer = Address::generate(&h.e);
        StellarAssetClient::new(&h.e, &h.token.address).mint(&yield_payer, &10_000_000);
        MockDefindexVaultClient::new(&h.e, &h.vault).simulate_yield(&yield_payer, &10_000_000);

        let out = h.adapter.withdraw_all(&h.allocator, &1);
        assert_eq!(out, 11_000_000);
        assert_eq!(h.adapter.value_usdc_6(), 0);
        assert_eq!(h.adapter.deployed_principal_6(), 0);
    }

    #[test]
    #[should_panic(expected = "slippage: min_shares not met")]
    fn deposit_enforces_min_shares() {
        let h = setup();
        h.adapter.deposit(&h.allocator, &10_000_000, &999_999_999_999);
    }

    #[test]
    #[should_panic(expected = "slippage: min_out_6 not met")]
    fn withdraw_enforces_min_out() {
        let h = setup();
        h.adapter.deposit(&h.allocator, &10_000_000, &1);
        h.adapter.withdraw(&h.allocator, &5_000_000, &5_000_001);
    }

    #[test]
    #[should_panic(expected = "not allocator")]
    fn only_allocator_can_deposit() {
        let h = setup();
        let attacker = Address::generate(&h.e);
        h.adapter.deposit(&attacker, &1_000_000, &0);
    }

    #[test]
    #[should_panic(expected = "adapter paused")]
    fn paused_blocks_deposit() {
        let h = setup();
        h.adapter.set_paused(&h.admin, &true);
        h.adapter.deposit(&h.allocator, &1_000_000, &0);
    }

    #[test]
    fn paused_does_not_block_withdraw() {
        let h = setup();
        h.adapter.deposit(&h.allocator, &10_000_000, &1);
        h.adapter.set_paused(&h.admin, &true);
        let out = h.adapter.withdraw(&h.allocator, &5_000_000, &1);
        assert_eq!(out, 5_000_000);
    }

    #[test]
    #[should_panic(expected = "max protocol exposure exceeded")]
    fn deposit_respects_max_exposure() {
        let h = setup();
        h.adapter.set_max_exposure(&h.admin, &5_000_000);
        h.adapter.deposit(&h.allocator, &5_000_001, &0);
    }

    #[test]
    fn emergency_exit_works_even_when_paused_and_bypasses_exposure_cap() {
        let h = setup();
        h.adapter.deposit(&h.allocator, &10_000_000, &1);
        h.adapter.set_paused(&h.admin, &true);
        h.adapter.set_max_exposure(&h.admin, &0);

        let out = h.adapter.emergency_exit(&h.admin, &1);
        assert_eq!(out, 10_000_000);
    }

    #[test]
    #[should_panic(expected = "not admin")]
    fn only_admin_can_emergency_exit() {
        let h = setup();
        h.adapter.deposit(&h.allocator, &10_000_000, &1);
        h.adapter.emergency_exit(&h.allocator, &1);
    }

    #[test]
    #[should_panic(expected = "no position")]
    fn withdraw_fails_with_no_position() {
        let h = setup();
        h.adapter.withdraw(&h.allocator, &1_000_000, &0);
    }

    #[test]
    fn sweep_moves_withdrawn_funds_onward() {
        let h = setup();
        h.adapter.deposit(&h.allocator, &10_000_000, &1);
        h.adapter.withdraw(&h.allocator, &10_000_000, &1);

        let treasury = Address::generate(&h.e);
        h.adapter.sweep(&h.allocator, &treasury, &10_000_000);
        assert_eq!(h.token.balance(&treasury), 100_000_000);
    }

    #[test]
    #[should_panic(expected = "not allocator")]
    fn only_allocator_can_sweep() {
        let h = setup();
        h.adapter.deposit(&h.allocator, &10_000_000, &1);
        h.adapter.withdraw(&h.allocator, &10_000_000, &1);
        let attacker = Address::generate(&h.e);
        let treasury = Address::generate(&h.e);
        h.adapter.sweep(&attacker, &treasury, &10_000_000);
    }

    #[test]
    fn underlying_decimals_is_always_six() {
        let h = setup();
        assert_eq!(h.adapter.underlying_decimals(), 6);
        assert_eq!(h.adapter.asset(), h.token.address);
    }
}
