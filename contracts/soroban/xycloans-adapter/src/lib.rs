// XycloansAdapter — deploys idle Stellar USDC into an xycLoans flash-loan liquidity pool
// (github.com/xycloo/xycloans) and reports a live valuation back to AllocationManager /
// VaultAccounting.
//
// Why xycLoans as a Blend replacement: it has no price oracle and no undercollateralized
// borrow/liquidation surface at all — a flash loan either repays in full within the same
// transaction or the whole transaction reverts, so there is no bad-debt state a pool can
// ever enter. That structurally rules out both failure modes behind this year's Stellar
// DeFi incidents (Reflector oracle manipulation against a Blend pool; a flash-loan chain
// through a separate AMM accounting bug that drained Blend's backstop). The trade-off is
// yield is flash-loan fee income, not term-loan interest — smaller and choppier, but real.
//
// Only the configured Allocator (AllocationManager) may deposit/withdraw; only the
// adapter admin (governance) may change risk configuration or force an emergency exit.
//
// Decimal boundary rule (see docs/CROSS_CHAIN_FUSD_TECHNICAL_SPEC.md §7.6): every public
// entry point here speaks 6-decimal USDC. Conversion to/from the pool's native asset
// decimals (7 for Stellar SAC USDC) happens entirely inside this adapter.

#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, token, Address, BytesN, Env};

mod xycloans_pool;
use xycloans_pool::PoolClient;

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct XycloansConfig {
    pub pool: Address,
    /// The pool's underlying asset (e.g. the Stellar USDC SAC).
    pub usdc_token: Address,
    /// Decimals of `usdc_token` (7 for Stellar SAC USDC).
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
}

#[contract]
pub struct XycloansAdapter;

#[contractimpl]
impl XycloansAdapter {
    // ── Initialisation ────────────────────────────────────────────────────────

