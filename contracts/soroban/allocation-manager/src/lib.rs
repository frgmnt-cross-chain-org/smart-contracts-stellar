// AllocationManager — orchestrates moving idle Stellar USDC into (and out of)
// governance-approved yield strategies, e.g. `blend-adapter`.
//
// This contract is the sole "Allocator" for VaultAccounting, MintRedeemController, and
// every strategy adapter it manages — each of those contracts gates its
// strategy-related mutating calls to only accept calls from the address configured as
// their Allocator, which must be set (by their own admin) to this contract's address.
//
// Role model:
//   - `admin`    (governance/timelock): register strategies, enable/disable them.
//   - `operator` (trader/allocation-operator): execute allocate/deallocate within the
//                 caps VaultAccounting enforces (debt ceiling) and the flags set here.
//   - anyone:     refresh a strategy's reported value (report_value) — this can only
//                 ever move VaultAccounting's books to match the adapter's own
//                 conservative valuation, never fabricate backing, so it is safe to
//                 leave permissionless (keeper-friendly).

#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, Address, BytesN, Env};

mod vault_accounting {
    use soroban_sdk::{contractclient, Address, BytesN, Env};

    #[allow(dead_code)]
    #[contractclient(name = "Client")]
    pub trait VaultAccountingInterface {
        fn register_strategy(e: Env, caller: Address, strategy_id: BytesN<32>, adapter: Address, debt_ceiling_6: i128);
        fn move_idle_to_strategy(e: Env, caller: Address, strategy_id: BytesN<32>, amount_6: i128);
        fn move_strategy_to_idle(e: Env, caller: Address, strategy_id: BytesN<32>, amount_6: i128);
        fn report_strategy_value(e: Env, caller: Address, strategy_id: BytesN<32>, new_value_6: i128);
    }
}

mod controller {
    use soroban_sdk::{contractclient, Address, Env};

    #[allow(dead_code)]
    #[contractclient(name = "Client")]
    pub trait ControllerInterface {
        fn move_idle_to_allocator(e: Env, caller: Address, to: Address, amount_7: i128);
        fn usdc_sac(e: Env) -> Address;
    }
}

/// Generic strategy-adapter interface — implemented by `blend-adapter` and any future
/// adapter this protocol integrates. All amounts are 6-decimal USDC; the adapter itself
/// owns the boundary conversion to the underlying venue's native decimals.
mod strategy_adapter {
    use soroban_sdk::{contractclient, Address, Env};

    #[allow(dead_code)]
    #[contractclient(name = "Client")]
    pub trait StrategyAdapterInterface {
        fn asset(e: Env) -> Address;
        fn deposit(e: Env, caller: Address, amount_6: i128, min_shares: i128);
        fn withdraw(e: Env, caller: Address, amount_6: i128, min_out_6: i128) -> i128;
        fn withdraw_all(e: Env, caller: Address, min_out_6: i128) -> i128;
        fn emergency_exit(e: Env, caller: Address, min_out_6: i128) -> i128;
        fn value_usdc_6(e: Env) -> i128;
        fn sweep(e: Env, caller: Address, to: Address, amount_6: i128);
    }
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct StrategyEntry {
    pub adapter: Address,
    pub active: bool,
    pub deposit_enabled: bool,
    pub withdraw_enabled: bool,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Operator,
    VaultAccounting,
    Controller,
    Strategy(BytesN<32>),
}

#[contract]
pub struct AllocationManager;

#[contractimpl]
impl AllocationManager {
    // ── Initialisation ────────────────────────────────────────────────────────

    pub fn initialize(e: Env, admin: Address, operator: Address, vault_accounting: Address, controller: Address) {
        assert!(!e.storage().instance().has(&DataKey::Admin), "already initialized");
        admin.require_auth();
        e.storage().instance().set(&DataKey::Admin, &admin);
        e.storage().instance().set(&DataKey::Operator, &operator);
        e.storage().instance().set(&DataKey::VaultAccounting, &vault_accounting);
        e.storage().instance().set(&DataKey::Controller, &controller);
    }

