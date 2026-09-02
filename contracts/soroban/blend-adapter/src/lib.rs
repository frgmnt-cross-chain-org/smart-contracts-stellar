// BlendAdapter — deploys idle Stellar USDC into a Blend Protocol lending pool
// (github.com/blend-capital/blend-contracts "V1", or blend-contracts-v2 "V2") and
// reports a conservative on-chain valuation back to AllocationManager / VaultAccounting.
//
// Only the configured Allocator (AllocationManager) may deposit/withdraw; only the
// adapter admin (governance) may change risk configuration or force an emergency exit.
//
// Decimal boundary rule (see docs/CROSS_CHAIN_FUSD_TECHNICAL_SPEC.md §7.6): every public
// entry point here speaks 6-decimal USDC. Conversion to/from the pool's native asset
// decimals (7 for Stellar SAC USDC) happens entirely inside this adapter.

#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, token, vec, Address, BytesN, Env};

mod blend_pool;
use blend_pool::{PoolClient, Request, REQUEST_TYPE_SUPPLY, REQUEST_TYPE_WITHDRAW};

/// Blend reserve exchange rates (b_rate / d_rate) are fixed-point with 9 decimals.
const RATE_SCALAR_9: i128 = 1_000_000_000;

/// "Withdraw everything" sentinel, expressed in the pool's native asset decimals.
///
/// Blend's `Withdraw` handler converts `request.amount` to a bToken-burn amount by
/// multiplying by the (9-decimal) rate scalar *before* capping to the caller's actual
/// balance (see blend-contracts `pool/src/pool/actions.rs::build_actions_from_request`).
/// Passing `i128::MAX` would overflow that intermediate multiplication and panic inside
/// the pool. `1e21` is large enough to exceed any realistic USDC position (it represents
/// ~1e14 real USDC at 7 decimals) while `1e21 * RATE_SCALAR_9 = 1e30` stays far below
/// `i128::MAX` (~1.7e38), so the pool's own cap-to-available-balance logic is what
/// actually executes.
const WITHDRAW_ALL_SENTINEL_NATIVE: i128 = 1_000_000_000_000_000_000_000;

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct BlendConfig {
    pub blend_pool: Address,
    /// The underlying asset address as registered with the pool (e.g. the Stellar USDC SAC).
    pub usdc_token: Address,
    /// 1 = blend-contracts (V1), 2 = blend-contracts-v2 (V2).
    /// Gates whether `get_reserve` is used for live, interest-accruing valuation — V1 pool
    /// contracts do not expose that view function publicly.
    pub pool_version: u32,
    /// Decimals of `usdc_token` as registered with the pool (7 for Stellar SAC USDC).
    pub asset_decimals: u32,
    /// Hard cap on principal this adapter may ever have deployed at once (6 decimals).
    pub max_protocol_exposure_6: i128,
    /// When true, `deposit` is blocked. Withdrawals always remain available.
    pub paused: bool,
    /// Must be explicitly set to `true` to initialize this adapter at all. This is not
    /// a functional flag — it's a deliberate deployment speed bump: this crate is
    /// retained only so a future, independently audited Blend V3 can be evaluated
    /// without a rewrite (see docs/CROSS_CHAIN_FUSD_TECHNICAL_SPEC.md §8 status note).
    /// Blend V2's backstop was drained in the August 2026 Comet AMM exploit and cannot
    /// be repaired; nothing should force this field to `true` without a governance
    /// decision made with that context in hand, made explicit in the deployment
    /// transaction itself rather than assumed by a default.
    pub deprecation_acknowledged: bool,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Allocator,
    StrategyId,
    Config,
    /// Conservative, adapter-tracked cost basis of capital currently deployed (6 decimals).
    /// This is the reported value for V1 pools (see `value_usdc_6`), and is always kept
    /// up to date for V2 pools too so a downgrade/misconfiguration never loses the figure.
    DeployedPrincipal6,
}

#[contract]
pub struct BlendAdapter;