    pub fn initialize(
        e: Env,
        admin: Address,
        allocator: Address,
        strategy_id: BytesN<32>,
        config: XycloansConfig,
    ) {
        assert!(!e.storage().instance().has(&DataKey::Admin), "already initialized");
        admin.require_auth();
        assert!(config.asset_decimals >= 6, "asset_decimals must be >= 6");
        assert!(config.max_protocol_exposure_6 >= 0, "max exposure must be non-negative");

        e.storage().instance().set(&DataKey::Admin, &admin);
        e.storage().instance().set(&DataKey::Allocator, &allocator);
        e.storage().instance().set(&DataKey::StrategyId, &strategy_id);
        e.storage().instance().set(&DataKey::Config, &config);
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

    /// Value of the adapter's position: principal shares (1:1 with deposited underlying,
    /// always exact — no rate-derived approximation like Blend's b_rate) plus the
    /// pool's currently-snapshotted matured-fee balance. That snapshot only refreshes
    /// when something calls `update_fee_rewards` for this address — until then, newly
    /// accrued (but not yet snapshotted) fees are simply absent from this figure. That
    /// can only ever make this function under-report real value, never over-report it,
    /// which is the safe direction for a number that ultimately backs VaultAccounting's
    /// solvency invariant.
    pub fn value_usdc_6(e: Env) -> i128 {
        let config = Self::load_config(&e);
        let pool = PoolClient::new(&e, &config.pool);
        let this = e.current_contract_address();
        let principal_native = pool.shares(&this);
        let matured_native = pool.matured(&this);
        let total_native = principal_native.checked_add(matured_native).expect("overflow");
        Self::native_to_usdc6(total_native, config.asset_decimals)
    }

    /// Deployed principal only (excludes matured, unharvested fees) — present for
    /// interface parity with `blend-adapter`/`defindex-adapter`, both of which expose
    /// this as a locally-tracked figure. xycLoans needs no local tracking for it: shares
    /// are always exactly 1:1 with deposited principal, so this is just the pool's own
    /// `shares` balance read live rather than cached.
    pub fn deployed_principal_6(e: Env) -> i128 {
        let config = Self::load_config(&e);
        Self::principal_usdc6(&e, &config)
    }

    // ── Allocator-gated actions ───────────────────────────────────────────────

    /// Supply `amount_6` USDC into the pool. `min_shares` is enforced against the
    /// adapter's own principal-share delta (native decimals) — always exactly `amount`
    /// in the honest case, but measured rather than assumed, matching the balance-delta
    /// discipline used throughout this protocol.
    pub fn deposit(e: Env, caller: Address, amount_6: i128, min_shares: i128) {
        Self::auth_allocator(&e, &caller);
        assert!(amount_6 > 0, "amount must be positive");

        let config = Self::load_config(&e);
        assert!(!config.paused, "adapter paused");

        let pool = PoolClient::new(&e, &config.pool);
        let this = e.current_contract_address();

        // Single `pool.shares` read serves both the exposure-cap check and the
        // pre-deposit baseline for the post-deposit slippage check below — no need for
        // two separate cross-contract calls returning the identical value.
        let shares_before = pool.shares(&this);
        let principal_before_6 = Self::native_to_usdc6(shares_before, config.asset_decimals);
        let new_principal_6 = principal_before_6.checked_add(amount_6).expect("overflow");
        assert!(new_principal_6 <= config.max_protocol_exposure_6, "max protocol exposure exceeded");

        let amount_native = Self::usdc6_to_native(amount_6, config.asset_decimals);
        pool.deposit(&this, &amount_native);
        let shares_after = pool.shares(&this);

        let minted = shares_after.saturating_sub(shares_before);
        assert!(minted >= min_shares, "slippage: min_shares not met");

        e.events().publish((symbol_short!("Deposit"),), amount_6);
    }

    /// Withdraw `amount_6` of principal, harvesting any matured fees along the way (so
    /// the balance delta returned can legitimately exceed `amount_6` — this mirrors the
    /// same "measure the real transfer, not the request" rule used for Blend and CCTP
    /// settlement elsewhere in this protocol).
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
    /// a prior withdrawal pulled out of the pool back to the hub's idle balance.
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

    pub fn config(e: Env) -> XycloansConfig {
        Self::load_config(&e)
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// `withdraw`/`withdraw_all`/`emergency_exit` share this: always harvest matured
    /// fees first (cheap, and keeps `value_usdc_6` accurate), then withdraw the
    /// requested principal amount (or the entire remaining principal for a full exit).
    /// The returned amount is always measured as a real token balance delta.
    fn withdraw_internal(e: &Env, requested_6: Option<i128>, min_out_6: i128) -> i128 {
        let config = Self::load_config(e);
        let pool = PoolClient::new(e, &config.pool);
        let this = e.current_contract_address();
        let token = token::Client::new(e, &config.usdc_token);

        let balance_before = token.balance(&this);

        pool.update_fee_rewards(&this);
        if pool.matured(&this) > 0 {
            pool.withdraw_matured(&this);
        }

        match requested_6 {
            Some(amount_6) => {
                assert!(amount_6 > 0, "amount must be positive");
                let amount_native = Self::usdc6_to_native(amount_6, config.asset_decimals);
                let available = pool.shares(&this);
                assert!(available >= amount_native, "insufficient principal in pool");
                pool.withdraw(&this, &amount_native);
            }
            None => {
                let available = pool.shares(&this);
                if available > 0 {
                    pool.withdraw(&this, &available);
                }
            }
        }

        let balance_after = token.balance(&this);
        let received_native = balance_after.checked_sub(balance_before).expect("overflow");
        assert!(received_native >= 0, "withdraw produced a negative balance delta");
        let out_6 = Self::native_to_usdc6(received_native, config.asset_decimals);
        assert!(out_6 >= min_out_6, "slippage: min_out_6 not met");

        out_6
    }

    /// This adapter's principal-only value (excludes unharvested matured fees), in
    /// 6-decimal USDC — used only for the exposure-cap check on deposit, which is a cap
    /// on *deployed principal*, not on total position value including yield.
    fn principal_usdc6(e: &Env, config: &XycloansConfig) -> i128 {
        let pool = PoolClient::new(e, &config.pool);
        let this = e.current_contract_address();
        Self::native_to_usdc6(pool.shares(&this), config.asset_decimals)
    }

    fn usdc6_to_native(amount_6: i128, asset_decimals: u32) -> i128 {
        let scale = 10i128.pow(asset_decimals - 6);
        amount_6.checked_mul(scale).expect("overflow")
    }

    fn native_to_usdc6(amount_native: i128, asset_decimals: u32) -> i128 {
        let scale = 10i128.pow(asset_decimals - 6);
        amount_native / scale
    }

    fn load_config(e: &Env) -> XycloansConfig {
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
    use crate::xycloans_pool::PoolInterface;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::token::StellarAssetClient;

    // ── Mock xycLoans pool ────────────────────────────────────────────────────
    //
    // Implements the same `PoolInterface` the real adapter calls, moving real tokens
    // via `token::Client` so balance-delta assertions are exercised exactly as they
    // would be against the real pool. Adds a test-only `credit_fee` to simulate flash
    // loan fee income landing in the pool (a real flash loan borrower would have repaid
    // principal + fee into the pool's own balance; `credit_fee` stands in for that).

    #[contracttype]
    enum MockKey {
        Token,
        Shares(Address),
        FeePerShareParticular(Address),
        Matured(Address),
        FeePerShareUniversal,
        TotalSupply,
    }

    const RATE_SCALAR: i128 = 1_000_000_000_000;

    #[contract]
    struct MockXycloansPool;

    #[contractimpl]
    impl MockXycloansPool {
        pub fn init(e: Env, token: Address) {
            e.storage().instance().set(&MockKey::Token, &token);
            e.storage().instance().set(&MockKey::FeePerShareUniversal, &0_i128);
            e.storage().instance().set(&MockKey::TotalSupply, &0_i128);
        }

        /// Test-only: simulate `amount` of real flash-loan fee income landing in the
        /// pool, distributed pro-rata to current shareholders (mirrors
        /// `update_fee_per_share_universal` in the real contract).
        pub fn credit_fee(e: Env, from: Address, amount: i128) {
            let token_addr: Address = e.storage().instance().get(&MockKey::Token).unwrap();
            token::Client::new(&e, &token_addr).transfer(&from, &e.current_contract_address(), &amount);

            let total_supply: i128 = e.storage().instance().get(&MockKey::TotalSupply).unwrap_or(0);
            if total_supply > 0 {
                let fps: i128 = e.storage().instance().get(&MockKey::FeePerShareUniversal).unwrap_or(0);
                let delta = amount.checked_mul(RATE_SCALAR).unwrap() / total_supply;
                e.storage().instance().set(&MockKey::FeePerShareUniversal, &(fps + delta));
            }
        }

        fn update_rewards(e: &Env, addr: &Address) {
            let fps: i128 = e.storage().instance().get(&MockKey::FeePerShareUniversal).unwrap_or(0);
            let particular: i128 = e.storage().instance().get(&MockKey::FeePerShareParticular(addr.clone())).unwrap_or(0);
            let shares: i128 = e.storage().instance().get(&MockKey::Shares(addr.clone())).unwrap_or(0);
            let earned = shares.checked_mul(fps - particular).unwrap() / RATE_SCALAR;
            let matured: i128 = e.storage().instance().get(&MockKey::Matured(addr.clone())).unwrap_or(0);
            e.storage().instance().set(&MockKey::Matured(addr.clone()), &(matured + earned));
            e.storage().instance().set(&MockKey::FeePerShareParticular(addr.clone()), &fps);
        }
    }

    #[contractimpl]
    impl PoolInterface for MockXycloansPool {
        fn deposit(e: Env, from: Address, amount: i128) {
            Self::update_rewards(&e, &from);
            let token_addr: Address = e.storage().instance().get(&MockKey::Token).unwrap();
            token::Client::new(&e, &token_addr).transfer(&from, &e.current_contract_address(), &amount);
            let shares: i128 = e.storage().instance().get(&MockKey::Shares(from.clone())).unwrap_or(0);
            e.storage().instance().set(&MockKey::Shares(from.clone()), &(shares + amount));
            let total_supply: i128 = e.storage().instance().get(&MockKey::TotalSupply).unwrap_or(0);
            e.storage().instance().set(&MockKey::TotalSupply, &(total_supply + amount));
        }

        fn update_fee_rewards(e: Env, addr: Address) {
            Self::update_rewards(&e, &addr);
        }

        fn withdraw_matured(e: Env, addr: Address) {
            let matured: i128 = e.storage().instance().get(&MockKey::Matured(addr.clone())).unwrap_or(0);
            assert!(matured > 0, "NoFeesMatured");
            let token_addr: Address = e.storage().instance().get(&MockKey::Token).unwrap();
            token::Client::new(&e, &token_addr).transfer(&e.current_contract_address(), &addr, &matured);
            e.storage().instance().set(&MockKey::Matured(addr), &0_i128);
        }

        fn withdraw(e: Env, addr: Address, amount: i128) {
            let shares: i128 = e.storage().instance().get(&MockKey::Shares(addr.clone())).unwrap_or(0);
            assert!(shares >= amount && amount > 0, "InvalidShareBalance");
            let token_addr: Address = e.storage().instance().get(&MockKey::Token).unwrap();
            token::Client::new(&e, &token_addr).transfer(&e.current_contract_address(), &addr, &amount);
            e.storage().instance().set(&MockKey::Shares(addr.clone()), &(shares - amount));
            let total_supply: i128 = e.storage().instance().get(&MockKey::TotalSupply).unwrap_or(0);
            e.storage().instance().set(&MockKey::TotalSupply, &(total_supply - amount));
        }

        fn shares(e: Env, addr: Address) -> i128 {
            e.storage().instance().get(&MockKey::Shares(addr)).unwrap_or(0)
        }

        fn matured(e: Env, addr: Address) -> i128 {
            e.storage().instance().get(&MockKey::Matured(addr)).unwrap_or(0)
        }
    }

    // ── Test harness ───────────────────────────────────────────────────────────

    struct Harness {
        e: Env,
        admin: Address,
        allocator: Address,
        adapter: XycloansAdapterClient<'static>,
        token: token::Client<'static>,
        pool: Address,
    }

    fn setup() -> Harness {
        let e = Env::default();
        // See blend-adapter for why this non-root-auth variant is required: `deposit`/
        // `withdraw` route `require_auth` through a nested call (adapter -> pool ->
        // token.transfer), where the adapter is not the *direct* caller of the token.
        e.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&e);
        let allocator = Address::generate(&e);

        let sac = e.register_stellar_asset_contract_v2(admin.clone());
        let token_addr = sac.address();
        let token = token::Client::new(&e, &token_addr);
        let token_admin = StellarAssetClient::new(&e, &token_addr);

        let pool_id = e.register_contract(None, MockXycloansPool);
        let pool_setup = MockXycloansPoolClient::new(&e, &pool_id);
        pool_setup.init(&token_addr);

        let adapter_id = e.register_contract(None, XycloansAdapter);
        let adapter = XycloansAdapterClient::new(&e, &adapter_id);

        let strategy_id = BytesN::from_array(&e, &[11u8; 32]);
        let config = XycloansConfig {
            pool: pool_id.clone(),
            usdc_token: token_addr.clone(),
            asset_decimals: 7,
            max_protocol_exposure_6: 100_000_000,
            paused: false,
        };
        adapter.initialize(&admin, &allocator, &strategy_id, &config);

        token_admin.mint(&adapter_id, &1_000_000_000); // 100 USDC at 7 decimals

        Harness { e, admin, allocator, adapter, token, pool: pool_id }
    }

    #[test]
    fn deposit_moves_tokens_and_reports_exact_value() {
        let h = setup();
        h.adapter.deposit(&h.allocator, &10_000_000, &1);

        assert_eq!(h.token.balance(&h.adapter.address), 900_000_000);
        assert_eq!(h.token.balance(&h.pool), 100_000_000);
        assert_eq!(h.adapter.value_usdc_6(), 10_000_000, "1:1 shares, no rate approximation");
        assert_eq!(h.adapter.deployed_principal_6(), 10_000_000);
    }

    #[test]
    fn value_reflects_matured_fees_separately_from_principal() {
        let h = setup();
        h.adapter.deposit(&h.allocator, &10_000_000, &1);

        // Simulate a flash-loan borrower repaying with a 0.5 USDC fee (funded by a
        // third party, not the adapter — mirrors real flash-loan fee income).
        let fee_payer = Address::generate(&h.e);
        StellarAssetClient::new(&h.e, &h.token.address).mint(&fee_payer, &5_000_000);
        let pool_admin = MockXycloansPoolClient::new(&h.e, &h.pool);
        pool_admin.credit_fee(&fee_payer, &5_000_000);

        // `matured()` is a snapshot that only refreshes when `update_fee_rewards` runs
        // for this address. Before that happens, `value_usdc_6` conservatively
        // under-reports rather than over-reports — the same safe direction as Blend's
        // V1 fallback.
        assert_eq!(h.adapter.value_usdc_6(), 10_000_000, "matured fee not yet snapshotted");

        pool_admin.update_fee_rewards(&h.adapter.address);
        assert_eq!(h.adapter.value_usdc_6(), 10_500_000, "principal + matured fee after refresh");
    }

    #[test]
    fn withdraw_all_harvests_matured_fees_and_principal_together() {
        let h = setup();
        h.adapter.deposit(&h.allocator, &10_000_000, &1);

        let fee_payer = Address::generate(&h.e);
        StellarAssetClient::new(&h.e, &h.token.address).mint(&fee_payer, &5_000_000);
        MockXycloansPoolClient::new(&h.e, &h.pool).credit_fee(&fee_payer, &5_000_000);

        let out = h.adapter.withdraw_all(&h.allocator, &1);
        assert_eq!(out, 10_500_000, "both principal and matured fee returned");
        assert_eq!(h.adapter.value_usdc_6(), 0);
    }

    #[test]
    fn partial_withdraw_also_harvests_available_matured_fees() {
        let h = setup();
        h.adapter.deposit(&h.allocator, &10_000_000, &1);

        let fee_payer = Address::generate(&h.e);
        // Native (7-dec) units: 10_000_000 == 1 USDC.
        StellarAssetClient::new(&h.e, &h.token.address).mint(&fee_payer, &10_000_000);
        MockXycloansPoolClient::new(&h.e, &h.pool).credit_fee(&fee_payer, &10_000_000);

        // Ask for 3 USDC of principal; the 1 USDC matured fee rides along.
        let out = h.adapter.withdraw(&h.allocator, &3_000_000, &1);
        assert_eq!(out, 4_000_000, "3 USDC requested principal + 1 USDC harvested fee");
        assert_eq!(h.adapter.value_usdc_6(), 7_000_000, "7 USDC principal remains");
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

    #[test]
    #[should_panic(expected = "NoFeesMatured")]
    fn mock_pool_rejects_withdraw_matured_with_nothing_accrued() {
        // Sanity check on the mock itself: exercises the same guard the real pool has,
        // so `withdraw_internal`'s `matured(this) > 0` guard is proven necessary.
        let h = setup();
        h.adapter.deposit(&h.allocator, &10_000_000, &1);
        let pool_client = MockXycloansPoolClient::new(&h.e, &h.pool);
        pool_client.withdraw_matured(&h.adapter.address);
    }
}