    pub fn set_operator(e: Env, caller: Address, new_operator: Address) {
        Self::auth_admin(&e, &caller);
        e.storage().instance().set(&DataKey::Operator, &new_operator);
    }

    // ── Governance: strategy whitelist ────────────────────────────────────────

    /// Registers a strategy on both this contract and VaultAccounting. `caller` must be
    /// the shared governance address that administers both AllocationManager and
    /// VaultAccounting — the same `caller` (and its auth) is relayed through to the
    /// nested VaultAccounting.register_strategy call.
    pub fn register_strategy(
        e: Env,
        caller: Address,
        strategy_id: BytesN<32>,
        adapter: Address,
        debt_ceiling_6: i128,
    ) {
        Self::auth_admin(&e, &caller);
        assert!(
            !e.storage().persistent().has(&DataKey::Strategy(strategy_id.clone())),
            "strategy already registered"
        );

        // The adapter's own configured asset must match the hub's actual USDC SAC — a
        // misregistered adapter (wrong `usdc_token` in its own config) would receive
        // real USDC from the controller but run its balance-delta accounting against a
        // different token entirely, silently breaking every slippage/valuation check it
        // relies on.
        let adapter_asset = strategy_adapter::Client::new(&e, &adapter).asset();
        let hub_usdc = Self::controller_client(&e).usdc_sac();
        assert!(adapter_asset == hub_usdc, "adapter asset does not match hub USDC SAC");

        let vault = Self::vault_client(&e);
        vault.register_strategy(&caller, &strategy_id, &adapter, &debt_ceiling_6);

        let entry = StrategyEntry {
            adapter,
            active: true,
            deposit_enabled: true,
            withdraw_enabled: true,
        };
        let key = DataKey::Strategy(strategy_id.clone());
        e.storage().persistent().set(&key, &entry);
        // 5-year floor TTL — a registered strategy may go untouched (paused, or simply
        // not allocated/deallocated/report_value'd) for long stretches and must not be
        // archived out from under `load_strategy` in the meantime.
        e.storage().persistent().extend_ttl(&key, 13_140_000, 13_140_000);
        e.events().publish((symbol_short!("StratReg"),), strategy_id);
    }

    pub fn set_strategy_flags(
        e: Env,
        caller: Address,
        strategy_id: BytesN<32>,
        active: bool,
        deposit_enabled: bool,
        withdraw_enabled: bool,
    ) {
        Self::auth_admin(&e, &caller);
        let mut entry = Self::load_strategy(&e, &strategy_id);
        entry.active = active;
        entry.deposit_enabled = deposit_enabled;
        entry.withdraw_enabled = withdraw_enabled;
        let key = DataKey::Strategy(strategy_id);
        e.storage().persistent().set(&key, &entry);
        e.storage().persistent().extend_ttl(&key, 13_140_000, 13_140_000);
    }

    pub fn strategy(e: Env, strategy_id: BytesN<32>) -> StrategyEntry {
        Self::load_strategy(&e, &strategy_id)
    }

    // ── Operator: capital movement ────────────────────────────────────────────

    /// Move `amount_6` of idle USDC into `strategy_id`'s adapter and deposit it into
    /// the underlying venue, in one atomic call:
    ///   1. real USDC moves controller -> adapter (`Controller.move_idle_to_allocator`)
    ///   2. VaultAccounting books idle -> strategy (`move_idle_to_strategy`)
    ///   3. the adapter deploys the capital into its venue (`adapter.deposit`)
    /// If any step fails (e.g. the venue's own slippage/cap checks), the whole
    /// transaction — including the token transfer — reverts atomically.
    pub fn allocate(e: Env, caller: Address, strategy_id: BytesN<32>, amount_6: i128, min_shares: i128) {
        Self::auth_operator(&e, &caller);
        assert!(amount_6 > 0, "amount must be positive");

        let entry = Self::load_strategy(&e, &strategy_id);
        assert!(entry.active, "strategy not active");
        assert!(entry.deposit_enabled, "strategy deposits disabled");

        let this = e.current_contract_address();
        let amount_7 = amount_6.checked_mul(10).expect("overflow");

        Self::controller_client(&e).move_idle_to_allocator(&this, &entry.adapter, &amount_7);
        Self::vault_client(&e).move_idle_to_strategy(&this, &strategy_id, &amount_6);
        strategy_adapter::Client::new(&e, &entry.adapter).deposit(&this, &amount_6, &min_shares);

        e.events().publish((symbol_short!("Alloc"), strategy_id), amount_6);
    }

