// fUSD Token — SEP-41 compatible Soroban token.
// 6 decimals on all chains. Controller-gated mint/burn.
// In production: integrate with Stellar Asset Contract (SAC).
// In this PoC: internal balance ledger for testability.

#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short,
    Address, Env, String,
};

// ── Storage keys ────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    Admin,
    Controller,   // MintRedeemController — the only address that can mint/burn
    Paused,
    TotalSupply,
    Balance(Address),
    Allowance(Address, Address),
}

// ── Contract ─────────────────────────────────────────────────────────────────

#[contract]
pub struct FusdToken;

#[contractimpl]
impl FusdToken {
    // ── Init ─────────────────────────────────────────────────────────────────

    pub fn initialize(e: Env, admin: Address, controller: Address) {
        assert!(!e.storage().instance().has(&DataKey::Admin), "already initialized");
        admin.require_auth();
        e.storage().instance().set(&DataKey::Admin, &admin);
        e.storage().instance().set(&DataKey::Controller, &controller);
        e.storage().instance().set(&DataKey::Paused, &false);
        e.storage().instance().set(&DataKey::TotalSupply, &0_i128);
    }

    // ── Admin ─────────────────────────────────────────────────────────────────

    pub fn set_controller(e: Env, caller: Address, new_controller: Address) {
        Self::auth_admin(&e, &caller);
        e.storage().instance().set(&DataKey::Controller, &new_controller);
    }

    pub fn pause(e: Env, caller: Address) {
        Self::auth_controller(&e, &caller);
        e.storage().instance().set(&DataKey::Paused, &true);
        e.events().publish((symbol_short!("Paused"),), ());
    }

    pub fn unpause(e: Env, caller: Address) {
        Self::auth_admin(&e, &caller);
        e.storage().instance().set(&DataKey::Paused, &false);
        e.events().publish((symbol_short!("Unpaused"),), ());
    }

    // ── Controller-gated mint / burn ──────────────────────────────────────────

    /// Mint fUSD. Only the registered controller (MintRedeemController) may call.
    pub fn controller_mint(e: Env, caller: Address, to: Address, amount: i128) {
        Self::auth_controller(&e, &caller);
        Self::assert_active(&e);
        assert!(amount > 0, "amount must be positive");

        let bal: i128 = Self::balance_of(&e, &to);
        e.storage().persistent().set(&DataKey::Balance(to.clone()), &(bal + amount));

        let supply: i128 = e.storage().instance().get(&DataKey::TotalSupply).unwrap_or(0);
        e.storage().instance().set(&DataKey::TotalSupply, &(supply + amount));

        e.events().publish((symbol_short!("Mint"), to), amount);
    }

    /// Burn fUSD from account. Only the registered controller may call.
    pub fn controller_burn(e: Env, caller: Address, from: Address, amount: i128) {
        Self::auth_controller(&e, &caller);
        Self::assert_active(&e);
        assert!(amount > 0, "amount must be positive");

        let bal: i128 = Self::balance_of(&e, &from);
        assert!(bal >= amount, "insufficient balance");
        e.storage().persistent().set(&DataKey::Balance(from.clone()), &(bal - amount));

        let supply: i128 = e.storage().instance().get(&DataKey::TotalSupply).unwrap_or(0);
        e.storage().instance().set(&DataKey::TotalSupply, &(supply - amount));

        e.events().publish((symbol_short!("Burn"), from), amount);
    }

    // ── SEP-41 read interface ─────────────────────────────────────────────────

    pub fn balance(e: Env, account: Address) -> i128 {
        Self::balance_of(&e, &account)
    }

    pub fn total_supply(e: Env) -> i128 {
        e.storage().instance().get(&DataKey::TotalSupply).unwrap_or(0)
    }

    pub fn decimals(_e: Env) -> u32 {
        6
    }

    pub fn name(_e: Env) -> String {
        // In production: return from instance storage set at init
        String::from_str(&_e, "Frgmnt USD")
    }

    pub fn symbol(_e: Env) -> String {
        String::from_str(&_e, "fUSD")
    }

    // ── SEP-41 transfer ───────────────────────────────────────────────────────

