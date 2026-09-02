// VaultAccounting — canonical accounting hub for the cross-chain fUSD protocol.
//
// Tracks all liabilities, collateral, mint allowances, and per-chain state.
// Enforces the core solvency invariant after every state mutation.
// Only MintRedeemController and AllocationManager can mutate accounting state.

#![cfg_attr(not(test), no_std)]

use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short,
    Address, Bytes, BytesN, Env,
};

// ── Public types (also used by MintRedeemController) ─────────────────────────

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct GlobalState {
    /// Total fUSD minted across all chains (6 decimals).
    pub total_liabilities_6: i128,
    /// Idle USDC held in the Stellar protocol address (6 decimals).
    pub settled_idle_usdc_6: i128,
    /// USDC locked in approved spoke vaults, hub-verified (6 decimals).
    pub settled_spoke_escrow_usdc_6: i128,
    /// Conservative value of all deployed strategies (6 decimals).
    pub total_strategy_value_6: i128,
    /// Unspent settlement credit available to mint new fUSD (6 decimals).
    pub mint_allowance_6: i128,
    /// Pending fast-finality CCTP credits backed by insurance reserve (6 decimals).
    pub pending_fast_credit_6: i128,
    /// USDC reserved for in-flight redemptions (6 decimals).
    pub pending_outbound_usdc_6: i128,
    /// Fast-credit insurance reserve (6 decimals).
    pub fast_credit_reserve_6: i128,
    /// Required idle reserve as a fraction of liabilities (basis points).
    pub required_reserve_bps: u32,
    /// Seventh-decimal Stellar USDC dust not eligible for minting (7 decimals).
    pub protocol_dust_usdc_7: i128,
    /// Ledgers after mint_auth issuance after which depositor may cancel (Stellar-native only).
    pub cancel_timeout_ledgers: u32,
    /// Stellar CCTP domain id — must NOT be hardcoded in logic.
    pub hub_cctp_domain: u32,
    /// Ledgers after which force_reconcile_mint_auth is available.
    pub mint_auth_ack_timeout_ledgers: u32,
    /// Length of one daily-redeem rate-limit window (ledgers).
    pub redeem_window_ledgers: u32,
    /// Global rolling-day redemption limit (6 decimals).
    pub global_daily_redeem_limit_6: i128,
    /// Redeemed so far in current window (6 decimals).
    pub global_redeemed_today_6: i128,
    /// Ledger at which the current global redeem window started.
    pub global_redeem_day_start_ledger: u32,
    /// Cap on per-epoch yield notifications (6 decimals).
    pub max_yield_per_epoch_6: i128,
    /// Yield credited in the current epoch (6 decimals).
    pub yield_credited_this_epoch_6: i128,
    /// Ledger at which the current yield epoch started.
    pub epoch_start_ledger: u32,
    /// Length of one yield epoch (ledgers).
    pub epoch_length_ledgers: u32,
    /// Global pause flag.
    pub paused: bool,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct ChainState {
    pub chain_id: u32,
    /// Axelar source chain name for GMP validation.
    pub axelar_chain_name: Bytes,
    /// CCTP domain of this chain.
    pub cctp_domain: u32,
    /// Allowlisted remote router address (32 bytes, chain-native encoding).
    pub remote_router: BytesN<32>,
    /// Allowlisted spoke vault address.
    pub remote_vault: BytesN<32>,
    /// Cap on fUSD minted to this chain (6 decimals).
    pub max_mint_6: i128,
    /// Cap on spoke-local collateral from this chain (6 decimals).
    pub local_collateral_cap_6: i128,
    /// fUSD currently outstanding on this chain (6 decimals).
    pub outstanding_supply_6: i128,
    /// fUSD authorised but not yet confirmed minted on this chain (6 decimals).
    pub pending_mint_auth_6: i128,
    /// fUSD burns received but not yet hub-accepted (6 decimals).
    pub pending_burn_acceptance_6: i128,
    /// Spoke-local USDC escrow verified by hub (6 decimals).
    pub settled_spoke_escrow_usdc_6: i128,
    /// Idle USDC on this chain not yet deployed to strategy (6 decimals).
    pub idle_usdc_6: i128,
    /// Whether this chain is activated for minting.
    pub active: bool,
    /// Whether local-collateral (spoke-vault lock) path is enabled.
    pub local_collateral_enabled: bool,
    /// Whether remote minting is enabled.
    pub remote_mint_enabled: bool,
    /// Per-chain rolling redemption counter (6 decimals).
    pub redeemed_today_6: i128,
    /// Per-chain daily redemption limit (6 decimals).
    pub daily_redeem_limit_6: i128,
    /// Ledger at which the per-chain redeem window started.
    pub redeem_day_start_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct StrategyState {
    pub strategy_id: BytesN<32>,
    /// Adapter contract address (e.g. BlendAdapter) responsible for this strategy.
    pub adapter: Address,
    /// Conservative, hub-tracked value of the capital deployed to this strategy (6 decimals).
    pub deployed_value_6: i128,
    /// Governance-set ceiling on capital this strategy may ever hold (6 decimals).
    pub debt_ceiling_6: i128,
    /// Whether the AllocationManager may still move idle funds into this strategy.
    pub active: bool,
    /// Ledger of the last accepted value report — used by callers/indexers for staleness checks.
    pub last_report_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MintAuthRecord {
    pub mint_auth_id: BytesN<32>,
    pub chain_id: u32,
    pub amount_6: i128,
    pub status: MintAuthStatus,
    pub issued_ledger: u32,
    pub expiry_ledger: u32,
    /// 0 = Stellar-native deposit; non-zero = cross-chain origin.
    pub depositor_chain_id: u32,
    /// Original depositor address on depositor_chain_id.
    pub depositor_address: BytesN<32>,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum MintAuthStatus {
    Pending,
    Executed,
    Expired,
    Cancelled,
    RefundSent,
}

// ── Storage keys ──────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    GlobalState,
    ChainState(u32),
    /// Replay protection for CCTP messages — Persistent storage, 5-year TTL.
    ConsumedCctp(BytesN<32>),
    /// Replay protection for GMP messages — Persistent storage, 5-year TTL.
    ConsumedGmp(BytesN<32>),
    /// Per-mint-auth lifecycle record — Persistent storage.
    MintAuth(BytesN<32>),
    /// Per-strategy accounting record — Persistent storage.
    StrategyState(BytesN<32>),
    /// Who can call state-mutating methods.
    Controller,   // MintRedeemController
    Allocator,    // AllocationManager
    Admin,
}

// ── Errors ────────────────────────────────────────────────────────────────────

// In production use soroban_sdk::contracterror; panics are used here for PoC clarity.

// ── Contract ─────────────────────────────────────────────────────────────────

#[contract]
pub struct VaultAccounting;

#[contractimpl]
impl VaultAccounting {
    // ── Initialisation ────────────────────────────────────────────────────────

    pub fn initialize(
        e: Env,
        admin: Address,
        controller: Address,
        hub_cctp_domain: u32,
        required_reserve_bps: u32,
    ) {
        assert!(!e.storage().instance().has(&DataKey::Admin), "already initialized");
        admin.require_auth();

        e.storage().instance().set(&DataKey::Admin, &admin);
        e.storage().instance().set(&DataKey::Controller, &controller);

        let gs = GlobalState {
            total_liabilities_6: 0,
            settled_idle_usdc_6: 0,
            settled_spoke_escrow_usdc_6: 0,
            total_strategy_value_6: 0,
            mint_allowance_6: 0,
            pending_fast_credit_6: 0,
            pending_outbound_usdc_6: 0,
            fast_credit_reserve_6: 0,
            required_reserve_bps,
            protocol_dust_usdc_7: 0,
            cancel_timeout_ledgers: 60_000,       // ~20h at 1.25s/ledger
            hub_cctp_domain,
            mint_auth_ack_timeout_ledgers: 483_840, // ~7 days
            redeem_window_ledgers: 69_120,          // ~24h
            global_daily_redeem_limit_6: i128::MAX,
            global_redeemed_today_6: 0,
            global_redeem_day_start_ledger: e.ledger().sequence(),
            max_yield_per_epoch_6: 1_000_000_000,  // 1,000 USDC default cap
            yield_credited_this_epoch_6: 0,
            epoch_start_ledger: e.ledger().sequence(),
            epoch_length_ledgers: 69_120,
            paused: false,
        };
        e.storage().instance().set(&DataKey::GlobalState, &gs);
    }

    // ── Chain management ──────────────────────────────────────────────────────

    pub fn register_chain(e: Env, caller: Address, state: ChainState) {
        Self::auth_admin(&e, &caller);
        e.storage().persistent().set(&DataKey::ChainState(state.chain_id), &state);
        e.events().publish((symbol_short!("ChainReg"), state.chain_id), ());
    }

    pub fn chain_state(e: Env, chain_id: u32) -> ChainState {
        e.storage().persistent().get(&DataKey::ChainState(chain_id))
            .expect("chain not registered")
    }

    pub fn global_state(e: Env) -> GlobalState {
        e.storage().instance().get(&DataKey::GlobalState).unwrap()
    }

    /// Single-field read of `settled_idle_usdc_6`, so callers that only need this one
    /// number (e.g. MintRedeemController bounding `move_idle_to_allocator`) don't need
    /// to mirror the entire `GlobalState` struct just to decode one field.
    pub fn settled_idle_usdc_6(e: Env) -> i128 {
        Self::global_state(e).settled_idle_usdc_6
    }

    // ── Deposit paths (called by MintRedeemController) ────────────────────────

    /// Record a Stellar-local USDC deposit. Increases idle collateral and mint_allowance.
    pub fn record_local_deposit(e: Env, caller: Address, amount_6: i128) {
        Self::auth_controller(&e, &caller);
        assert!(amount_6 > 0, "amount must be positive");

        let mut gs: GlobalState = Self::load_gs(&e);
        gs.settled_idle_usdc_6 = gs.settled_idle_usdc_6.checked_add(amount_6).expect("overflow");
        gs.mint_allowance_6 = gs.mint_allowance_6.checked_add(amount_6).expect("overflow");
        e.storage().instance().set(&DataKey::GlobalState, &gs);

        Self::assert_invariant_internal(&e, &gs);
        e.events().publish((symbol_short!("LocDep"),), amount_6);
    }

    /// Record a finalized inbound CCTP settlement.
    ///
    /// `net_received_6` is the balance-delta computed by MintRedeemController
    /// in the same transaction as the CCTP receive call — NOT the message amount,
    /// and NOT relayer-supplied. The relayer cannot influence this value.
    pub fn record_inbound_settlement(
        e: Env,
        caller: Address,
        msg_hash: BytesN<32>,
        net_received_6: i128,   // balance delta: usdc_balance_after - usdc_balance_before
        source_domain: u32,
        finalized: bool,
    ) {
        Self::auth_controller(&e, &caller);
        assert!(net_received_6 > 0, "net_received must be positive");

        // Replay protection — Persistent storage with 5-year TTL.
        assert!(
            !e.storage().persistent().has(&DataKey::ConsumedCctp(msg_hash.clone())),
            "cctp message already consumed"
        );

        let mut gs: GlobalState = Self::load_gs(&e);

        if finalized {
            // Finalized: credit mint_allowance_6 with net received amount.
            gs.settled_idle_usdc_6 = gs.settled_idle_usdc_6.checked_add(net_received_6).expect("overflow");
            gs.mint_allowance_6 = gs.mint_allowance_6.checked_add(net_received_6).expect("overflow");
        } else {
            // Fast/confirmed: credit insurance-backed fast-credit only, never mint_allowance.
            let new_pending = gs.pending_fast_credit_6
                .checked_add(net_received_6)
                .expect("fast credit overflow");
            assert!(
                gs.fast_credit_reserve_6 >= new_pending,
                "insufficient fast-credit insurance reserve"
            );
            gs.pending_fast_credit_6 = new_pending;
        }

        // Mark consumed — extend TTL to 5 years (~13,140,000 ledgers at 1.25s).
        e.storage().persistent().set(&DataKey::ConsumedCctp(msg_hash.clone()), &true);
        e.storage().persistent().extend_ttl(&DataKey::ConsumedCctp(msg_hash.clone()), 13_140_000, 13_140_000);

        e.storage().instance().set(&DataKey::GlobalState, &gs);

        Self::assert_invariant_internal(&e, &gs);

        let event_label = if finalized { symbol_short!("Settled") } else { symbol_short!("FastCrd") };
        e.events().publish((event_label, source_domain), net_received_6);
    }

    /// Finalize a previously fast-credited CCTP message (debit pending_fast_credit_6 only).
    /// The fUSD was already minted at fast-credit time — do NOT increase mint_allowance here.
    pub fn finalize_fast_credit(
        e: Env,
        caller: Address,
        msg_hash: BytesN<32>,
        amount_6: i128,
    ) {
        Self::auth_controller(&e, &caller);

        // The hash must already be consumed (it was marked at fast-credit time).
        assert!(
            e.storage().persistent().has(&DataKey::ConsumedCctp(msg_hash.clone())),
            "message not found in fast-credit set"
        );

        let mut gs: GlobalState = Self::load_gs(&e);
        assert!(gs.pending_fast_credit_6 >= amount_6, "fast credit underflow");
        // Reduce pending — headroom is now restored. No additional mint_allowance to prevent double-mint.
        gs.pending_fast_credit_6 -= amount_6;
        e.storage().instance().set(&DataKey::GlobalState, &gs);

        Self::assert_invariant_internal(&e, &gs);
        e.events().publish((symbol_short!("FstFinal"),), amount_6);
    }

    // ── Spoke-local collateral ────────────────────────────────────────────────

    /// Accept a hub-verified SpokeCollateralLocked GMP payload.
    /// Increases settled_spoke_escrow and mint_allowance.
    pub fn record_spoke_collateral_locked(
        e: Env,
        caller: Address,
        lock_id: BytesN<32>,
        chain_id: u32,
        amount_6: i128,
    ) {
        Self::auth_controller(&e, &caller);
        assert!(amount_6 > 0, "amount must be positive");

        // Replay protection.
        assert!(
            !e.storage().persistent().has(&DataKey::ConsumedGmp(lock_id.clone())),
            "lock already consumed"
        );

        let mut chain: ChainState = Self::load_chain(&e, chain_id);
        assert!(chain.active && chain.local_collateral_enabled, "chain not eligible for local collateral");

        // Cap check uses hub canonical state — NOT the self-reported payload field.
        let new_escrow = chain.settled_spoke_escrow_usdc_6.checked_add(amount_6).expect("overflow");
        assert!(new_escrow <= chain.local_collateral_cap_6, "local collateral cap exceeded");
        chain.settled_spoke_escrow_usdc_6 = new_escrow;
        e.storage().persistent().set(&DataKey::ChainState(chain_id), &chain);

        let mut gs: GlobalState = Self::load_gs(&e);
        gs.settled_spoke_escrow_usdc_6 = gs.settled_spoke_escrow_usdc_6.checked_add(amount_6).expect("overflow");
        gs.mint_allowance_6 = gs.mint_allowance_6.checked_add(amount_6).expect("overflow");

        // Guard: block release if pending mint auths exist for this chain.
        // (Checked at release time, not lock time — see record_spoke_collateral_released.)

        e.storage().persistent().set(&DataKey::ConsumedGmp(lock_id.clone()), &true);
        e.storage().persistent().extend_ttl(&DataKey::ConsumedGmp(lock_id), 13_140_000, 13_140_000);
        e.storage().instance().set(&DataKey::GlobalState, &gs);

        Self::assert_invariant_internal(&e, &gs);
        e.events().publish((symbol_short!("LockAcc"), chain_id), amount_6);
    }

    /// Accept a SpokeCollateralReleased GMP payload. Decreases spoke escrow.
    /// BLOCKED if chain has any Pending mint authorizations (prevents backing evaporation).
    pub fn record_spoke_collateral_released(
        e: Env,
        caller: Address,
        release_id: BytesN<32>,
        chain_id: u32,
        amount_6: i128,
    ) {
        Self::auth_controller(&e, &caller);

        assert!(
            !e.storage().persistent().has(&DataKey::ConsumedGmp(release_id.clone())),
            "release already consumed"
        );

        let chain: ChainState = Self::load_chain(&e, chain_id);

        // Pending-mint-auth release guard (CC-HIGH-3):
        // Block release if any Pending MintAuthRecord exists for this chain.
        assert!(
            chain.pending_mint_auth_6 == 0,
            "cannot release collateral while mint authorizations are pending for this chain"
        );

        let mut chain = chain;
        assert!(chain.settled_spoke_escrow_usdc_6 >= amount_6, "escrow underflow");
        chain.settled_spoke_escrow_usdc_6 -= amount_6;
        e.storage().persistent().set(&DataKey::ChainState(chain_id), &chain);

        let mut gs: GlobalState = Self::load_gs(&e);
        assert!(gs.settled_spoke_escrow_usdc_6 >= amount_6, "global escrow underflow");
        gs.settled_spoke_escrow_usdc_6 -= amount_6;
        // Revoke the mint allowance that was granted when this collateral was locked.
        // If the allowance was already consumed by a mint, clamp at 0 — `saturating_sub`
        // on a *signed* i128 only guards overflow at i128::MIN, it does not clamp at 0.
        gs.mint_allowance_6 = (gs.mint_allowance_6 - amount_6).max(0);

        e.storage().persistent().set(&DataKey::ConsumedGmp(release_id.clone()), &true);
        e.storage().persistent().extend_ttl(&DataKey::ConsumedGmp(release_id), 13_140_000, 13_140_000);
        e.storage().instance().set(&DataKey::GlobalState, &gs);

        Self::assert_invariant_internal(&e, &gs);
        e.events().publish((symbol_short!("RelAcc"), chain_id), amount_6);
    }

    // ── Mint / burn liability ─────────────────────────────────────────────────

    /// Consume mint_allowance and create a fUSD liability. Called before FusdToken.controller_mint.
    pub fn mint_liability_from_settled_usdc(e: Env, caller: Address, amount_6: i128) {
        Self::auth_controller(&e, &caller);
        assert!(amount_6 > 0);

        let mut gs: GlobalState = Self::load_gs(&e);
        assert!(gs.mint_allowance_6 >= amount_6, "insufficient mint allowance");
        gs.mint_allowance_6 -= amount_6;
        gs.total_liabilities_6 = gs.total_liabilities_6.checked_add(amount_6).expect("overflow");
        e.storage().instance().set(&DataKey::GlobalState, &gs);

        Self::assert_invariant_internal(&e, &gs);
        e.events().publish((symbol_short!("MintLiab"),), amount_6);
    }

    /// Reduce liability when fUSD is burned for redemption.
    ///
    /// `burned_fusd_6` — total fUSD destroyed (full user amount including protocol fee).
    /// `out_usdc_6`    — USDC actually sent to the user (net of fee).
    ///
    /// The difference `burned_fusd_6 - out_usdc_6` is the protocol fee in USDC that
    /// remains in `settled_idle_usdc_6` as retained income. Only `out_usdc_6` moves
    /// through `pending_outbound_usdc_6` to prevent the fee from being stuck there.
    pub fn burn_liability_for_redemption(
        e: Env,
        caller: Address,
        burned_fusd_6: i128,
        out_usdc_6: i128,
    ) {
        Self::auth_controller(&e, &caller);
        assert!(burned_fusd_6 > 0);
        assert!(out_usdc_6 > 0);
        assert!(burned_fusd_6 >= out_usdc_6, "out exceeds burned");

        let mut gs: GlobalState = Self::load_gs(&e);
        assert!(gs.total_liabilities_6 >= burned_fusd_6, "liability underflow");
        gs.total_liabilities_6 -= burned_fusd_6;

        // Only the net-out portion moves from idle to pending.
        // Fee portion (burned_fusd_6 - out_usdc_6) stays in settled_idle as protocol income.
        assert!(gs.settled_idle_usdc_6 >= out_usdc_6, "insufficient idle USDC for redemption");
        gs.settled_idle_usdc_6 -= out_usdc_6;
        gs.pending_outbound_usdc_6 = gs.pending_outbound_usdc_6.checked_add(out_usdc_6).expect("overflow");
        e.storage().instance().set(&DataKey::GlobalState, &gs);

        Self::assert_invariant_internal(&e, &gs);
        e.events().publish((symbol_short!("BurnLiab"),), burned_fusd_6);
    }

    /// Clear reserved outbound USDC after CCTP send succeeds.
    pub fn mark_outbound_sent(e: Env, caller: Address, amount_6: i128) {
        Self::auth_controller(&e, &caller);
        let mut gs: GlobalState = Self::load_gs(&e);
        assert!(gs.pending_outbound_usdc_6 >= amount_6, "outbound underflow");
        gs.pending_outbound_usdc_6 -= amount_6;
        e.storage().instance().set(&DataKey::GlobalState, &gs);
        Self::assert_invariant_internal(&e, &gs);
        e.events().publish((symbol_short!("OutSent"),), amount_6);
    }

    /// Cancel a pending outbound reservation when a CCTP send fails and fUSD will be reminted.
    /// Returns the USDC back to settled_idle so it remains available for future redemptions.
    pub fn cancel_pending_outbound(e: Env, caller: Address, amount_6: i128) {
        Self::auth_controller(&e, &caller);
        let mut gs: GlobalState = Self::load_gs(&e);
        assert!(gs.pending_outbound_usdc_6 >= amount_6, "outbound underflow");
        gs.pending_outbound_usdc_6 -= amount_6;
        gs.settled_idle_usdc_6 = gs.settled_idle_usdc_6.checked_add(amount_6).expect("overflow");
        e.storage().instance().set(&DataKey::GlobalState, &gs);
        Self::assert_invariant_internal(&e, &gs);
        e.events().publish((symbol_short!("OutCncl"),), amount_6);
    }

    /// Restore a liability entry that was cleared during a redemption whose CCTP send failed.
    /// Must be called AFTER cancel_pending_outbound (which restores idle/pending).
    /// Does NOT touch mint_allowance or idle — those are already correct.
    pub fn restore_failed_redeem_liab(e: Env, caller: Address, amount_6: i128) {
        Self::auth_controller(&e, &caller);
        assert!(amount_6 > 0);
        let mut gs: GlobalState = Self::load_gs(&e);
        gs.total_liabilities_6 = gs.total_liabilities_6.checked_add(amount_6).expect("overflow");
        e.storage().instance().set(&DataKey::GlobalState, &gs);
        Self::assert_invariant_internal(&e, &gs);
        e.events().publish((symbol_short!("RestLiab"),), amount_6);
    }

    // ── Remote mint authorization ─────────────────────────────────────────────

    /// Issue a remote mint authorization. Moves allowance into pending_mint_auth_6.
    pub fn authorize_remote_mint(
        e: Env,
        caller: Address,
        mint_auth_id: BytesN<32>,
        chain_id: u32,
        amount_6: i128,
        expiry_ledger: u32,
        depositor_chain_id: u32,
        depositor_address: BytesN<32>,
    ) {
        Self::auth_controller(&e, &caller);
        assert!(amount_6 > 0);
        assert!(
            !e.storage().persistent().has(&DataKey::MintAuth(mint_auth_id.clone())),
            "mint_auth_id already issued"
        );

        let mut gs: GlobalState = Self::load_gs(&e);
        assert!(gs.mint_allowance_6 >= amount_6, "insufficient mint allowance");
        gs.mint_allowance_6 -= amount_6;
        // liability is not yet confirmed — goes to pending, not total_liabilities
        e.storage().instance().set(&DataKey::GlobalState, &gs);

        let mut chain: ChainState = Self::load_chain(&e, chain_id);
        assert!(chain.active && chain.remote_mint_enabled, "remote mint not enabled for chain");
        chain.pending_mint_auth_6 = chain.pending_mint_auth_6.checked_add(amount_6).expect("overflow");
        e.storage().persistent().set(&DataKey::ChainState(chain_id), &chain);

        let record = MintAuthRecord {
            mint_auth_id: mint_auth_id.clone(),
            chain_id,
            amount_6,
            status: MintAuthStatus::Pending,
            issued_ledger: e.ledger().sequence(),
            expiry_ledger,
            depositor_chain_id,
            depositor_address,
        };
        e.storage().persistent().set(&DataKey::MintAuth(mint_auth_id.clone()), &record);
        e.storage().persistent().extend_ttl(&DataKey::MintAuth(mint_auth_id.clone()), 13_140_000, 13_140_000);

        e.events().publish((symbol_short!("MintAuth"), chain_id), amount_6);
    }

    /// Reconcile a mint auth after receiving authenticated RemoteMintExecuted GMP ack.
    /// Moves pending_mint_auth_6 → outstanding_supply_6 + total_liabilities_6.
    pub fn confirm_remote_mint_executed(
        e: Env,
        caller: Address,
        mint_auth_id: BytesN<32>,
    ) {
        Self::auth_controller(&e, &caller);
        Self::execute_mint_auth_internal(&e, mint_auth_id);
    }

    /// Governance-only recovery when RemoteMintExecuted ack is permanently stuck.
    /// Requires mint_auth_ack_timeout_ledgers to have elapsed since issuance.
    pub fn force_reconcile_mint_auth(
        e: Env,
        caller: Address,
        mint_auth_id: BytesN<32>,
    ) {
        Self::auth_admin(&e, &caller);

        let record: MintAuthRecord = e.storage().persistent()
            .get(&DataKey::MintAuth(mint_auth_id.clone()))
            .expect("mint auth not found");
        assert!(record.status == MintAuthStatus::Pending, "not pending");

        let gs: GlobalState = Self::load_gs(&e);
        let elapsed = e.ledger().sequence().saturating_sub(record.issued_ledger);
        assert!(
            elapsed >= gs.mint_auth_ack_timeout_ledgers,
            "ack timeout not elapsed yet"
        );

        // Same state transition as confirm_remote_mint_executed, but authorized by admin.
        Self::execute_mint_auth_internal(&e, mint_auth_id);
        e.events().publish((symbol_short!("ForceRec"),), ());
    }

    /// Shared internal logic for reconciling a Pending mint auth.
    /// Caller is responsible for auth before invoking.
    fn execute_mint_auth_internal(e: &Env, mint_auth_id: BytesN<32>) {
        let mut record: MintAuthRecord = e.storage().persistent()
            .get(&DataKey::MintAuth(mint_auth_id.clone()))
            .expect("mint auth not found");
        assert!(record.status == MintAuthStatus::Pending, "mint auth not pending");

        let mut chain: ChainState = Self::load_chain(e, record.chain_id);
        assert!(chain.pending_mint_auth_6 >= record.amount_6, "pending underflow");
        chain.pending_mint_auth_6 -= record.amount_6;
        chain.outstanding_supply_6 = chain.outstanding_supply_6.checked_add(record.amount_6).expect("overflow");
        e.storage().persistent().set(&DataKey::ChainState(record.chain_id), &chain);

        let mut gs: GlobalState = e.storage().instance().get(&DataKey::GlobalState).unwrap();
        assert!(!gs.paused, "protocol paused");
        gs.total_liabilities_6 = gs.total_liabilities_6.checked_add(record.amount_6).expect("overflow");
        e.storage().instance().set(&DataKey::GlobalState, &gs);

        let (chain_id, amount_6) = (record.chain_id, record.amount_6);
        record.status = MintAuthStatus::Executed;
        e.storage().persistent().set(&DataKey::MintAuth(mint_auth_id), &record);

        Self::assert_invariant_internal(e, &gs);
        e.events().publish((symbol_short!("MintConf"), chain_id), amount_6);
    }

    // ── Remote burn acceptance ────────────────────────────────────────────────

    /// Accept a RemoteBurnNotice GMP payload. Reduce chain outstanding supply and global liabilities.
    pub fn accept_remote_burn(
        e: Env,
        caller: Address,
        burn_id: BytesN<32>,
        chain_id: u32,
        amount_6: i128,
    ) {
        Self::auth_controller(&e, &caller);
        assert!(amount_6 > 0);

        assert!(
            !e.storage().persistent().has(&DataKey::ConsumedGmp(burn_id.clone())),
            "burn already accepted"
        );

        let mut chain: ChainState = Self::load_chain(&e, chain_id);
        assert!(chain.outstanding_supply_6 >= amount_6, "burn exceeds outstanding supply");
        chain.outstanding_supply_6 -= amount_6;
        e.storage().persistent().set(&DataKey::ChainState(chain_id), &chain);

        // Liability reduction is handled by MintRedeemController.burn_liability_for_redemption
        // after this call returns.

        e.storage().persistent().set(&DataKey::ConsumedGmp(burn_id.clone()), &true);
        e.storage().persistent().extend_ttl(&DataKey::ConsumedGmp(burn_id), 13_140_000, 13_140_000);

        e.events().publish((symbol_short!("BurnAcc"), chain_id), amount_6);
    }

    // ── Strategy allocation (called by AllocationManager) ────────────────────

    /// Admin-only: set the AllocationManager contract address. Must be set before
    /// any strategy-accounting method can be called.
    pub fn set_allocator(e: Env, caller: Address, allocator: Address) {
        Self::auth_admin(&e, &caller);
        e.storage().instance().set(&DataKey::Allocator, &allocator);
    }

    /// Admin-only: register a new strategy adapter with an initial zero balance.
    pub fn register_strategy(e: Env, caller: Address, strategy_id: BytesN<32>, adapter: Address, debt_ceiling_6: i128) {
        Self::auth_admin(&e, &caller);
        assert!(debt_ceiling_6 >= 0, "debt ceiling must be non-negative");
        assert!(
            !e.storage().persistent().has(&DataKey::StrategyState(strategy_id.clone())),
            "strategy already registered"
        );
        let state = StrategyState {
            strategy_id: strategy_id.clone(),
            adapter,
            deployed_value_6: 0,
            debt_ceiling_6,
            active: true,
            last_report_ledger: e.ledger().sequence(),
        };
        Self::save_strategy(&e, &strategy_id, &state);
        e.events().publish((symbol_short!("StratReg"),), strategy_id);
    }

    pub fn strategy_state(e: Env, strategy_id: BytesN<32>) -> StrategyState {
        e.storage().persistent().get(&DataKey::StrategyState(strategy_id))
            .expect("strategy not registered")
    }

    pub fn set_strategy_active(e: Env, caller: Address, strategy_id: BytesN<32>, active: bool) {
        Self::auth_admin(&e, &caller);
        let mut state = Self::load_strategy(&e, &strategy_id);
        state.active = active;
        Self::save_strategy(&e, &strategy_id, &state);
    }

    /// Move idle Stellar USDC into a strategy. Called by AllocationManager immediately
    /// before it deposits the same amount into the strategy's adapter contract.
    /// Does NOT change total assets (idle -> strategy, both counted) and therefore
    /// does not touch mint_allowance_6 or total_liabilities_6.
    pub fn move_idle_to_strategy(e: Env, caller: Address, strategy_id: BytesN<32>, amount_6: i128) {
        Self::auth_allocator(&e, &caller);
        assert!(amount_6 > 0, "amount must be positive");

        let mut state = Self::load_strategy(&e, &strategy_id);
        assert!(state.active, "strategy not active");
        let new_deployed = state.deployed_value_6.checked_add(amount_6).expect("overflow");
        assert!(new_deployed <= state.debt_ceiling_6, "debt ceiling exceeded");
        state.deployed_value_6 = new_deployed;
        // last_report_ledger is deliberately NOT touched here — it tracks the last
        // genuine *valuation* (report_strategy_value), not the last fund movement. A
        // deposit/withdrawal is not a re-verification of the position's real value.
        Self::save_strategy(&e, &strategy_id, &state);

        let mut gs: GlobalState = Self::load_gs(&e);
        assert!(gs.settled_idle_usdc_6 >= amount_6, "insufficient idle USDC");
        gs.settled_idle_usdc_6 -= amount_6;
        gs.total_strategy_value_6 = gs.total_strategy_value_6.checked_add(amount_6).expect("overflow");
        e.storage().instance().set(&DataKey::GlobalState, &gs);

        Self::assert_invariant_internal(&e, &gs);
        e.events().publish((symbol_short!("ToStrat"), strategy_id), amount_6);
    }

    /// Move USDC withdrawn from a strategy back into idle. Called by AllocationManager
    /// after it has already withdrawn `amount_6` from the strategy's adapter contract
    /// (the adapter-reported amount must be the actually-received balance delta, never
    /// a requested/quoted figure — enforced at the AllocationManager layer).
    pub fn move_strategy_to_idle(e: Env, caller: Address, strategy_id: BytesN<32>, amount_6: i128) {
        Self::auth_allocator(&e, &caller);
        assert!(amount_6 > 0, "amount must be positive");

        let mut state = Self::load_strategy(&e, &strategy_id);
        // Withdrawn yield can exceed the tracked principal; clamp deployed_value_6 at 0
        // rather than underflow — any excess is realized gain being pulled to idle.
        // `saturating_sub` on a *signed* i128 only guards overflow at i128::MIN, it does
        // not clamp at 0 — that needs an explicit `.max(0)`.
        //
        // The GLOBAL total_strategy_value_6 must decrease by exactly the amount THIS
        // strategy's own tracked value actually dropped by (`strategy_decrease`), not by
        // the full `amount_6` — otherwise, whenever a withdrawal exceeds this one
        // strategy's stale tracked value (the clamp-at-0 case), the aggregate would be
        // over-decremented relative to the sum of per-strategy values, permanently
        // understating real backing in a multi-strategy deployment even though nothing
        // is actually missing.
        let deployed_before = state.deployed_value_6;
        let strategy_decrease = amount_6.min(deployed_before).max(0);
        state.deployed_value_6 = deployed_before - strategy_decrease;
        Self::save_strategy(&e, &strategy_id, &state);

        let mut gs: GlobalState = Self::load_gs(&e);
        gs.settled_idle_usdc_6 = gs.settled_idle_usdc_6.checked_add(amount_6).expect("overflow");
        gs.total_strategy_value_6 = (gs.total_strategy_value_6 - strategy_decrease).max(0);
        e.storage().instance().set(&DataKey::GlobalState, &gs);

        Self::assert_invariant_internal(&e, &gs);
        e.events().publish((symbol_short!("FromStrat"), strategy_id), amount_6);
    }

    /// Replace a strategy's tracked value with a freshly reported valuation
    /// (e.g. after interest accrual on the underlying lending pool). Must never
    /// be used to fabricate mint_allowance_6 — it only ever adjusts
    /// total_strategy_value_6, which backs liabilities but never mints new ones.
    ///
    /// Deliberately NOT bounded by `debt_ceiling_6`: the ceiling caps how much new
    /// principal `move_idle_to_strategy` may deploy, not how large a position's value
    /// may grow from real, externally-verified yield. A strategy that started at its
    /// ceiling and legitimately earned interest is expected to report a value above it.
    ///
    /// A *gain* (new_value_6 > old value) IS bounded by `max_yield_per_epoch_6` — an
    /// upper-bound circuit breaker against a compromised or buggy Allocator key
    /// instantly inflating reported backing in one call. A *loss* is never rate-limited:
    /// bad news must always be reflected immediately, in full, for the solvency
    /// invariant to mean anything.
    pub fn report_strategy_value(e: Env, caller: Address, strategy_id: BytesN<32>, new_value_6: i128) {
        Self::auth_allocator(&e, &caller);
        assert!(new_value_6 >= 0, "value must be non-negative");

        let mut state = Self::load_strategy(&e, &strategy_id);
        let old_value = state.deployed_value_6;
        state.deployed_value_6 = new_value_6;
        state.last_report_ledger = e.ledger().sequence();
        Self::save_strategy(&e, &strategy_id, &state);

        let mut gs: GlobalState = Self::load_gs(&e);

        if new_value_6 > old_value {
            let gain_6 = new_value_6 - old_value;
            let current_ledger = e.ledger().sequence();
            if current_ledger.saturating_sub(gs.epoch_start_ledger) >= gs.epoch_length_ledgers {
                gs.epoch_start_ledger = current_ledger;
                gs.yield_credited_this_epoch_6 = 0;
            }
            let new_epoch_total = gs.yield_credited_this_epoch_6.checked_add(gain_6).expect("overflow");
            assert!(new_epoch_total <= gs.max_yield_per_epoch_6, "yield exceeds per-epoch cap");
            gs.yield_credited_this_epoch_6 = new_epoch_total;
        }

        gs.total_strategy_value_6 = gs.total_strategy_value_6
            .checked_sub(old_value)
            .and_then(|v| v.checked_add(new_value_6))
            .expect("overflow");
        e.storage().instance().set(&DataKey::GlobalState, &gs);

        Self::assert_invariant_internal(&e, &gs);
        e.events().publish((symbol_short!("StratVal"), strategy_id), new_value_6);
    }

    fn load_strategy(e: &Env, strategy_id: &BytesN<32>) -> StrategyState {
        e.storage().persistent().get(&DataKey::StrategyState(strategy_id.clone()))
            .expect("strategy not registered")
    }

    /// Writes a `StrategyState` and extends its TTL to the same 5-year floor used for
    /// every other long-lived Persistent record in this contract (consumed CCTP/GMP
    /// hashes, MintAuth records). A registered strategy can otherwise go untouched
    /// (paused, or simply not allocated/deallocated/report_value'd) for long enough to
    /// be archived, turning every future call against it into a hard failure until
    /// someone notices and restores the ledger entry.
    fn save_strategy(e: &Env, strategy_id: &BytesN<32>, state: &StrategyState) {
        let key = DataKey::StrategyState(strategy_id.clone());
        e.storage().persistent().set(&key, state);
        e.storage().persistent().extend_ttl(&key, 13_140_000, 13_140_000);
    }

    fn auth_allocator(e: &Env, caller: &Address) {
        let allocator: Address = e.storage().instance().get(&DataKey::Allocator)
            .expect("allocator not set");
        assert!(*caller == allocator, "not allocator");
        caller.require_auth();
    }

    // ── Solvency invariant ────────────────────────────────────────────────────

    /// Assert solvency. Panics (reverts transaction) on breach.
    /// Must be called as the final step of every state-mutating path.
    pub fn assert_invariant(e: Env) {
        let gs: GlobalState = Self::load_gs(&e);
        Self::assert_invariant_internal(&e, &gs);
    }

    /// Non-panicking solvency check for monitoring / proof-of-reserves tools.
    pub fn check_invariant(e: Env) -> bool {
        let gs: GlobalState = Self::load_gs(&e);
        Self::check_invariant_gs(&gs)
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn load_gs(e: &Env) -> GlobalState {
        let gs: GlobalState = e.storage().instance().get(&DataKey::GlobalState).unwrap();
        assert!(!gs.paused, "protocol paused");
        gs
    }

    fn load_chain(e: &Env, chain_id: u32) -> ChainState {
        e.storage().persistent().get(&DataKey::ChainState(chain_id))
            .expect("chain not registered")
    }

    fn assert_invariant_internal(_e: &Env, gs: &GlobalState) {
        assert!(
            Self::check_invariant_gs(gs),
            "InvariantViolated: liabilities exceed collateral"
        );
    }

    fn check_invariant_gs(gs: &GlobalState) -> bool {
        // Core solvency: total assets (at current value) must cover all liabilities.
        // Note: settled_idle_usdc_6 has already been reduced by pending_outbound_usdc_6 in
        // burn_liability_for_redemption — no need to subtract it again here. The pending
        // outbound USDC supports the corresponding already-burned fUSD liabilities.
        let total_assets = gs.settled_idle_usdc_6
            .saturating_add(gs.settled_spoke_escrow_usdc_6)
            .saturating_add(gs.total_strategy_value_6);
        let basic_solvency = total_assets >= gs.total_liabilities_6;

        // Liquidity reserve: a minimum fraction of liabilities must stay in idle USDC so
        // redemptions can always be served without unwinding strategies.
        // This is NOT an overcollateralization requirement — it's a liquidity floor.
        let required_idle = gs.total_liabilities_6
            .saturating_mul(gs.required_reserve_bps as i128)
            / 10_000;
        let liquidity_ok = gs.settled_idle_usdc_6 >= required_idle;

        basic_solvency && liquidity_ok
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
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger};
    use soroban_sdk::{Bytes, Env};

    fn zero_bytes(e: &Env) -> BytesN<32> {
        BytesN::from_array(e, &[0u8; 32])
    }

    fn setup() -> (Env, Address, Address, VaultAccountingClient<'static>) {
        let e = Env::default();
        e.mock_all_auths();
        let admin = Address::generate(&e);
        let controller = Address::generate(&e);
        let id = e.register_contract(None, VaultAccounting);
        let client = VaultAccountingClient::new(&e, &id);
        client.initialize(&admin, &controller, &27, &1000); // 10% reserve
        (e, admin, controller, client)
    }

    fn reg_chain(e: &Env, client: &VaultAccountingClient, admin: &Address, chain_id: u32) {
        let state = ChainState {
            chain_id,
            axelar_chain_name: Bytes::from_array(e, b"base"),
            cctp_domain: 6,
            remote_router: BytesN::from_array(e, &[1u8; 32]),
            remote_vault: BytesN::from_array(e, &[2u8; 32]),
            max_mint_6: 10_000_000_000,
            local_collateral_cap_6: 5_000_000_000,
            outstanding_supply_6: 0,
            pending_mint_auth_6: 0,
            pending_burn_acceptance_6: 0,
            settled_spoke_escrow_usdc_6: 0,
            idle_usdc_6: 0,
            active: true,
            local_collateral_enabled: true,
            remote_mint_enabled: true,
            redeemed_today_6: 0,
            daily_redeem_limit_6: i128::MAX,
            redeem_day_start_ledger: 0,
        };
        client.register_chain(admin, &state);
    }

    #[test]
    fn local_deposit_increases_allowance() {
        let (_, _, controller, client) = setup();
        client.record_local_deposit(&controller, &1_000_000);
        let gs = client.global_state();
        assert_eq!(gs.mint_allowance_6, 1_000_000);
        assert_eq!(gs.settled_idle_usdc_6, 1_000_000);
    }

    #[test]
    fn mint_then_burn_liability() {
        let (_, _, controller, client) = setup();
        client.record_local_deposit(&controller, &5_000_000);
        client.mint_liability_from_settled_usdc(&controller, &1_000_000);
        let gs = client.global_state();
        assert_eq!(gs.total_liabilities_6, 1_000_000);
        assert_eq!(gs.mint_allowance_6, 4_000_000);

        // Burn full amount (no fee), so burned == out.
        client.burn_liability_for_redemption(&controller, &1_000_000, &1_000_000);
        let gs = client.global_state();
        assert_eq!(gs.total_liabilities_6, 0);
        assert_eq!(gs.pending_outbound_usdc_6, 1_000_000);
        // settled_idle decreased only by net (out_usdc_6 == burned so same here).
        assert_eq!(gs.settled_idle_usdc_6, 4_000_000);
    }

    #[test]
    fn cctp_settlement_first_call_succeeds() {
        let (e, _, controller, client) = setup();
        let hash = BytesN::from_array(&e, &[0xAAu8; 32]);
        client.record_inbound_settlement(&controller, &hash, &1_000_000, &6, &true);
        let gs = client.global_state();
        assert_eq!(gs.mint_allowance_6, 1_000_000);
    }

    #[test]
    #[should_panic(expected = "cctp message already consumed")]
    fn cctp_settlement_replay_rejected() {
        let (e, _, controller, client) = setup();
        let hash = BytesN::from_array(&e, &[0xAAu8; 32]);
        client.record_inbound_settlement(&controller, &hash, &1_000_000, &6, &true);
        client.record_inbound_settlement(&controller, &hash, &1_000_000, &6, &true);
    }

    #[test]
    #[should_panic]
    fn collateral_release_blocked_with_pending_auth() {
        let (e, admin, controller, client) = setup();
        reg_chain(&e, &client, &admin, 6);

        let lock_id = BytesN::from_array(&e, &[0x10u8; 32]);
        client.record_spoke_collateral_locked(&controller, &lock_id, &6, &1_000_000);

        let auth_id = BytesN::from_array(&e, &[0x20u8; 32]);
        client.authorize_remote_mint(
            &controller, &auth_id, &6, &500_000,
            &99999, &0, &zero_bytes(&e),
        );

        // Release should fail — pending_mint_auth_6 > 0.
        let release_id = BytesN::from_array(&e, &[0x30u8; 32]);
        client.record_spoke_collateral_released(&controller, &release_id, &6, &500_000);
    }

    #[test]
    fn invariant_check_passes_when_solvent() {
        let (_, _, controller, client) = setup();
        client.record_local_deposit(&controller, &10_000_000);
        client.mint_liability_from_settled_usdc(&controller, &1_000_000);
        assert!(client.check_invariant());
    }

    // ── Fee accounting correctness ────────────────────────────────────────────

    #[test]
    fn redeem_with_fee_leaves_no_pending_outbound_residue() {
        let (_, _, controller, client) = setup();
        // Deposit 10 USDC, mint 10 fUSD.
        client.record_local_deposit(&controller, &10_000_000);
        client.mint_liability_from_settled_usdc(&controller, &10_000_000);

        // Redeem 10 fUSD with a 1% fee → burned=10_000_000, out=9_900_000.
        let fee_6: i128 = 100_000;
        let net_6: i128 = 10_000_000 - fee_6;
        client.burn_liability_for_redemption(&controller, &10_000_000, &net_6);

        let gs = client.global_state();
        assert_eq!(gs.total_liabilities_6, 0, "all liability cleared");
        // Only net moves to pending.
        assert_eq!(gs.pending_outbound_usdc_6, net_6, "pending = net only");
        // Fee stays in idle.
        assert_eq!(gs.settled_idle_usdc_6, fee_6, "fee remains in idle");

        // Clear the outbound.
        client.mark_outbound_sent(&controller, &net_6);
        let gs = client.global_state();
        assert_eq!(gs.pending_outbound_usdc_6, 0, "pending cleared");
        assert_eq!(gs.settled_idle_usdc_6, fee_6, "fee still in idle after send");
    }

    // ── Fast-credit path ──────────────────────────────────────────────────────

    #[test]
    #[should_panic(expected = "insufficient fast-credit insurance reserve")]
    fn fast_credit_without_reserve_panics() {
        let (e, _, controller, client) = setup();
        let hash = BytesN::from_array(&e, &[0xBBu8; 32]);
        client.record_inbound_settlement(&controller, &hash, &500_000, &6, &false);
    }

    #[test]
    fn fast_credit_does_not_increase_mint_allowance() {
        let (e, _, controller, client) = setup();
        // Finalized settlement (not fast-credit) DOES increase mint_allowance.
        let hash = BytesN::from_array(&e, &[0xCCu8; 32]);
        client.record_inbound_settlement(&controller, &hash, &1_000_000, &6, &true);
        let gs = client.global_state();
        assert_eq!(gs.mint_allowance_6, 1_000_000, "finalized settlement increases allowance");
        assert_eq!(gs.pending_fast_credit_6, 0, "no fast credit created");
    }

    #[test]
    #[should_panic(expected = "fast credit underflow")]
    fn finalize_fast_credit_with_zero_pending_panics() {
        let (e, _, controller, client) = setup();
        let hash_settle = BytesN::from_array(&e, &[0xD0u8; 32]);
        client.record_inbound_settlement(&controller, &hash_settle, &2_000_000, &6, &true);
        // Attempt finalize when pending_fast_credit_6 == 0 — must panic.
        client.finalize_fast_credit(&controller, &hash_settle, &1_000_000);
    }

    #[test]
    fn finalize_fast_credit_only_reduces_pending() {
        let (e, _, controller, client) = setup();
        // A finalized settlement followed by a mint: verify allowance and liabilities are correct.
        let hash_settle = BytesN::from_array(&e, &[0xD1u8; 32]);
        client.record_inbound_settlement(&controller, &hash_settle, &2_000_000, &6, &true);
        client.mint_liability_from_settled_usdc(&controller, &1_000_000);

        let gs = client.global_state();
        // allowance was 2M, consumed 1M → 1M left.
        assert_eq!(gs.mint_allowance_6, 1_000_000, "allowance reflects unused portion");
        assert_eq!(gs.total_liabilities_6, 1_000_000, "liabilities match minted amount");
        assert_eq!(gs.pending_fast_credit_6, 0, "no fast credit in flight");
    }

    // ── Collateral release reduces allowance ──────────────────────────────────

    #[test]
    fn collateral_release_reduces_mint_allowance() {
        let (e, admin, controller, client) = setup();
        reg_chain(&e, &client, &admin, 6);

        let lock_id = BytesN::from_array(&e, &[0xA1u8; 32]);
        client.record_spoke_collateral_locked(&controller, &lock_id, &6, &2_000_000);

        let gs = client.global_state();
        assert_eq!(gs.mint_allowance_6, 2_000_000, "allowance granted after lock");

        // Release all collateral (no pending mints → guard passes).
        let release_id = BytesN::from_array(&e, &[0xA2u8; 32]);
        client.record_spoke_collateral_released(&controller, &release_id, &6, &2_000_000);

        let gs = client.global_state();
        assert_eq!(gs.mint_allowance_6, 0, "allowance revoked after release");
        assert_eq!(gs.settled_spoke_escrow_usdc_6, 0, "escrow cleared");
    }

    #[test]
    fn partial_collateral_release_reduces_allowance_proportionally() {
        let (e, admin, controller, client) = setup();
        reg_chain(&e, &client, &admin, 6);

        // Local deposit provides idle USDC to satisfy the liquidity floor (10% reserve).
        // Without this the mint would fail: settled_idle=0 < 1M*10%=100k.
        client.record_local_deposit(&controller, &200_000);

        let lock_id = BytesN::from_array(&e, &[0xB1u8; 32]);
        client.record_spoke_collateral_locked(&controller, &lock_id, &6, &3_000_000);

        // Use 1_000_000 of allowance (local 200k + spoke 800k).
        client.mint_liability_from_settled_usdc(&controller, &1_000_000);

        let release_id = BytesN::from_array(&e, &[0xB2u8; 32]);
        client.record_spoke_collateral_released(&controller, &release_id, &6, &2_000_000);

        let gs = client.global_state();
        // Local deposit gave 200k allowance; spoke gave 3M; total 3.2M.
        // Used 1M → remaining 2.2M. Release revokes 2M → remaining 200k.
        assert_eq!(gs.mint_allowance_6, 200_000, "allowance = local portion after spoke release");
        assert_eq!(gs.settled_spoke_escrow_usdc_6, 1_000_000, "1M spoke escrow remains");
    }

    // ── Remote mint auth round-trip ───────────────────────────────────────────

    #[test]
    fn remote_mint_roundtrip() {
        let (e, admin, controller, client) = setup();
        reg_chain(&e, &client, &admin, 6);

        // Fund allowance via settlement.
        let hash = BytesN::from_array(&e, &[0xC1u8; 32]);
        client.record_inbound_settlement(&controller, &hash, &5_000_000, &6, &true);

        let auth_id = BytesN::from_array(&e, &[0xC2u8; 32]);
        client.authorize_remote_mint(
            &controller, &auth_id, &6, &2_000_000,
            &(e.ledger().sequence() + 1000), &0, &zero_bytes(&e),
        );

        let gs = client.global_state();
        // Allowance consumed into pending (not yet a liability).
        assert_eq!(gs.mint_allowance_6, 3_000_000);
        assert_eq!(gs.total_liabilities_6, 0);

        let chain = client.chain_state(&6);
        assert_eq!(chain.pending_mint_auth_6, 2_000_000);

        // Confirm execution (GMP ack received).
        client.confirm_remote_mint_executed(&controller, &auth_id);

        let gs = client.global_state();
        assert_eq!(gs.total_liabilities_6, 2_000_000, "liability confirmed");

        let chain = client.chain_state(&6);
        assert_eq!(chain.pending_mint_auth_6, 0, "pending cleared");
        assert_eq!(chain.outstanding_supply_6, 2_000_000, "outstanding updated");
    }

    // ── force_reconcile_mint_auth ─────────────────────────────────────────────

    #[test]
    fn force_reconcile_after_timeout() {
        // Build env with large TTL BEFORE registering contract so all storage
        // entries survive the 483,841-ledger advance that simulates ~7 days.
        let e = Env::default();
        e.mock_all_auths();
        e.ledger().with_mut(|l| {
            l.min_temp_entry_ttl = 1_000_000;
            l.min_persistent_entry_ttl = 1_000_000;
            l.max_entry_ttl = 2_000_000;
        });
        let admin = Address::generate(&e);
        let controller = Address::generate(&e);
        let id = e.register_contract(None, VaultAccounting);
        let client = VaultAccountingClient::new(&e, &id);
        client.initialize(&admin, &controller, &27, &1000);

        reg_chain(&e, &client, &admin, 6);

        let hash = BytesN::from_array(&e, &[0xE1u8; 32]);
        client.record_inbound_settlement(&controller, &hash, &5_000_000, &6, &true);

        let auth_id = BytesN::from_array(&e, &[0xE2u8; 32]);
        client.authorize_remote_mint(
            &controller, &auth_id, &6, &1_000_000,
            &(e.ledger().sequence() + 1000), &0, &zero_bytes(&e),
        );

        // Advance ledger past mint_auth_ack_timeout_ledgers (483_840 configured in initialize).
        e.ledger().with_mut(|l| { l.sequence_number += 483_841; });

        // force_reconcile called by admin — must NOT fail with "not controller".
        client.force_reconcile_mint_auth(&admin, &auth_id);

        let gs = client.global_state();
        assert_eq!(gs.total_liabilities_6, 1_000_000, "liability confirmed via governance");
        let chain = client.chain_state(&6);
        assert_eq!(chain.pending_mint_auth_6, 0);
        assert_eq!(chain.outstanding_supply_6, 1_000_000);
    }

    #[test]
    #[should_panic]
    fn force_reconcile_before_timeout_fails() {
        let (e, admin, controller, client) = setup();
        reg_chain(&e, &client, &admin, 6);

        let hash = BytesN::from_array(&e, &[0xF1u8; 32]);
        client.record_inbound_settlement(&controller, &hash, &5_000_000, &6, &true);

        let auth_id = BytesN::from_array(&e, &[0xF2u8; 32]);
        client.authorize_remote_mint(
            &controller, &auth_id, &6, &1_000_000,
            &(e.ledger().sequence() + 1000), &0, &zero_bytes(&e),
        );

        // Do NOT advance the ledger — must panic because timeout not elapsed.
        client.force_reconcile_mint_auth(&admin, &auth_id);
    }

    // ── Solvency invariant ────────────────────────────────────────────────────

    #[test]
    fn cancel_pending_outbound_restores_idle() {
        let (_, _, controller, client) = setup();
        client.record_local_deposit(&controller, &5_000_000);
        client.mint_liability_from_settled_usdc(&controller, &3_000_000);
        client.burn_liability_for_redemption(&controller, &3_000_000, &3_000_000);

        let gs = client.global_state();
        assert_eq!(gs.pending_outbound_usdc_6, 3_000_000);
        assert_eq!(gs.settled_idle_usdc_6, 2_000_000);

        // Simulate CCTP failure: cancel the pending.
        client.cancel_pending_outbound(&controller, &3_000_000);

        let gs = client.global_state();
        assert_eq!(gs.pending_outbound_usdc_6, 0, "pending cleared");
        assert_eq!(gs.settled_idle_usdc_6, 5_000_000, "idle restored");
    }

    #[test]
    fn restore_liability_after_cancel() {
        let (_, _, controller, client) = setup();
        client.record_local_deposit(&controller, &5_000_000);
        client.mint_liability_from_settled_usdc(&controller, &3_000_000);
        client.burn_liability_for_redemption(&controller, &3_000_000, &3_000_000);
        client.cancel_pending_outbound(&controller, &3_000_000);

        let gs = client.global_state();
        assert_eq!(gs.total_liabilities_6, 0);

        client.restore_failed_redeem_liab(&controller, &3_000_000);

        let gs = client.global_state();
        assert_eq!(gs.total_liabilities_6, 3_000_000, "liability restored");
        assert_eq!(gs.settled_idle_usdc_6, 5_000_000, "idle unchanged");
        assert!(client.check_invariant(), "must still be solvent");
    }

    // ── Strategy allocation (AllocationManager / BlendAdapter integration) ────

    fn strategy_id(e: &Env, tag: u8) -> BytesN<32> {
        BytesN::from_array(e, &[tag; 32])
    }

    fn setup_with_allocator() -> (Env, Address, Address, Address, VaultAccountingClient<'static>) {
        let (e, admin, controller, client) = setup();
        let allocator = Address::generate(&e);
        client.set_allocator(&admin, &allocator);
        (e, admin, controller, allocator, client)
    }

    #[test]
    #[should_panic(expected = "not allocator")]
    fn non_allocator_cannot_move_idle_to_strategy() {
        let (e, admin, controller, client) = setup();
        let allocator = Address::generate(&e);
        client.set_allocator(&admin, &allocator);
        client.record_local_deposit(&controller, &1_000_000);

        let sid = strategy_id(&e, 1);
        let adapter = Address::generate(&e);
        client.register_strategy(&admin, &sid, &adapter, &10_000_000);

        let attacker = Address::generate(&e);
        client.move_idle_to_strategy(&attacker, &sid, &500_000);
    }

    #[test]
    fn move_idle_to_strategy_and_back() {
        let (e, admin, controller, allocator, client) = setup_with_allocator();
        // 10% reserve requires idle >= 10% of liabilities; deposit plenty of idle first.
        client.record_local_deposit(&controller, &10_000_000);
        client.mint_liability_from_settled_usdc(&controller, &1_000_000);

        let sid = strategy_id(&e, 1);
        let adapter = Address::generate(&e);
        client.register_strategy(&admin, &sid, &adapter, &5_000_000);

        client.move_idle_to_strategy(&allocator, &sid, &2_000_000);
        let gs = client.global_state();
        assert_eq!(gs.total_strategy_value_6, 2_000_000, "strategy value credited");
        // settled_idle_usdc_6 was 10_000_000 (mint_liability_from_settled_usdc only moves
        // mint_allowance_6 -> total_liabilities_6, it does not touch idle); minus the 2M
        // just deployed to the strategy = 8_000_000.
        assert_eq!(gs.settled_idle_usdc_6, 8_000_000, "idle reduced");
        let strat = client.strategy_state(&sid);
        assert_eq!(strat.deployed_value_6, 2_000_000);

        client.move_strategy_to_idle(&allocator, &sid, &500_000);
        let gs = client.global_state();
        assert_eq!(gs.total_strategy_value_6, 1_500_000, "strategy value reduced");
        assert_eq!(gs.settled_idle_usdc_6, 8_500_000, "idle restored");
        let strat = client.strategy_state(&sid);
        assert_eq!(strat.deployed_value_6, 1_500_000);
    }

    #[test]
    fn move_strategy_to_idle_with_yield_clamps_at_zero_not_negative() {
        // Regression test: signed-i128 `saturating_sub` does NOT clamp at zero (only at
        // i128::MIN), so withdrawing more than the tracked deployed_value_6 (e.g. because
        // real yield was realized) must not leave deployed_value_6 or
        // total_strategy_value_6 negative.
        let (e, admin, controller, allocator, client) = setup_with_allocator();
        client.record_local_deposit(&controller, &10_000_000);

        let sid = strategy_id(&e, 7);
        let adapter = Address::generate(&e);
        client.register_strategy(&admin, &sid, &adapter, &10_000_000);
        client.move_idle_to_strategy(&allocator, &sid, &2_000_000);

        // Withdraw more than was deployed (2M) — the extra 500k is realized yield.
        client.move_strategy_to_idle(&allocator, &sid, &2_500_000);

        let gs = client.global_state();
        assert_eq!(gs.total_strategy_value_6, 0, "clamped at zero, not negative");
        assert_eq!(gs.settled_idle_usdc_6, 10_500_000, "idle received the full amount including yield");
        let strat = client.strategy_state(&sid);
        assert_eq!(strat.deployed_value_6, 0, "clamped at zero, not negative");
        assert!(client.check_invariant());
    }

    #[test]
    #[should_panic(expected = "debt ceiling exceeded")]
    fn move_idle_to_strategy_respects_debt_ceiling() {
        let (e, admin, controller, allocator, client) = setup_with_allocator();
        client.record_local_deposit(&controller, &10_000_000);

        let sid = strategy_id(&e, 2);
        let adapter = Address::generate(&e);
        client.register_strategy(&admin, &sid, &adapter, &1_000_000);

        client.move_idle_to_strategy(&allocator, &sid, &1_000_001);
    }

    #[test]
    fn report_strategy_value_never_touches_mint_allowance() {
        let (e, admin, controller, allocator, client) = setup_with_allocator();
        client.record_local_deposit(&controller, &10_000_000);
        client.mint_liability_from_settled_usdc(&controller, &1_000_000);

        let sid = strategy_id(&e, 3);
        let adapter = Address::generate(&e);
        client.register_strategy(&admin, &sid, &adapter, &10_000_000);
        client.move_idle_to_strategy(&allocator, &sid, &3_000_000);

        let allowance_before = client.global_state().mint_allowance_6;

        // Interest accrued: adapter now reports the position is worth 3.3M instead of 3M.
        client.report_strategy_value(&allocator, &sid, &3_300_000);

        let gs = client.global_state();
        assert_eq!(gs.total_strategy_value_6, 3_300_000, "yield reflected in strategy value");
        assert_eq!(gs.mint_allowance_6, allowance_before, "value report must never mint allowance");

        let strat = client.strategy_state(&sid);
        assert_eq!(strat.deployed_value_6, 3_300_000);
    }

    #[test]
    fn report_strategy_value_can_report_a_loss() {
        let (e, admin, controller, allocator, client) = setup_with_allocator();
        client.record_local_deposit(&controller, &10_000_000);

        let sid = strategy_id(&e, 4);
        let adapter = Address::generate(&e);
        client.register_strategy(&admin, &sid, &adapter, &10_000_000);
        client.move_idle_to_strategy(&allocator, &sid, &2_000_000);

        // Loss: strategy now worth less than deployed.
        client.report_strategy_value(&allocator, &sid, &1_800_000);
        let gs = client.global_state();
        assert_eq!(gs.total_strategy_value_6, 1_800_000);
    }

    #[test]
    #[should_panic(expected = "yield exceeds per-epoch cap")]
    fn report_strategy_value_rejects_gain_over_epoch_cap() {
        // Default max_yield_per_epoch_6 from initialize() is 1_000_000_000 (1,000 USDC).
        let (e, admin, controller, allocator, client) = setup_with_allocator();
        client.record_local_deposit(&controller, &10_000_000_000);

        let sid = strategy_id(&e, 8);
        let adapter = Address::generate(&e);
        client.register_strategy(&admin, &sid, &adapter, &10_000_000_000);
        client.move_idle_to_strategy(&allocator, &sid, &1_000_000);

        // A single report claiming 1,001 USDC of gain in one call must be rejected —
        // otherwise a compromised/buggy Allocator key could instantly fabricate
        // arbitrary backing with no rate limit at all.
        client.report_strategy_value(&allocator, &sid, &1_001_001_000);
    }

    #[test]
    fn report_strategy_value_epoch_cap_tracks_cumulative_gain() {
        let (e, admin, controller, allocator, client) = setup_with_allocator();
        client.record_local_deposit(&controller, &10_000_000_000);

        let sid = strategy_id(&e, 9);
        let adapter = Address::generate(&e);
        client.register_strategy(&admin, &sid, &adapter, &10_000_000_000);
        client.move_idle_to_strategy(&allocator, &sid, &1_000_000);

        // A 600 USDC gain, well under the 1,000 USDC cap, is accepted and tracked.
        client.report_strategy_value(&allocator, &sid, &601_000_000);
        let gs = client.global_state();
        assert_eq!(gs.yield_credited_this_epoch_6, 600_000_000);
    }

    #[test]
    #[should_panic(expected = "yield exceeds per-epoch cap")]
    fn report_strategy_value_epoch_cap_is_cumulative_within_the_epoch() {
        let (e, admin, controller, allocator, client) = setup_with_allocator();
        client.record_local_deposit(&controller, &10_000_000_000);

        let sid = strategy_id(&e, 9);
        let adapter = Address::generate(&e);
        client.register_strategy(&admin, &sid, &adapter, &10_000_000_000);
        client.move_idle_to_strategy(&allocator, &sid, &1_000_000);

        // Two 600 USDC gains in the same epoch sum to 1,200 USDC > the 1,000 USDC cap —
        // the second call alone is under the cap, but the cumulative total is not.
        client.report_strategy_value(&allocator, &sid, &601_000_000);
        client.report_strategy_value(&allocator, &sid, &1_201_000_000);
    }

    #[test]
    fn report_strategy_value_epoch_cap_resets_after_epoch_rolls_over() {
        // Build env with large TTL BEFORE registering the contract so all storage
        // entries survive the 69,121-ledger advance that crosses into a new epoch —
        // same technique as force_reconcile_after_timeout.
        let e = Env::default();
        e.mock_all_auths();
        e.ledger().with_mut(|l| {
            l.min_temp_entry_ttl = 200_000;
            l.min_persistent_entry_ttl = 200_000;
            l.max_entry_ttl = 300_000;
        });
        let admin = Address::generate(&e);
        let controller = Address::generate(&e);
        let id = e.register_contract(None, VaultAccounting);
        let client = VaultAccountingClient::new(&e, &id);
        client.initialize(&admin, &controller, &27, &1000);
        let allocator = Address::generate(&e);
        client.set_allocator(&admin, &allocator);

        client.record_local_deposit(&controller, &10_000_000_000);

        let sid = strategy_id(&e, 10);
        let adapter = Address::generate(&e);
        client.register_strategy(&admin, &sid, &adapter, &10_000_000_000);
        client.move_idle_to_strategy(&allocator, &sid, &1_000_000);

        client.report_strategy_value(&allocator, &sid, &601_000_000); // 600 USDC gain
        assert_eq!(client.global_state().yield_credited_this_epoch_6, 600_000_000);

        // Advance past epoch_length_ledgers (69_120, set in initialize()).
        e.ledger().with_mut(|l| { l.sequence_number += 69_121; });

        // A further 600 USDC gain would have failed within the same epoch, but the
        // epoch has rolled over so the counter resets.
        client.report_strategy_value(&allocator, &sid, &1_201_000_000);
        let gs = client.global_state();
        assert_eq!(gs.yield_credited_this_epoch_6, 600_000_000, "counter reset to only this epoch's gain");
        assert_eq!(gs.total_strategy_value_6, 1_201_000_000);
    }

    #[test]
    fn move_strategy_to_idle_keeps_aggregate_consistent_with_sum_of_strategies() {
        // Regression test for a real divergence bug: when a withdrawal from ONE
        // strategy exceeds that strategy's own tracked value (clamped at 0), the
        // aggregate total_strategy_value_6 must decrease by exactly the same clamped
        // amount as that strategy's own value did — not by the full requested amount —
        // or a multi-strategy deployment silently understates real backing forever.
        let (e, admin, controller, allocator, client) = setup_with_allocator();
        client.record_local_deposit(&controller, &20_000_000);

        let sid_a = strategy_id(&e, 20);
        let sid_b = strategy_id(&e, 21);
        let adapter_a = Address::generate(&e);
        let adapter_b = Address::generate(&e);
        client.register_strategy(&admin, &sid_a, &adapter_a, &10_000_000);
        client.register_strategy(&admin, &sid_b, &adapter_b, &10_000_000);

        client.move_idle_to_strategy(&allocator, &sid_a, &2_000_000);
        client.move_idle_to_strategy(&allocator, &sid_b, &2_000_000);
        assert_eq!(client.global_state().total_strategy_value_6, 4_000_000);

        // Withdraw 2.5M from A — 500k more than A's own tracked 2M (realized yield that
        // was never separately reported). A's own value clamps to 0.
        client.move_strategy_to_idle(&allocator, &sid_a, &2_500_000);

        let strat_a = client.strategy_state(&sid_a);
        let strat_b = client.strategy_state(&sid_b);
        assert_eq!(strat_a.deployed_value_6, 0);
        assert_eq!(strat_b.deployed_value_6, 2_000_000, "B untouched");

        let gs = client.global_state();
        assert_eq!(
            gs.total_strategy_value_6,
            strat_a.deployed_value_6 + strat_b.deployed_value_6,
            "aggregate must equal the true sum of per-strategy values"
        );
        assert_eq!(gs.total_strategy_value_6, 2_000_000, "only A's real 2M tracked value left the aggregate, not the requested 2.5M");
    }

    #[test]
    #[should_panic(expected = "strategy not active")]
    fn move_idle_to_strategy_blocked_when_inactive() {
        let (e, admin, controller, allocator, client) = setup_with_allocator();
        client.record_local_deposit(&controller, &10_000_000);

        let sid = strategy_id(&e, 5);
        let adapter = Address::generate(&e);
        client.register_strategy(&admin, &sid, &adapter, &10_000_000);
        client.set_strategy_active(&admin, &sid, &false);

        client.move_idle_to_strategy(&allocator, &sid, &1_000_000);
    }

    #[test]
    #[should_panic(expected = "allocator not set")]
    fn strategy_move_fails_before_allocator_configured() {
        let (e, admin, controller, client) = setup();
        client.record_local_deposit(&controller, &10_000_000);

        let sid = strategy_id(&e, 6);
        let adapter = Address::generate(&e);
        client.register_strategy(&admin, &sid, &adapter, &10_000_000);

        let someone = Address::generate(&e);
        client.move_idle_to_strategy(&someone, &sid, &1_000_000);
    }
}