    /// Withdraw `amount_6` from `strategy_id` and route the actually-received amount
    /// (never the requested figure) back to the hub's idle balance. Returns the amount
    /// actually returned to idle.
    pub fn deallocate(e: Env, caller: Address, strategy_id: BytesN<32>, amount_6: i128, min_out_6: i128) -> i128 {
        Self::auth_operator(&e, &caller);
        assert!(amount_6 > 0, "amount must be positive");
        let entry = Self::load_strategy(&e, &strategy_id);
        assert!(entry.withdraw_enabled, "strategy withdrawals disabled");

        let out_6 = strategy_adapter::Client::new(&e, &entry.adapter)
            .withdraw(&e.current_contract_address(), &amount_6, &min_out_6);
        Self::settle_withdrawal(&e, &strategy_id, &entry, out_6);
        out_6
    }

    /// Same as `deallocate` but pulls the entire position.
    pub fn deallocate_all(e: Env, caller: Address, strategy_id: BytesN<32>, min_out_6: i128) -> i128 {
        Self::auth_operator(&e, &caller);
        let entry = Self::load_strategy(&e, &strategy_id);
        assert!(entry.withdraw_enabled, "strategy withdrawals disabled");

        let out_6 = strategy_adapter::Client::new(&e, &entry.adapter)
            .withdraw_all(&e.current_contract_address(), &min_out_6);
        Self::settle_withdrawal(&e, &strategy_id, &entry, out_6);
        out_6
    }

    /// Governance-only: pull the entire position out regardless of `withdraw_enabled` /
    /// `active` / the adapter's own paused flag or exposure cap.
    ///
    /// Relays the ORIGINAL `caller` to the adapter, not this contract's own address:
    /// every adapter's `emergency_exit` is gated on that adapter's own configured
    /// `Admin` (governance), which is a separate identity from `Allocator` (this
    /// contract's own address). This only works when the same governance address
    /// administers both AllocationManager and each registered adapter — the same
    /// shared-governance assumption `register_strategy` already documents for
    /// VaultAccounting.
    pub fn emergency_exit(e: Env, caller: Address, strategy_id: BytesN<32>, min_out_6: i128) -> i128 {
        Self::auth_admin(&e, &caller);
        let entry = Self::load_strategy(&e, &strategy_id);

        let out_6 = strategy_adapter::Client::new(&e, &entry.adapter)
            .emergency_exit(&caller, &min_out_6);
        Self::settle_withdrawal(&e, &strategy_id, &entry, out_6);
        out_6
    }

    /// Permissionless: pull the adapter's own conservative valuation and reflect it in
    /// VaultAccounting. Can only move total_strategy_value_6 to match the adapter's
    /// report — never touches mint_allowance_6, so this can never fabricate backing.
    pub fn report_value(e: Env, strategy_id: BytesN<32>) -> i128 {
        let entry = Self::load_strategy(&e, &strategy_id);
        let value_6 = strategy_adapter::Client::new(&e, &entry.adapter).value_usdc_6();
        Self::vault_client(&e).report_strategy_value(&e.current_contract_address(), &strategy_id, &value_6);
        value_6
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn settle_withdrawal(e: &Env, strategy_id: &BytesN<32>, entry: &StrategyEntry, out_6: i128) {
        let this = e.current_contract_address();
        if out_6 > 0 {
            Self::vault_client(e).move_strategy_to_idle(&this, strategy_id, &out_6);
            let controller_addr: Address = e.storage().instance().get(&DataKey::Controller).unwrap();
            let adapter = strategy_adapter::Client::new(e, &entry.adapter);
            adapter.sweep(&this, &controller_addr, &out_6);
        }
        e.events().publish((symbol_short!("Dealloc"), strategy_id.clone()), out_6);
    }

    fn load_strategy(e: &Env, strategy_id: &BytesN<32>) -> StrategyEntry {
        e.storage().persistent().get(&DataKey::Strategy(strategy_id.clone()))
            .expect("strategy not registered")
    }

    fn vault_client(e: &Env) -> vault_accounting::Client<'_> {
        let addr: Address = e.storage().instance().get(&DataKey::VaultAccounting).unwrap();
        vault_accounting::Client::new(e, &addr)
    }