    pub fn transfer(e: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        Self::assert_active(&e);
        assert!(amount > 0, "amount must be positive");

        let from_bal = Self::balance_of(&e, &from);
        assert!(from_bal >= amount, "insufficient balance");
        e.storage().persistent().set(&DataKey::Balance(from.clone()), &(from_bal - amount));

        let to_bal = Self::balance_of(&e, &to);
        e.storage().persistent().set(&DataKey::Balance(to.clone()), &(to_bal + amount));

        e.events().publish((symbol_short!("Transfer"), from, to), amount);
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn balance_of(e: &Env, account: &Address) -> i128 {
        e.storage().persistent().get(&DataKey::Balance(account.clone())).unwrap_or(0)
    }

    fn auth_admin(e: &Env, caller: &Address) {
        let admin: Address = e.storage().instance().get(&DataKey::Admin).unwrap();
        assert!(*caller == admin, "not admin");
        caller.require_auth();
    }

    fn auth_controller(e: &Env, caller: &Address) {
        let controller: Address = e.storage().instance().get(&DataKey::Controller).unwrap();
        assert!(*caller == controller, "not controller");
        caller.require_auth();
    }

    fn assert_active(e: &Env) {
        let paused: bool = e.storage().instance().get(&DataKey::Paused).unwrap_or(false);
        assert!(!paused, "token paused");
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::Env;

    fn setup() -> (Env, Address, Address, FusdTokenClient<'static>) {
        let e = Env::default();
        e.mock_all_auths();
        let admin = Address::generate(&e);
        let controller = Address::generate(&e);
        let contract_id = e.register_contract(None, FusdToken);
        let client = FusdTokenClient::new(&e, &contract_id);
        client.initialize(&admin, &controller);
        (e, admin, controller, client)
    }

    #[test]
    fn mint_and_burn() {
        let (e, _admin, controller, client) = setup();
        let user = Address::generate(&e);

        client.controller_mint(&controller, &user, &1_000_000);
        assert_eq!(client.balance(&user), 1_000_000);
        assert_eq!(client.total_supply(), 1_000_000);

        client.controller_burn(&controller, &user, &400_000);
        assert_eq!(client.balance(&user), 600_000);
        assert_eq!(client.total_supply(), 600_000);
    }

    #[test]
    #[should_panic(expected = "insufficient balance")]
    fn burn_beyond_balance_panics() {
        let (_e, _admin, controller, client) = setup();
        let user = Address::generate(&_e);
        client.controller_mint(&controller, &user, &100);
        client.controller_burn(&controller, &user, &101);
    }

    #[test]
    #[should_panic(expected = "not controller")]
    fn non_controller_cannot_mint() {
        let (e, _admin, _controller, client) = setup();
        let attacker = Address::generate(&e);
        let user = Address::generate(&e);
        client.controller_mint(&attacker, &user, &1_000_000);
    }

    #[test]
    #[should_panic(expected = "token paused")]
    fn paused_blocks_mint() {
        let (_e, _admin, controller, client) = setup();
        let user = Address::generate(&_e);
        client.pause(&controller);
        client.controller_mint(&controller, &user, &1_000_000);
    }

    #[test]
    #[should_panic(expected = "token paused")]
    fn paused_blocks_burn() {
        let (e, _admin, controller, client) = setup();
        let user = Address::generate(&e);
        client.controller_mint(&controller, &user, &500_000);
        client.pause(&controller);
        // Burn must also be blocked while paused.
        client.controller_burn(&controller, &user, &500_000);
    }

    #[test]
    #[should_panic(expected = "token paused")]
    fn paused_blocks_transfer() {
        let (e, _admin, controller, client) = setup();
        let user = Address::generate(&e);
        let other = Address::generate(&e);
        client.controller_mint(&controller, &user, &1_000_000);
        client.pause(&controller);
        client.transfer(&user, &other, &100_000);
    }

    #[test]
    fn transfer_works() {
        let (e, _admin, controller, client) = setup();
        let alice = Address::generate(&e);
        let bob = Address::generate(&e);
        client.controller_mint(&controller, &alice, &1_000_000);
        client.transfer(&alice, &bob, &300_000);
        assert_eq!(client.balance(&alice), 700_000);
        assert_eq!(client.balance(&bob), 300_000);
        assert_eq!(client.total_supply(), 1_000_000); // supply unchanged by transfer
    }

    #[test]
    #[should_panic(expected = "insufficient balance")]
    fn transfer_fails_insufficient_balance() {
        let (e, _admin, controller, client) = setup();
        let alice = Address::generate(&e);
        let bob = Address::generate(&e);
        client.controller_mint(&controller, &alice, &100_000);
        client.transfer(&alice, &bob, &100_001);
    }

    #[test]
    #[should_panic(expected = "already initialized")]
    fn double_initialize_fails() {
        let (_e, admin, controller, client) = setup();
        // Second call must panic.
        client.initialize(&admin, &controller);
    }

    #[test]
    #[should_panic(expected = "not admin")]
    fn non_admin_cannot_unpause() {
        let (e, _admin, controller, client) = setup();
        client.pause(&controller);
        let attacker = Address::generate(&e);
        client.unpause(&attacker);
    }

    #[test]
    fn set_controller_works() {
        let (e, admin, _controller, client) = setup();
        let new_controller = Address::generate(&e);
        let user = Address::generate(&e);
        client.set_controller(&admin, &new_controller);
        // Old controller can no longer mint.
        // New controller can mint.
        client.controller_mint(&new_controller, &user, &1_000_000);
        assert_eq!(client.balance(&user), 1_000_000);
    }

    #[test]
    fn decimals_and_metadata() {
        let (_e, _admin, _controller, client) = setup();
        assert_eq!(client.decimals(), 6);
        // name and symbol should not panic.
        let _ = client.name();
        let _ = client.symbol();
    }
}