#[contractimpl]
impl BlendAdapter {
    // ── Initialisation ────────────────────────────────────────────────────────

    pub fn initialize(
        e: Env,
        admin: Address,
        allocator: Address,
        strategy_id: BytesN<32>,
        config: BlendConfig,
    ) {
        assert!(!e.storage().instance().has(&DataKey::Admin), "already initialized");
        admin.require_auth();
        assert!(
            config.pool_version == 1 || config.pool_version == 2,
            "pool_version must be 1 or 2"
        );
        assert!(config.asset_decimals >= 6, "asset_decimals must be >= 6");
        assert!(config.max_protocol_exposure_6 >= 0, "max exposure must be non-negative");
        assert!(
            config.deprecation_acknowledged,
            "BlendAdapter is retained but not recommended for production: Blend V2's \
             backstop cannot be repaired after the August 2026 Comet AMM exploit. Set \
             deprecation_acknowledged = true only after an explicit \
             governance decision to deploy against a specific, independently evaluated \
             Blend pool (e.g. a future audited V3)."
        );

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

    /// Conservative value of the adapter's position in the Blend pool, in 6-decimal USDC.
    ///
    /// V2 pools: computed live from the pool-reported `b_rate` (via `get_reserve`), so
    /// accrued interest is reflected immediately.
    /// V1 pools: `blend-contracts` (V1) does not expose a public reserve-rate getter, so
    /// the adapter reports its own tracked deployed principal. This can only understate
    /// value, never overstate it — accrued V1 yield is recognized the moment it is
    /// actually withdrawn (see `withdraw`), never assumed in advance.
    pub fn value_usdc_6(e: Env) -> i128 {
        let config = Self::load_config(&e);
        if config.pool_version == 2 {
            let pool = PoolClient::new(&e, &config.blend_pool);
            let this = e.current_contract_address();
            let shares = Self::my_supply_shares(&pool, &this);
            if shares == 0 {
                return 0;
            }
            let reserve = pool.get_reserve(&config.usdc_token);
            let underlying_native = shares.checked_mul(reserve.b_rate).expect("overflow") / RATE_SCALAR_9;
            Self::native_to_usdc6(underlying_native, config.asset_decimals)
        } else {
            e.storage().instance().get(&DataKey::DeployedPrincipal6).unwrap_or(0)
        }
    }

    pub fn deployed_principal_6(e: Env) -> i128 {
        e.storage().instance().get(&DataKey::DeployedPrincipal6).unwrap_or(0)
    }

    // ── Allocator-gated actions ───────────────────────────────────────────────

    /// Supply `amount_6` USDC into the pool. `min_shares` is enforced against the
    /// adapter's own bToken (supply) share delta, read via `get_positions` — a view
    /// available identically on Blend V1 and V2, so slippage protection does not
    /// depend on `pool_version`.
    pub fn deposit(e: Env, caller: Address, amount_6: i128, min_shares: i128) {
        Self::auth_allocator(&e, &caller);
        assert!(amount_6 > 0, "amount must be positive");

        let config = Self::load_config(&e);
        assert!(!config.paused, "adapter paused");

        let principal_before: i128 = e.storage().instance().get(&DataKey::DeployedPrincipal6).unwrap_or(0);
        let new_principal = principal_before.checked_add(amount_6).expect("overflow");
        assert!(new_principal <= config.max_protocol_exposure_6, "max protocol exposure exceeded");

        let pool = PoolClient::new(&e, &config.blend_pool);
        let this = e.current_contract_address();

        let shares_before = Self::my_supply_shares(&pool, &this);

        let amount_native = Self::usdc6_to_native(amount_6, config.asset_decimals);
        let requests = vec![
            &e,
            Request {
                request_type: REQUEST_TYPE_SUPPLY,
                address: config.usdc_token.clone(),
                amount: amount_native,
            },
        ];
        pool.submit(&this, &this, &this, &requests);

        let shares_after = Self::my_supply_shares(&pool, &this);
        let minted = shares_after.saturating_sub(shares_before);
        assert!(minted >= min_shares, "slippage: min_shares not met");

        e.storage().instance().set(&DataKey::DeployedPrincipal6, &new_principal);
        e.events().publish((symbol_short!("Deposit"),), amount_6);
    }

    /// Withdraw exactly `amount_6` (best-effort — the pool may return less if its own
    /// liquidity is insufficient, in which case `min_out_6` reverts the call).
    /// Returns the amount actually received, measured as a USDC token balance delta —
    /// never trusted from the pool's return value or the request amount, mirroring the
    /// balance-delta discipline used for CCTP settlement elsewhere in this protocol.
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
    /// deposits) and bypasses the exposure cap so capital can always be pulled out.
    pub fn emergency_exit(e: Env, caller: Address, min_out_6: i128) -> i128 {
        Self::auth_admin(&e, &caller);
        let out_6 = Self::withdraw_internal(&e, None, min_out_6);
        e.events().publish((symbol_short!("EmerExit"),), out_6);
        out_6
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

    pub fn config(e: Env) -> BlendConfig {
        Self::load_config(&e)
    }

    /// Move `amount_6` of this adapter's own (already-withdrawn, idle-within-the-adapter)
    /// USDC balance to an arbitrary destination. Allocator-gated — used by
    /// AllocationManager to route funds a prior `withdraw`/`withdraw_all` call pulled out
    /// of the Blend pool back to the hub's idle balance (MintRedeemController).
    pub fn sweep(e: Env, caller: Address, to: Address, amount_6: i128) {
        Self::auth_allocator(&e, &caller);
        assert!(amount_6 > 0, "amount must be positive");
        let config = Self::load_config(&e);
        let amount_native = Self::usdc6_to_native(amount_6, config.asset_decimals);
        let token = token::Client::new(&e, &config.usdc_token);
        token.transfer(&e.current_contract_address(), &to, &amount_native);
        e.events().publish((symbol_short!("Sweep"),), amount_6);
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn withdraw_internal(e: &Env, requested_6: Option<i128>, min_out_6: i128) -> i128 {
        let config = Self::load_config(e);
        let pool = PoolClient::new(e, &config.blend_pool);
        let this = e.current_contract_address();
        let token = token::Client::new(e, &config.usdc_token);

        let balance_before = token.balance(&this);

        let amount_native = match requested_6 {
            Some(amount_6) => {
                assert!(amount_6 > 0, "amount must be positive");
                Self::usdc6_to_native(amount_6, config.asset_decimals)
            }
            None => WITHDRAW_ALL_SENTINEL_NATIVE,
        };

        let requests = vec![
            e,
            Request {
                request_type: REQUEST_TYPE_WITHDRAW,
                address: config.usdc_token.clone(),
                amount: amount_native,
            },
        ];
        pool.submit(&this, &this, &this, &requests);

        let balance_after = token.balance(&this);
        let received_native = balance_after.checked_sub(balance_before).expect("overflow");
        assert!(received_native >= 0, "withdraw produced a negative balance delta");
        let out_6 = Self::native_to_usdc6(received_native, config.asset_decimals);
        assert!(out_6 >= min_out_6, "slippage: min_out_6 not met");

        // `saturating_sub` on a *signed* i128 only guards against underflow at i128::MIN —
        // it does NOT clamp at zero. A real balance-delta withdraw can exceed the tracked
        // principal (that's accrued yield), so clamp explicitly.
        let principal: i128 = e.storage().instance().get(&DataKey::DeployedPrincipal6).unwrap_or(0);
        e.storage().instance().set(&DataKey::DeployedPrincipal6, &(principal - out_6).max(0));

        out_6
    }

    /// Sum of this contract's Blend "supply" (uncollateralized bToken) positions across
    /// every reserve. BlendAdapter only ever calls `Supply`/`Withdraw` for its single
    /// configured `usdc_token`, so in practice this map holds at most one entry — which
    /// means this works without knowing that reserve's index, identically on V1 and V2.
    fn my_supply_shares(pool: &PoolClient, holder: &Address) -> i128 {
        let positions = pool.get_positions(holder);
        let mut total: i128 = 0;
        for (_, amount) in positions.supply.iter() {
            total = total.checked_add(amount).expect("overflow");
        }
        total
    }

    fn usdc6_to_native(amount_6: i128, asset_decimals: u32) -> i128 {
        let scale = 10i128.pow(asset_decimals - 6);
        amount_6.checked_mul(scale).expect("overflow")
    }

    fn native_to_usdc6(amount_native: i128, asset_decimals: u32) -> i128 {
        let scale = 10i128.pow(asset_decimals - 6);
        amount_native / scale
    }

    fn load_config(e: &Env) -> BlendConfig {
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
    use crate::blend_pool::{PoolInterface, Positions, Reserve};
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::{token::StellarAssetClient, Map, Vec};

    // `MockBlendPoolClient` is auto-generated by `#[contractimpl]` below (one client type
    // accumulating methods from both the inherent `impl MockBlendPool` block — `init`,
    // `set_b_rate` — and the `impl PoolInterface for MockBlendPool` block).

    // ── Mock Blend pool ────────────────────────────────────────────────────────
    //
    // Implements the same `PoolInterface` the real adapter calls, moving real tokens
    // via `token::Client` so balance-delta assertions in the adapter are exercised
    // exactly as they would be against a real Blend pool. Exposes `get_reserve` /
    // `get_reserve_list` unconditionally (as a real V2 pool would) — the V1-vs-V2
    // behavioral difference under test lives in the *adapter*, which only calls those
    // when configured with `pool_version == 2`.

    #[contracttype]
    enum MockKey {
        Token,
        BRate,
        Supply(Address),
    }

    #[contract]
    struct MockBlendPool;

    #[contractimpl]
    impl MockBlendPool {
        pub fn init(e: Env, token: Address) {
            e.storage().instance().set(&MockKey::Token, &token);
            e.storage().instance().set(&MockKey::BRate, &RATE_SCALAR_9);
        }

        pub fn set_b_rate(e: Env, new_rate: i128) {
            e.storage().instance().set(&MockKey::BRate, &new_rate);
        }
    }

    #[contractimpl]
    impl PoolInterface for MockBlendPool {
        fn get_positions(e: Env, address: Address) -> Positions {
            let shares: i128 = e.storage().instance().get(&MockKey::Supply(address)).unwrap_or(0);
            let mut supply = Map::new(&e);
            if shares != 0 {
                supply.set(0u32, shares);
            }
            Positions {
                liabilities: Map::new(&e),
                collateral: Map::new(&e),
                supply,
            }
        }

        fn submit(e: Env, from: Address, spender: Address, to: Address, requests: Vec<Request>) -> Positions {
            let token_addr: Address = e.storage().instance().get(&MockKey::Token).unwrap();
            let token = token::Client::new(&e, &token_addr);
            let b_rate: i128 = e.storage().instance().get(&MockKey::BRate).unwrap();
            let mut shares: i128 = e.storage().instance().get(&MockKey::Supply(from.clone())).unwrap_or(0);

            for req in requests.iter() {
                if req.request_type == REQUEST_TYPE_SUPPLY {
                    token.transfer(&spender, &e.current_contract_address(), &req.amount);
                    let minted = req.amount.checked_mul(RATE_SCALAR_9).unwrap() / b_rate;
                    shares = shares.checked_add(minted).unwrap();
                } else if req.request_type == REQUEST_TYPE_WITHDRAW {
                    let requested_shares = req.amount.checked_mul(RATE_SCALAR_9).unwrap() / b_rate;
                    let (burn, out_native) = if requested_shares >= shares {
                        (shares, shares.checked_mul(b_rate).unwrap() / RATE_SCALAR_9)
                    } else {
                        (requested_shares, req.amount)
                    };
                    shares -= burn;
                    token.transfer(&e.current_contract_address(), &to, &out_native);
                } else {
                    panic!("unsupported request type in mock");
                }
            }
            e.storage().instance().set(&MockKey::Supply(from.clone()), &shares);
            Self::get_positions(e, from)
        }

        fn get_reserve(e: Env, asset: Address) -> Reserve {
            let b_rate: i128 = e.storage().instance().get(&MockKey::BRate).unwrap();
            Reserve {
                asset,
                index: 0,
                l_factor: 0,
                c_factor: 0,
                max_util: 0,
                last_time: 0,
                scalar: 10_000_000,
                d_rate: RATE_SCALAR_9,
                b_rate,
                ir_mod: 0,
                b_supply: 0,
                d_supply: 0,
                backstop_credit: 0,
            }
        }

        fn get_reserve_list(e: Env) -> Vec<Address> {
            let token_addr: Address = e.storage().instance().get(&MockKey::Token).unwrap();
            vec![&e, token_addr]
        }
    }

    // ── Test harness ───────────────────────────────────────────────────────────

    struct Harness {
        e: Env,
        admin: Address,
        allocator: Address,
        adapter: BlendAdapterClient<'static>,
        token: token::Client<'static>,
        pool: Address,
    }

    fn setup(pool_version: u32) -> Harness {
        let e = Env::default();
        // `deposit`/`withdraw` route `spender.require_auth()` through a nested call
        // (adapter -> pool.submit -> token.transfer), where the adapter's contract
        // address is not the *direct* caller of `token.transfer` (the pool is). Plain
        // `mock_all_auths()` only auto-satisfies "direct caller" contract auth; this
        // non-root case needs the `_allowing_non_root_auth` variant — exactly what
        // Blend's own test suite uses for the identical `execute_submit` pattern.
        e.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&e);
        let allocator = Address::generate(&e);

        let sac = e.register_stellar_asset_contract_v2(admin.clone());
        let token_addr = sac.address();
        let token = token::Client::new(&e, &token_addr);
        let token_admin = StellarAssetClient::new(&e, &token_addr);

        let pool_id = e.register_contract(None, MockBlendPool);
        let pool_setup = MockBlendPoolClient::new(&e, &pool_id);
        pool_setup.init(&token_addr);

        let adapter_id = e.register_contract(None, BlendAdapter);
        let adapter = BlendAdapterClient::new(&e, &adapter_id);

        let strategy_id = BytesN::from_array(&e, &[7u8; 32]);
        let config = BlendConfig {
            blend_pool: pool_id.clone(),
            usdc_token: token_addr.clone(),
            pool_version,
            asset_decimals: 7,
            max_protocol_exposure_6: 100_000_000,
            paused: false,
            deprecation_acknowledged: true,
        };
        adapter.initialize(&admin, &allocator, &strategy_id, &config);

        // Fund the allocator's "wallet" (here: the adapter itself acts as spender/holder
        // of the funds it moves — mirroring how AllocationManager would first move idle
        // USDC to the adapter contract before calling deposit).
        token_admin.mint(&adapter_id, &1_000_000_000); // 100 USDC at 7 decimals

        Harness { e, admin, allocator, adapter, token, pool: pool_id }
    }

    #[test]
    #[should_panic(expected = "pool_version must be 1 or 2")]
    fn initialize_rejects_bad_pool_version() {
        let e = Env::default();
        e.mock_all_auths();
        let admin = Address::generate(&e);
        let allocator = Address::generate(&e);
        let pool = Address::generate(&e);
        let token = Address::generate(&e);
        let adapter_id = e.register_contract(None, BlendAdapter);
        let adapter = BlendAdapterClient::new(&e, &adapter_id);
        let config = BlendConfig {
            blend_pool: pool,
            usdc_token: token,
            pool_version: 3,
            asset_decimals: 7,
            max_protocol_exposure_6: 100,
            paused: false,
            deprecation_acknowledged: true,
        };
        let strategy_id = BytesN::from_array(&e, &[1u8; 32]);
        adapter.initialize(&admin, &allocator, &strategy_id, &config);
    }

    #[test]
    #[should_panic(expected = "not recommended for production")]
    fn initialize_requires_explicit_deprecation_acknowledgment() {
        let e = Env::default();
        e.mock_all_auths();
        let admin = Address::generate(&e);
        let allocator = Address::generate(&e);
        let pool = Address::generate(&e);
        let token = Address::generate(&e);
        let adapter_id = e.register_contract(None, BlendAdapter);
        let adapter = BlendAdapterClient::new(&e, &adapter_id);
        let config = BlendConfig {
            blend_pool: pool,
            usdc_token: token,
            pool_version: 2,
            asset_decimals: 7,
            max_protocol_exposure_6: 100,
            paused: false,
            deprecation_acknowledged: false,
        };
        let strategy_id = BytesN::from_array(&e, &[2u8; 32]);
        adapter.initialize(&admin, &allocator, &strategy_id, &config);
    }

    #[test]
    fn deposit_moves_tokens_and_reports_value_v2() {
        let h = setup(2);
        h.adapter.deposit(&h.allocator, &10_000_000, &1); // 10 USDC (6-dec)

        assert_eq!(h.token.balance(&h.adapter.address), 900_000_000, "7-dec balance left the adapter");
        assert_eq!(h.token.balance(&h.pool), 100_000_000, "7-dec balance arrived at the pool");
        assert_eq!(h.adapter.deployed_principal_6(), 10_000_000);
        assert_eq!(h.adapter.value_usdc_6(), 10_000_000, "b_rate is 1.0 at deposit time");
    }

    #[test]
    fn value_usdc_6_reflects_accrued_interest_on_v2() {
        let h = setup(2);
        h.adapter.deposit(&h.allocator, &10_000_000, &1);

        // Simulate 10% interest accrual: b_rate goes from 1.0 to 1.1.
        let pool_admin = MockBlendPoolClient::new(&h.e, &h.pool);
        pool_admin.set_b_rate(&1_100_000_000);

        assert_eq!(h.adapter.value_usdc_6(), 11_000_000, "value reflects live b_rate");
        // Principal tracking is untouched until capital is actually withdrawn.
        assert_eq!(h.adapter.deployed_principal_6(), 10_000_000);
    }

    #[test]
    fn v1_pool_falls_back_to_principal_tracking_even_with_accrued_interest() {
        let h = setup(1);
        h.adapter.deposit(&h.allocator, &10_000_000, &1);

        let pool_admin = MockBlendPoolClient::new(&h.e, &h.pool);
        pool_admin.set_b_rate(&1_100_000_000);

        // V1 adapter never calls get_reserve, so it never sees the accrued interest —
        // it reports the conservative tracked principal instead.
        assert_eq!(h.adapter.value_usdc_6(), 10_000_000, "V1 conservative fallback");
    }

    #[test]
    fn withdraw_measures_real_balance_delta_not_requested_amount() {
        let h = setup(2);
        h.adapter.deposit(&h.allocator, &10_000_000, &1);

        // Interest accrued: the position (100M shares) is now worth 12 USDC, not 10.
        // Fund the pool with the extra 2 USDC it would really hold from borrower
        // interest payments, so it can actually honor the higher payout.
        let pool_admin = MockBlendPoolClient::new(&h.e, &h.pool);
        pool_admin.set_b_rate(&1_200_000_000);
        StellarAssetClient::new(&h.e, &h.token.address).mint(&h.pool, &20_000_000);

        // Request MORE than is actually available (15 USDC). The pool caps the payout
        // to what the position is really worth; the adapter must report what actually
        // moved (measured via a token balance delta), not the amount that was requested.
        let out = h.adapter.withdraw(&h.allocator, &15_000_000, &1);
        assert_eq!(out, 12_000_000, "balance delta reflects the real payout, not the requested figure");
        assert_eq!(h.adapter.deployed_principal_6(), 0, "principal cannot go negative — clamped at 0");
        assert_eq!(h.adapter.value_usdc_6(), 0, "position fully drained");
    }

    #[test]
    fn withdraw_all_returns_full_position() {
        let h = setup(2);
        h.adapter.deposit(&h.allocator, &10_000_000, &1);

        let out = h.adapter.withdraw_all(&h.allocator, &1);
        assert_eq!(out, 10_000_000);
        assert_eq!(h.adapter.value_usdc_6(), 0);
        assert_eq!(h.adapter.deployed_principal_6(), 0);
    }

    #[test]
    #[should_panic(expected = "slippage: min_shares not met")]
    fn deposit_enforces_min_shares() {
        let h = setup(2);
        h.adapter.deposit(&h.allocator, &10_000_000, &999_999_999_999);
    }

    #[test]
    #[should_panic(expected = "slippage: min_out_6 not met")]
    fn withdraw_enforces_min_out() {
        let h = setup(2);
        h.adapter.deposit(&h.allocator, &10_000_000, &1);
        h.adapter.withdraw(&h.allocator, &5_000_000, &5_000_001);
    }

    #[test]
    #[should_panic(expected = "not allocator")]
    fn only_allocator_can_deposit() {
        let h = setup(2);
        let attacker = Address::generate(&h.e);
        h.adapter.deposit(&attacker, &1_000_000, &0);
    }

    #[test]
    #[should_panic(expected = "adapter paused")]
    fn paused_blocks_deposit() {
        let h = setup(2);
        h.adapter.set_paused(&h.admin, &true);
        h.adapter.deposit(&h.allocator, &1_000_000, &0);
    }

    #[test]
    fn paused_does_not_block_withdraw() {
        let h = setup(2);
        h.adapter.deposit(&h.allocator, &10_000_000, &1);
        h.adapter.set_paused(&h.admin, &true);
        let out = h.adapter.withdraw(&h.allocator, &5_000_000, &1);
        assert_eq!(out, 5_000_000);
    }

    #[test]
    #[should_panic(expected = "max protocol exposure exceeded")]
    fn deposit_respects_max_exposure() {
        let h = setup(2);
        h.adapter.set_max_exposure(&h.admin, &5_000_000);
        h.adapter.deposit(&h.allocator, &5_000_001, &0);
    }

    #[test]
    fn emergency_exit_works_even_when_paused_and_bypasses_exposure_cap() {
        let h = setup(2);
        h.adapter.deposit(&h.allocator, &10_000_000, &1);
        h.adapter.set_paused(&h.admin, &true);
        h.adapter.set_max_exposure(&h.admin, &0);

        let out = h.adapter.emergency_exit(&h.admin, &1);
        assert_eq!(out, 10_000_000);
    }

    #[test]
    #[should_panic(expected = "not admin")]
    fn only_admin_can_emergency_exit() {
        let h = setup(2);
        h.adapter.deposit(&h.allocator, &10_000_000, &1);
        h.adapter.emergency_exit(&h.allocator, &1);
    }

    #[test]
    fn underlying_decimals_is_always_six() {
        let h = setup(2);
        assert_eq!(h.adapter.underlying_decimals(), 6);
        assert_eq!(h.adapter.asset(), h.token.address);
    }

    #[test]
    fn sweep_moves_withdrawn_funds_onward() {
        let h = setup(2);
        h.adapter.deposit(&h.allocator, &10_000_000, &1);
        h.adapter.withdraw(&h.allocator, &10_000_000, &1);

        let treasury = Address::generate(&h.e);
        h.adapter.sweep(&h.allocator, &treasury, &10_000_000);

        assert_eq!(h.token.balance(&treasury), 100_000_000, "swept funds arrived (7-dec)");
    }

    #[test]
    #[should_panic(expected = "not allocator")]
    fn only_allocator_can_sweep() {
        let h = setup(2);
        h.adapter.deposit(&h.allocator, &10_000_000, &1);
        h.adapter.withdraw(&h.allocator, &10_000_000, &1);
        let attacker = Address::generate(&h.e);
        let treasury = Address::generate(&h.e);
        h.adapter.sweep(&attacker, &treasury, &10_000_000);
    }
}