    fn controller_client(e: &Env) -> controller::Client<'_> {
        let addr: Address = e.storage().instance().get(&DataKey::Controller).unwrap();
        controller::Client::new(e, &addr)
    }

    fn auth_admin(e: &Env, caller: &Address) {
        let admin: Address = e.storage().instance().get(&DataKey::Admin).unwrap();
        assert!(*caller == admin, "not admin");
        caller.require_auth();
    }

    fn auth_operator(e: &Env, caller: &Address) {
        let operator: Address = e.storage().instance().get(&DataKey::Operator).unwrap();
        assert!(*caller == operator, "not operator");
        caller.require_auth();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────
//
// Exercises AllocationManager's own orchestration/role logic end-to-end against the
// real fusd-token, vault-accounting, and mint-redeem-controller contracts, backed by a
// lightweight mock strategy adapter (implementing the same `StrategyAdapterInterface`
// blend-adapter does) — Blend-specific correctness (b_rate math, V1/V2 differences,
// pool request handling) is covered by blend-adapter's own test suite; this suite
// covers whether AllocationManager sequences and gates the cross-contract calls
// correctly.

#[cfg(test)]
mod test {
    use super::*;
    use ::fusd_token::{FusdToken, FusdTokenClient};
    use ::mint_redeem_controller::{MintRedeemController, MintRedeemControllerClient};
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::{token, token::StellarAssetClient};
    use ::vault_accounting::{VaultAccounting, VaultAccountingClient};
    use crate::strategy_adapter::StrategyAdapterInterface;

    // ── Mock strategy adapter ────────────────────────────────────────────────

    #[contracttype]
    enum MockKey {
        Admin,
        Allocator,
        Token,
        Value,
    }

    #[contract]
    struct MockAdapter;

    #[contractimpl]
    impl MockAdapter {
        // Mirrors the real adapters' `Admin` (governance) / `Allocator`
        // (AllocationManager) split exactly — `emergency_exit` gates on `Admin`,
        // everything else gates on `Allocator`. An earlier version of this mock gated
        // `emergency_exit` on `Allocator` too, which masked a real bug where
        // AllocationManager relayed its own contract address instead of the original
        // admin caller into the adapter's `emergency_exit` call.
        pub fn init(e: Env, admin: Address, allocator: Address, token: Address) {
            e.storage().instance().set(&MockKey::Admin, &admin);
            e.storage().instance().set(&MockKey::Allocator, &allocator);
            e.storage().instance().set(&MockKey::Token, &token);
            e.storage().instance().set(&MockKey::Value, &0_i128);
        }

        pub fn set_value(e: Env, new_value_6: i128) {
            e.storage().instance().set(&MockKey::Value, &new_value_6);
        }

        fn auth(e: &Env, caller: &Address) {
            let allocator: Address = e.storage().instance().get(&MockKey::Allocator).unwrap();
            assert!(*caller == allocator, "not allocator");
            caller.require_auth();
        }

        fn auth_admin(e: &Env, caller: &Address) {
            let admin: Address = e.storage().instance().get(&MockKey::Admin).unwrap();
            assert!(*caller == admin, "not admin");
            caller.require_auth();
        }
    }

    #[contractimpl]
    impl strategy_adapter::StrategyAdapterInterface for MockAdapter {
        fn asset(e: Env) -> Address {
            e.storage().instance().get(&MockKey::Token).unwrap()
        }

        fn deposit(e: Env, caller: Address, amount_6: i128, _min_shares: i128) {
            Self::auth(&e, &caller);
            let v: i128 = e.storage().instance().get(&MockKey::Value).unwrap_or(0);
            e.storage().instance().set(&MockKey::Value, &(v + amount_6));
        }

        fn withdraw(e: Env, caller: Address, amount_6: i128, min_out_6: i128) -> i128 {
            Self::auth(&e, &caller);
            let v: i128 = e.storage().instance().get(&MockKey::Value).unwrap_or(0);
            assert!(v >= amount_6, "insufficient mock position");
            assert!(amount_6 >= min_out_6, "slippage");
            e.storage().instance().set(&MockKey::Value, &(v - amount_6));
            amount_6
        }

        fn withdraw_all(e: Env, caller: Address, min_out_6: i128) -> i128 {
            Self::auth(&e, &caller);
            let v: i128 = e.storage().instance().get(&MockKey::Value).unwrap_or(0);
            assert!(v >= min_out_6, "slippage");
            e.storage().instance().set(&MockKey::Value, &0_i128);
            v
        }

        fn emergency_exit(e: Env, caller: Address, min_out_6: i128) -> i128 {
            Self::auth_admin(&e, &caller);
            let v: i128 = e.storage().instance().get(&MockKey::Value).unwrap_or(0);
            assert!(v >= min_out_6, "slippage");
            e.storage().instance().set(&MockKey::Value, &0_i128);
            v
        }

        fn value_usdc_6(e: Env) -> i128 {
            e.storage().instance().get(&MockKey::Value).unwrap_or(0)
        }

        fn sweep(e: Env, caller: Address, to: Address, amount_6: i128) {
            Self::auth(&e, &caller);
            let token_addr: Address = e.storage().instance().get(&MockKey::Token).unwrap();
            let t = token::Client::new(&e, &token_addr);
            t.transfer(&e.current_contract_address(), &to, &(amount_6 * 10));
        }
    }

    // `MockAdapterClient` is auto-generated by `#[contractimpl]` above (accumulating the
    // inherent `init`/`set_value` methods and the `StrategyAdapterInterface` methods into
    // one client type) — no separate hand-written client needed.

    // ── Harness ────────────────────────────────────────────────────────────────

    struct Harness {
        e: Env,
        admin: Address,
        operator: Address,
        manager: AllocationManagerClient<'static>,
        vault: VaultAccountingClient<'static>,
        controller: MintRedeemControllerClient<'static>,
        adapter: strategy_adapter::Client<'static>,
        adapter_id: Address,
        usdc: token::Client<'static>,
        strategy_id: BytesN<32>,
    }

    fn setup() -> Harness {
        let e = Env::default();
        e.mock_all_auths();

        let admin = Address::generate(&e);
        let operator = Address::generate(&e);
        let fee_recipient = Address::generate(&e);

        let sac = e.register_stellar_asset_contract_v2(admin.clone());
        let usdc_addr = sac.address();
        let usdc = token::Client::new(&e, &usdc_addr);
        let usdc_admin = StellarAssetClient::new(&e, &usdc_addr);

        let fusd_id = e.register_contract(None, FusdToken);
        let vault_id = e.register_contract(None, VaultAccounting);
        let controller_id = e.register_contract(None, MintRedeemController);
        let manager_id = e.register_contract(None, AllocationManager);
        let adapter_id = e.register_contract(None, MockAdapter);

        let fusd = FusdTokenClient::new(&e, &fusd_id);
        let vault = VaultAccountingClient::new(&e, &vault_id);
        let controller = MintRedeemControllerClient::new(&e, &controller_id);
        let manager = AllocationManagerClient::new(&e, &manager_id);
        let adapter = strategy_adapter::Client::new(&e, &adapter_id);
        let adapter_admin = MockAdapterClient::new(&e, &adapter_id);

        fusd.initialize(&admin, &controller_id);
        vault.initialize(&admin, &controller_id, &27, &1000);
        controller.initialize(&admin, &fusd_id, &vault_id, &usdc_addr, &0, &0, &fee_recipient);
        manager.initialize(&admin, &operator, &vault_id, &controller_id);
        adapter_admin.init(&admin, &manager_id, &usdc_addr);

        // Wire the AllocationManager as the sole allocator on every contract it
        // orchestrates — this is the real production wiring, not a test shortcut.
        vault.set_allocator(&admin, &manager_id);
        controller.set_allocator(&admin, &manager_id);

        let strategy_id = BytesN::from_array(&e, &[9u8; 32]);
        manager.register_strategy(&admin, &strategy_id, &adapter_id, &50_000_000);

        // Seed idle USDC via a real deposit.
        let user = Address::generate(&e);
        usdc_admin.mint(&user, &1_000_000_000); // 100 USDC
        controller.deposit_usdc(&user, &1_000_000_000, &0, &1);

        Harness { e, admin, operator, manager, vault, controller, adapter, adapter_id, usdc, strategy_id }
    }

    #[test]
    fn allocate_moves_real_funds_and_updates_accounting() {
        let h = setup();

        h.manager.allocate(&h.operator, &h.strategy_id, &30_000_000, &0);

        assert_eq!(h.usdc.balance(&h.adapter_id), 300_000_000, "adapter holds the real (7-dec) USDC");
        assert_eq!(h.usdc.balance(&h.controller.address), 700_000_000, "controller balance reduced");

        let gs = h.vault.global_state();
        assert_eq!(gs.total_strategy_value_6, 30_000_000);
        assert_eq!(gs.settled_idle_usdc_6, 70_000_000);

        let strat = h.vault.strategy_state(&h.strategy_id);
        assert_eq!(strat.deployed_value_6, 30_000_000);

        assert_eq!(h.adapter.value_usdc_6(), 30_000_000);
    }

    #[test]
    fn deallocate_routes_funds_back_to_controller() {
        let h = setup();
        h.manager.allocate(&h.operator, &h.strategy_id, &30_000_000, &0);

        let out = h.manager.deallocate(&h.operator, &h.strategy_id, &10_000_000, &1);
        assert_eq!(out, 10_000_000);

        assert_eq!(h.usdc.balance(&h.controller.address), 800_000_000, "funds swept back to controller");
        assert_eq!(h.usdc.balance(&h.adapter_id), 200_000_000);

        let gs = h.vault.global_state();
        assert_eq!(gs.total_strategy_value_6, 20_000_000);
        assert_eq!(gs.settled_idle_usdc_6, 80_000_000);
    }

    #[test]
    fn deallocate_all_empties_the_strategy() {
        let h = setup();
        h.manager.allocate(&h.operator, &h.strategy_id, &30_000_000, &0);

        let out = h.manager.deallocate_all(&h.operator, &h.strategy_id, &1);
        assert_eq!(out, 30_000_000);
        assert_eq!(h.adapter.value_usdc_6(), 0);

        let gs = h.vault.global_state();
        assert_eq!(gs.total_strategy_value_6, 0);
        assert_eq!(gs.settled_idle_usdc_6, 100_000_000, "fully returned to idle");
    }

    #[test]
    fn report_value_reflects_yield_without_touching_mint_allowance() {
        let h = setup();
        h.manager.allocate(&h.operator, &h.strategy_id, &30_000_000, &0);
        let allowance_before = h.vault.global_state().mint_allowance_6;

        let adapter_admin = MockAdapterClient::new(&h.e, &h.adapter_id);
        adapter_admin.set_value(&33_000_000); // simulate 10% yield

        let reported = h.manager.report_value(&h.strategy_id);
        assert_eq!(reported, 33_000_000);

        let gs = h.vault.global_state();
        assert_eq!(gs.total_strategy_value_6, 33_000_000);
        assert_eq!(gs.mint_allowance_6, allowance_before, "yield report never mints allowance");
    }

    #[test]
    fn emergency_exit_bypasses_withdraw_disabled_flag() {
        // Also the primary regression coverage for a real bug: AllocationManager used to
        // relay its own contract address to the adapter's `emergency_exit` instead of
        // the original admin `caller`. Every real adapter (blend/defindex/xycloans)
        // gates `emergency_exit` on its own separately-configured `Admin`, distinct from
        // `Allocator` (= this manager's address) — MockAdapter now mirrors that split,
        // so this call only succeeds because the fix correctly relays `h.admin` through.
        let h = setup();
        h.manager.allocate(&h.operator, &h.strategy_id, &30_000_000, &0);
        h.manager.set_strategy_flags(&h.admin, &h.strategy_id, &true, &true, &false);

        let out = h.manager.emergency_exit(&h.admin, &h.strategy_id, &1);
        assert_eq!(out, 30_000_000);
        assert_eq!(h.usdc.balance(&h.controller.address), 1_000_000_000, "all capital returned");
    }

    #[test]
    #[should_panic(expected = "not admin")]
    fn emergency_exit_rejects_non_admin_even_though_it_is_a_valid_operator() {
        // The Operator role (who can allocate/deallocate normally) must not be able to
        // trigger the governance-only emergency path just by being a legitimate signer
        // for other entry points.
        let h = setup();
        h.manager.allocate(&h.operator, &h.strategy_id, &30_000_000, &0);
        h.manager.emergency_exit(&h.operator, &h.strategy_id, &1);
    }

    #[test]
    #[should_panic(expected = "slippage")]
    fn emergency_exit_enforces_caller_supplied_min_out() {
        // Regression test: emergency_exit previously hardcoded min_out_6 = 0, so the
        // one withdrawal path meant for adverse conditions had no slippage protection
        // at all — an admin now has a real floor to require.
        let h = setup();
        h.manager.allocate(&h.operator, &h.strategy_id, &30_000_000, &0);
        h.manager.emergency_exit(&h.admin, &h.strategy_id, &30_000_001);
    }

    #[test]
    #[should_panic(expected = "strategy withdrawals disabled")]
    fn deallocate_respects_withdraw_disabled_flag() {
        let h = setup();
        h.manager.allocate(&h.operator, &h.strategy_id, &30_000_000, &0);
        h.manager.set_strategy_flags(&h.admin, &h.strategy_id, &true, &true, &false);
        h.manager.deallocate(&h.operator, &h.strategy_id, &10_000_000, &1);
    }

    #[test]
    #[should_panic(expected = "strategy not active")]
    fn allocate_respects_active_flag() {
        let h = setup();
        h.manager.set_strategy_flags(&h.admin, &h.strategy_id, &false, &true, &true);
        h.manager.allocate(&h.operator, &h.strategy_id, &10_000_000, &0);
    }

    #[test]
    #[should_panic(expected = "not operator")]
    fn only_operator_can_allocate() {
        let h = setup();
        let attacker = Address::generate(&h.e);
        h.manager.allocate(&attacker, &h.strategy_id, &10_000_000, &0);
    }

    #[test]
    #[should_panic(expected = "debt ceiling exceeded")]
    fn allocate_respects_vault_debt_ceiling() {
        let h = setup();
        // debt ceiling registered as 50_000_000; try to exceed it.
        h.manager.allocate(&h.operator, &h.strategy_id, &50_000_001, &0);
    }

    #[test]
    #[should_panic(expected = "strategy already registered")]
    fn cannot_register_same_strategy_twice() {
        let h = setup();
        h.manager.register_strategy(&h.admin, &h.strategy_id, &h.adapter_id, &1_000_000);
    }

    #[test]
    #[should_panic(expected = "not admin")]
    fn only_admin_can_register_strategy() {
        let h = setup();
        let attacker = Address::generate(&h.e);
        let sid = BytesN::from_array(&h.e, &[3u8; 32]);
        h.manager.register_strategy(&attacker, &sid, &h.adapter_id, &1_000_000);
    }
}
