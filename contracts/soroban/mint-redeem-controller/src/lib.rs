// MintRedeemController — user entry point for the fUSD hub.
//
// Orchestrates USDC SAC transfers, CCTP settlement processing,
// fUSD token mint/burn, and remote mint authorization issuance.
// Delegates canonical accounting to VaultAccounting.

#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short,
    Address, BytesN, Env,
};

// ── External contract interfaces ──────────────────────────────────────────────
//
// Hand-written interface traits (rather than `contractimport!` of a prebuilt WASM
// blob) so this crate compiles standalone with plain `cargo test`/`cargo build` and
// does not depend on `fusd-token`/`vault-accounting` having already been built to
// wasm32 first. Each trait is the subset of the real contract's public interface this
// controller actually calls; the two crates' own test suites cover the rest.

mod fusd_token {
    use soroban_sdk::{contractclient, Address, Env};

    #[allow(dead_code)]
    #[contractclient(name = "Client")]
    pub trait FusdTokenInterface {
        fn controller_mint(e: Env, caller: Address, to: Address, amount: i128);
        fn controller_burn(e: Env, caller: Address, from: Address, amount: i128);
    }
}

mod vault_accounting {
    use soroban_sdk::{contractclient, Address, BytesN, Env};

    #[allow(dead_code)]
    #[contractclient(name = "Client")]
    pub trait VaultAccountingInterface {
        fn record_local_deposit(e: Env, caller: Address, amount_6: i128);
        fn record_inbound_settlement(
            e: Env,
            caller: Address,
            msg_hash: BytesN<32>,
            net_received_6: i128,
            source_domain: u32,
            finalized: bool,
        );
        fn mint_liability_from_settled_usdc(e: Env, caller: Address, amount_6: i128);
        fn burn_liability_for_redemption(e: Env, caller: Address, burned_fusd_6: i128, out_usdc_6: i128);
        fn mark_outbound_sent(e: Env, caller: Address, amount_6: i128);
        fn cancel_pending_outbound(e: Env, caller: Address, amount_6: i128);
        fn restore_failed_redeem_liab(e: Env, caller: Address, amount_6: i128);
        #[allow(clippy::too_many_arguments)]
        fn authorize_remote_mint(
            e: Env,
            caller: Address,
            mint_auth_id: BytesN<32>,
            chain_id: u32,
            amount_6: i128,
            expiry_ledger: u32,
            depositor_chain_id: u32,
            depositor_address: BytesN<32>,
        );
        fn confirm_remote_mint_executed(e: Env, caller: Address, mint_auth_id: BytesN<32>);
        fn accept_remote_burn(e: Env, caller: Address, burn_id: BytesN<32>, chain_id: u32, amount_6: i128);
        /// Read-only: the hub's currently tracked idle USDC (6 decimals). Used to bound
        /// `move_idle_to_allocator` against real accounting rather than trusting the
        /// Allocator's requested amount alone.
        fn settled_idle_usdc_6(e: Env) -> i128;
    }
}

// ── Storage keys ──────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    Admin,
    FusdToken,
    VaultAccounting,
    UsdcSac,          // Stellar USDC SAC address
    FeeVersion,       // current active fee config version
    MintFeeBps,
    RedeemFeeBps,
    FeeRecipient,
    Paused,
    Allocator,        // AllocationManager — the only address that may pull idle USDC out
    Relayer,          // admin-appointed address permitted to submit CCTP settlements
}

// ── Redeem request (two-phase state machine for remote redeems) ───────────────

#[contracttype]
#[derive(Clone, Debug)]
pub struct RedeemRequest {
    pub id: BytesN<32>,
    pub user: Address,
    pub amount_6: i128,
    pub destination_domain: u32,
    pub destination_recipient: BytesN<32>,
    pub status: RedeemStatus,
    pub created_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum RedeemStatus {
    BurnAccepted,
    BurnAcceptedSendFailed,
    Sent,
    Completed,
    RemintIssued,
}

#[contracttype]
pub enum DataKeyExt {
    RedeemRequest(BytesN<32>),
    ConsumedBurn(BytesN<32>),
}

// ── Contract ─────────────────────────────────────────────────────────────────

#[contract]
pub struct MintRedeemController;

#[contractimpl]
impl MintRedeemController {
    // ── Initialisation ────────────────────────────────────────────────────────

    pub fn initialize(
        e: Env,
        admin: Address,
        fusd_token: Address,
        vault_accounting: Address,
        usdc_sac: Address,
        mint_fee_bps: u32,
        redeem_fee_bps: u32,
        fee_recipient: Address,
    ) {
        assert!(!e.storage().instance().has(&DataKey::Admin), "already initialized");
        admin.require_auth();
        e.storage().instance().set(&DataKey::Admin, &admin);
        e.storage().instance().set(&DataKey::FusdToken, &fusd_token);
        e.storage().instance().set(&DataKey::VaultAccounting, &vault_accounting);
        e.storage().instance().set(&DataKey::UsdcSac, &usdc_sac);
        e.storage().instance().set(&DataKey::MintFeeBps, &mint_fee_bps);
        e.storage().instance().set(&DataKey::RedeemFeeBps, &redeem_fee_bps);
        e.storage().instance().set(&DataKey::FeeRecipient, &fee_recipient);
        e.storage().instance().set(&DataKey::FeeVersion, &1_u32);
        e.storage().instance().set(&DataKey::Paused, &false);
    }

    // ── Local Stellar deposit → fUSD mint ─────────────────────────────────────
    //
    // Flow:
    //   user approves USDC SAC to this contract
    //   -> deposit_usdc called
    //   -> USDC transferred from user (7 decimals)
    //   -> floored to 6 decimals; dust returned or retained
    //   -> VaultAccounting.record_local_deposit(amount_6)              [gross, full USDC held]
    //   -> VaultAccounting.mint_liability_from_settled_usdc(net_6)     [net of mint fee]
    //   -> FusdToken.controller_mint(user, amount_after_fee_6)

    pub fn deposit_usdc(
        e: Env,
        user: Address,
        amount_7: i128,    // Stellar SAC USDC (7 decimals)
        min_fusd_6: i128,  // slippage guard
        min_fee_version: u32,
    ) {
        user.require_auth();
        Self::assert_active(&e);
        assert!(amount_7 > 0, "amount must be positive");
        Self::assert_fee_version(&e, min_fee_version);

        // Transfer USDC from user.
        let usdc_sac: Address = e.storage().instance().get(&DataKey::UsdcSac).unwrap();
        Self::usdc_transfer(&e, &usdc_sac, &user, &e.current_contract_address(), amount_7);

        // Floor to 6 decimals. Return seventh-decimal dust to user.
        let amount_6 = amount_7 / 10;
        assert!(amount_6 > 0, "amount too small after decimal conversion");
        let dust_7 = amount_7 - (amount_6 * 10);
        if dust_7 > 0 {
            Self::usdc_transfer(&e, &usdc_sac, &e.current_contract_address(), &user, dust_7);
            e.events().publish((symbol_short!("DustRet"), user.clone()), dust_7);
        }

        // Apply mint fee.
        let mint_fee_bps: u32 = e.storage().instance().get(&DataKey::MintFeeBps).unwrap();
        let fee_6 = (amount_6 * mint_fee_bps as i128) / 10_000;
        let net_6 = amount_6 - fee_6;
        assert!(net_6 >= min_fusd_6, "slippage: min_fusd not met");

        // Collect fee. The fee stays in settled_idle_usdc_6 as retained protocol income —
        // it is real USDC the contract already holds (the full gross amount_6 was
        // transferred in above) but is never turned into a fUSD liability, mirroring how
        // redeem_local retains its fee (see burn_liability_for_redemption docs in
        // vault-accounting). It is not swept to fee_recipient; fee_recipient is reserved
        // for a future explicit sweep/treasury flow.
        if fee_6 > 0 {
            e.events().publish((symbol_short!("FeeCol"),), fee_6);
        }

        // Accounting: credit the FULL gross deposit as idle collateral (that is the real
        // USDC balance the contract now holds), then consume only the net-of-fee portion
        // of that allowance to create the fUSD liability. Crediting only net_6 here would
        // leave fee_6 of real, transferred-in USDC permanently untracked by the protocol's
        // own accounting.
        let vault: vault_accounting::Client = Self::vault_client(&e);
        vault.record_local_deposit(&e.current_contract_address(), &amount_6);
        vault.mint_liability_from_settled_usdc(&e.current_contract_address(), &net_6);

        let fusd: fusd_token::Client = Self::fusd_client(&e);
        fusd.controller_mint(&e.current_contract_address(), &user, &net_6);

        e.events().publish((symbol_short!("DepLoc"), user), net_6);
    }

    // ── Local Stellar burn → USDC redemption ─────────────────────────────────
    //
    // Flow:
    //   user calls redeem_local
    //   -> FusdToken.controller_burn(user, fusd_amount)
    //   -> VaultAccounting.burn_liability_for_redemption(amount_6)
    //   -> transfer USDC (7 decimals) to user
    //   -> VaultAccounting.mark_outbound_sent

    pub fn redeem_local(
        e: Env,
        user: Address,
        fusd_amount_6: i128,
        min_usdc_7: i128,
        min_fee_version: u32,
    ) {
        user.require_auth();
        Self::assert_active(&e);
        assert!(fusd_amount_6 > 0, "amount must be positive");
        Self::assert_fee_version(&e, min_fee_version);

        // Apply redeem fee.
        let redeem_fee_bps: u32 = e.storage().instance().get(&DataKey::RedeemFeeBps).unwrap();
        let fee_6 = (fusd_amount_6 * redeem_fee_bps as i128) / 10_000;
        let net_6 = fusd_amount_6 - fee_6;

        // Burn fUSD from user.
        let fusd: fusd_token::Client = Self::fusd_client(&e);
        fusd.controller_burn(&e.current_contract_address(), &user, &fusd_amount_6);

        // Reduce liability by full burned amount; only net_6 moves through pending_outbound.
        // Fee portion stays in settled_idle_usdc_6 as retained protocol income.
        let vault: vault_accounting::Client = Self::vault_client(&e);
        vault.burn_liability_for_redemption(&e.current_contract_address(), &fusd_amount_6, &net_6);

        // Send USDC (7-decimal) to user.
        let out_7 = net_6 * 10;
        assert!(out_7 >= min_usdc_7, "slippage: min_usdc_7 not met");

        let usdc_sac: Address = e.storage().instance().get(&DataKey::UsdcSac).unwrap();
        Self::usdc_transfer(&e, &usdc_sac, &e.current_contract_address(), &user, out_7);
        vault.mark_outbound_sent(&e.current_contract_address(), &net_6);

        e.events().publish((symbol_short!("RedLoc"), user), net_6);
    }

    // ── Inbound CCTP settlement → fUSD mint (or remote mint auth) ────────────
    //
    // Called by the off-chain CCTP relayer after Circle attestation.
    // The relayer does NOT supply the credited amount — it is computed from
    // balance delta (CC-CRIT-1 fix). The relayer only supplies proof materials.
    //
    // In production: integrate with Stellar CCTP MessageTransmitter contract.
    // In this PoC: simulate attestation with a mock hash for testability.

    pub fn receive_cctp_settlement(
        e: Env,
        caller: Address,         // must be the admin-appointed Relayer (see set_relayer)
        cctp_message_hash: BytesN<32>,
        source_domain: u32,
        source_sender: BytesN<32>,  // must match allowlisted remote router
        destination_chain_id: u32,  // which chain the user wants fUSD on
        // Encoded recipient for a REMOTE destination chain (destination_chain_id != 0).
        // Carried into the eventual Axelar GMP RemoteMintAuthorize payload; the hub itself
        // never decodes it, since remote-chain addresses are not representable as a
        // Stellar Address.
        destination_recipient: BytesN<32>,
        // Stellar Address to mint to when destination_chain_id == 0. Ignored for remote
        // destinations. A raw BytesN<32> cannot be turned into a callable Stellar Address
        // on-chain, so the Stellar-local case must be given a real Address directly rather
        // than decoded from destination_recipient.
        local_recipient: Address,
        finalized: bool,
        // `amount_6` is NOT taken from the caller.
        // In production the controller calls Stellar CCTP receive_message,
        // then computes: net_received_6 = balance_after - balance_before.
        // For the PoC we accept a mock_balance_delta for testability.
        mock_net_received_6: i128,
    ) {
        // Gated to a single admin-appointed Relayer rather than fully permissionless.
        // This does NOT make `mock_net_received_6` trustworthy on its own — that still
        // requires real CCTP balance-delta verification before production use (see the
        // comment above `mock_net_received_6`) — but it removes the "anyone on the
        // internet can call this and mint themselves fUSD" attack surface that existed
        // once `local_recipient` let a caller direct the mint to an address of their
        // choosing. Bound the blast radius to a single, governance-revocable key while
        // the underlying amount is still a caller-supplied mock.
        Self::auth_relayer(&e, &caller);
        Self::assert_active(&e);

        // TODO production: validate source_sender against ChainState.remote_router
        // destination_recipient: reserved for the not-yet-built Axelar GMP
        // RemoteMintAuthorize payload (see the `else` branch below) — the hub itself
        // never decodes it.
        let _ = &destination_recipient;

        // Validate destination chain is active (loaded from VaultAccounting).
        // In production: call vault.chain_state(destination_chain_id).active

        let vault: vault_accounting::Client = Self::vault_client(&e);

        // The net_received_6 here is the balance-delta from the CCTP receive call,
        // not the CCTP message amount. This prevents relayer amount injection.
        vault.record_inbound_settlement(
            &e.current_contract_address(),
            &cctp_message_hash,
            &mock_net_received_6,
            &source_domain,
            &finalized,
        );

        if !finalized {
            // Fast-credit path: fUSD minted against insurance reserve, not confirmed collateral.
            // For the PoC, skip the actual fast-credit mint and emit an event.
            e.events().publish(
                (symbol_short!("FastCrd"), destination_chain_id),
                mock_net_received_6,
            );
            return;
        }

        // Finalized: consume mint_allowance, either into an immediate liability (local
        // mint) or into a pending remote mint authorization (remote mint) — never both.
        // `authorize_remote_mint` performs its own mint_allowance_6 consumption, so
        // calling `mint_liability_from_settled_usdc` first would drain the allowance out
        // from under it and make every remote settlement revert with
        // "insufficient mint allowance".
        if destination_chain_id == 0 {
            // Destination is Stellar: consume allowance into an immediate liability and
            // mint fUSD directly to the caller-supplied Address.
            vault.mint_liability_from_settled_usdc(
                &e.current_contract_address(),
                &mock_net_received_6,
            );

            let fusd: fusd_token::Client = Self::fusd_client(&e);
            fusd.controller_mint(&e.current_contract_address(), &local_recipient, &mock_net_received_6);

            e.events().publish(
                (symbol_short!("MintLoc"), source_domain),
                mock_net_received_6,
            );
        } else {
            // Destination is a spoke: issue remote mint authorization via GMP.
            // mint_auth_id = hash("FUSD_REMOTE_MINT_AUTH_V1" || canonical_payload)
            // For PoC: derive a deterministic id from hash inputs.
            let auth_id = Self::derive_mint_auth_id(&e, &cctp_message_hash, destination_chain_id);

            // depositor_chain_id: source_domain (cross-chain deposit)
            vault.authorize_remote_mint(
                &e.current_contract_address(),
                &auth_id,
                &destination_chain_id,
                &mock_net_received_6,
                &(e.ledger().sequence() + 172_800), // ~60 day expiry
                &source_domain,
                &source_sender,
            );

            // In production: send RemoteMintAuthorize GMP via Axelar to destination spoke.
            e.events().publish(
                (symbol_short!("RemAuth"), destination_chain_id),
                mock_net_received_6,
            );
        }
    }

    // ── Accept remote burn notice (spoke burn → Stellar hub → CCTP out) ───────
    //
    // Called when an authenticated RemoteBurnNotice GMP arrives from a spoke.
    // After hub acceptance, the hub sends native USDC via CCTP to the destination.

    pub fn accept_spoke_burn(
        e: Env,
        caller: Address,
        burn_id: BytesN<32>,
        source_chain_id: u32,
        amount_6: i128,
        destination_domain: u32,
        usdc_recipient: BytesN<32>,
    ) {
        caller.require_auth();
        Self::assert_active(&e);
        assert!(amount_6 > 0);

        let vault: vault_accounting::Client = Self::vault_client(&e);

        // 1. Accept the burn — reduces chain outstanding_supply_6.
        vault.accept_remote_burn(
            &e.current_contract_address(),
            &burn_id,
            &source_chain_id,
            &amount_6,
        );

        // 2. Reduce global liability and reserve outbound USDC.
        // Cross-chain redeems carry no fee, so burned == out.
        vault.burn_liability_for_redemption(&e.current_contract_address(), &amount_6, &amount_6);

        // 3. In production: call Stellar CCTP deposit_for_burn to send USDC.
        //    Here we emit the intent event and store the redeem request.
        let redeem_id = Self::derive_redeem_id(&e, &burn_id, source_chain_id);
        let req = RedeemRequest {
            id: redeem_id.clone(),
            user: caller.clone(),
            amount_6,
            destination_domain,
            destination_recipient: usdc_recipient,
            status: RedeemStatus::BurnAccepted,
            created_ledger: e.ledger().sequence(),
        };
        e.storage().persistent().set(&DataKeyExt::RedeemRequest(redeem_id.clone()), &req);

        e.events().publish(
            (symbol_short!("BurnAcc"), source_chain_id),
            amount_6,
        );
        e.events().publish(
            (symbol_short!("CctpOut"), destination_domain),
            amount_6,
        );
    }

    // ── Receive RemoteMintExecuted ack from spoke ─────────────────────────────

    pub fn confirm_remote_mint_executed(
        e: Env,
        caller: Address,
        mint_auth_id: BytesN<32>,
    ) {
        caller.require_auth();
        let vault: vault_accounting::Client = Self::vault_client(&e);
        vault.confirm_remote_mint_executed(&e.current_contract_address(), &mint_auth_id);
        e.events().publish((symbol_short!("AckConf"),), mint_auth_id);
    }

    // ── Retry / remint recovery for failed CCTP sends ────────────────────────

    pub fn retry_redeem_cctp_send(e: Env, caller: Address, redeem_id: BytesN<32>) {
        caller.require_auth();
        let mut req: RedeemRequest = e.storage().persistent()
            .get(&DataKeyExt::RedeemRequest(redeem_id.clone()))
            .expect("redeem not found");
        assert!(
            req.status == RedeemStatus::BurnAcceptedSendFailed,
            "not in SendFailed state"
        );
        // In production: re-call Stellar CCTP deposit_for_burn.
        req.status = RedeemStatus::Sent;
        e.storage().persistent().set(&DataKeyExt::RedeemRequest(redeem_id), &req);
        e.events().publish((symbol_short!("RetryCC"),), req.amount_6);
    }

    pub fn remint_on_redeem_failure(
        e: Env,
        caller: Address,
        redeem_id: BytesN<32>,
        remint_recipient: Address,
    ) {
        caller.require_auth();
        let mut req: RedeemRequest = e.storage().persistent()
            .get(&DataKeyExt::RedeemRequest(redeem_id.clone()))
            .expect("redeem not found");
        assert!(
            req.status == RedeemStatus::BurnAcceptedSendFailed,
            "not in SendFailed state"
        );

        let vault: vault_accounting::Client = Self::vault_client(&e);
        // Step 1: Cancel the outbound that never left — returns USDC from pending back to idle.
        vault.cancel_pending_outbound(&e.current_contract_address(), &req.amount_6);
        // Step 2: Re-create the fUSD liability (idle/pending are already correct after step 1).
        vault.restore_failed_redeem_liab(&e.current_contract_address(), &req.amount_6);

        let fusd: fusd_token::Client = Self::fusd_client(&e);
        fusd.controller_mint(&e.current_contract_address(), &remint_recipient, &req.amount_6);

        req.status = RedeemStatus::RemintIssued;
        e.storage().persistent().set(&DataKeyExt::RedeemRequest(redeem_id), &req);

        e.events().publish((symbol_short!("Remint"), remint_recipient), req.amount_6);
    }

    // ── Governance ────────────────────────────────────────────────────────────

    pub fn pause(e: Env, caller: Address) {
        Self::auth_admin(&e, &caller);
        e.storage().instance().set(&DataKey::Paused, &true);
        e.events().publish((symbol_short!("Paused"),), ());
    }

    pub fn unpause(e: Env, caller: Address) {
        Self::auth_admin(&e, &caller);
        e.storage().instance().set(&DataKey::Paused, &false);
    }

    // Fee management: only Manager-submitted ManagerFeeConfig (no fee_recipient field).
    // CRIT-1 fix: fee_recipient is NOT in this call; governance uses set_fee_recipient.
    pub fn manager_set_fees(e: Env, manager: Address, mint_fee_bps: u32, redeem_fee_bps: u32) {
        manager.require_auth();
        // In production: verify manager role against RoleRegistry.
        assert!(mint_fee_bps <= 100, "mint fee exceeds governance-set max (1%)");
        assert!(redeem_fee_bps <= 100, "redeem fee exceeds governance-set max (1%)");

        e.storage().instance().set(&DataKey::MintFeeBps, &mint_fee_bps);
        e.storage().instance().set(&DataKey::RedeemFeeBps, &redeem_fee_bps);

        // Bump version so in-flight quotes become invalid.
        let v: u32 = e.storage().instance().get(&DataKey::FeeVersion).unwrap_or(1);
        e.storage().instance().set(&DataKey::FeeVersion, &(v + 1));
    }

    /// Sets the address collected fees are *earmarked* for. Note: no code path currently
    /// reads this value to actually sweep funds — fees are deliberately left mixed into
    /// `settled_idle_usdc_6` as retained protocol income (see `deposit_usdc`). This
    /// setter exists so a future explicit sweep/treasury flow has a configured
    /// destination ready; it is not itself a fee-collection mechanism yet.
    pub fn set_fee_recipient(e: Env, caller: Address, recipient: Address) {
        Self::auth_admin(&e, &caller);
        e.storage().instance().set(&DataKey::FeeRecipient, &recipient);
    }

    pub fn fee_version(e: Env) -> u32 {
        e.storage().instance().get(&DataKey::FeeVersion).unwrap_or(1)
    }

    /// The Stellar USDC SAC this hub moves. Used by AllocationManager at strategy
    /// registration time to confirm an adapter's configured asset actually matches.
    pub fn usdc_sac(e: Env) -> Address {
        e.storage().instance().get(&DataKey::UsdcSac).unwrap()
    }

    // ── Strategy allocation (AllocationManager integration) ──────────────────

    pub fn set_allocator(e: Env, caller: Address, allocator: Address) {
        Self::auth_admin(&e, &caller);
        e.storage().instance().set(&DataKey::Allocator, &allocator);
    }

    pub fn set_relayer(e: Env, caller: Address, relayer: Address) {
        Self::auth_admin(&e, &caller);
        e.storage().instance().set(&DataKey::Relayer, &relayer);
    }

    /// Send `amount_7` of the hub's idle USDC directly to `to` (a strategy adapter).
    /// Allocator-gated — this is the only way idle USDC custody leaves this contract
    /// other than a user redemption. VaultAccounting's own idle/strategy accounting is
    /// updated by a separate `move_idle_to_strategy` call the Allocator makes directly
    /// against VaultAccounting; this call only moves the real token balance.
    ///
    /// Respects the emergency pause, like every other fund-moving entry point, and is
    /// bounded against VaultAccounting's own tracked idle balance rather than trusting
    /// the Allocator's requested amount alone — defense in depth against a compromised
    /// or buggy Allocator draining more than the hub's books say is actually idle.
    pub fn move_idle_to_allocator(e: Env, caller: Address, to: Address, amount_7: i128) {
        Self::auth_allocator(&e, &caller);
        Self::assert_active(&e);
        assert!(amount_7 > 0, "amount must be positive");

        let amount_6 = amount_7 / 10;
        let vault: vault_accounting::Client = Self::vault_client(&e);
        let idle_6 = vault.settled_idle_usdc_6();
        assert!(amount_6 <= idle_6, "amount exceeds tracked idle USDC");

        let usdc_sac: Address = e.storage().instance().get(&DataKey::UsdcSac).unwrap();
        Self::usdc_transfer(&e, &usdc_sac, &e.current_contract_address(), &to, amount_7);
        e.events().publish((symbol_short!("IdleOut"), to), amount_7);
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn vault_client(e: &Env) -> vault_accounting::Client<'_> {
        let addr: Address = e.storage().instance().get(&DataKey::VaultAccounting).unwrap();
        vault_accounting::Client::new(e, &addr)
    }

    fn fusd_client(e: &Env) -> fusd_token::Client<'_> {
        let addr: Address = e.storage().instance().get(&DataKey::FusdToken).unwrap();
        fusd_token::Client::new(e, &addr)
    }

    fn usdc_transfer(e: &Env, sac: &Address, from: &Address, to: &Address, amount_7: i128) {
        soroban_sdk::token::Client::new(e, sac).transfer(from, to, &amount_7);
    }

    fn derive_mint_auth_id(e: &Env, settlement_hash: &BytesN<32>, dest_chain: u32) -> BytesN<32> {
        // Production: hash("FUSD_REMOTE_MINT_AUTH_V1" || full canonical payload).
        // PoC: xor settlement hash with chain id for determinism.
        let mut bytes = settlement_hash.to_array();
        let chain_bytes = dest_chain.to_be_bytes();
        for i in 0..4 {
            bytes[28 + i] ^= chain_bytes[i];
        }
        BytesN::from_array(e, &bytes)
    }

    fn derive_redeem_id(e: &Env, burn_id: &BytesN<32>, source_chain: u32) -> BytesN<32> {
        let mut bytes = burn_id.to_array();
        let chain_bytes = source_chain.to_be_bytes();
        for i in 0..4 {
            bytes[24 + i] ^= chain_bytes[i];
        }
        BytesN::from_array(e, &bytes)
    }

    fn assert_active(e: &Env) {
        let paused: bool = e.storage().instance().get(&DataKey::Paused).unwrap_or(false);
        assert!(!paused, "controller paused");
    }

    fn assert_fee_version(e: &Env, min_fee_version: u32) {
        let fee_version: u32 = e.storage().instance().get(&DataKey::FeeVersion).unwrap();
        assert!(fee_version == min_fee_version, "FeeVersionMismatch");
    }

    fn auth_admin(e: &Env, caller: &Address) {
        let admin: Address = e.storage().instance().get(&DataKey::Admin).unwrap();
        assert!(*caller == admin, "not admin");
        caller.require_auth();
    }

    fn auth_allocator(e: &Env, caller: &Address) {
        let allocator: Address = e.storage().instance().get(&DataKey::Allocator)
            .expect("allocator not set");
        assert!(*caller == allocator, "not allocator");
        caller.require_auth();
    }

    fn auth_relayer(e: &Env, caller: &Address) {
        let relayer: Address = e.storage().instance().get(&DataKey::Relayer)
            .expect("relayer not set");
        assert!(*caller == relayer, "not relayer");
        caller.require_auth();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod test {
    use super::*;
    use ::fusd_token::{FusdToken, FusdTokenClient};
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::token::{self, StellarAssetClient};
    use ::vault_accounting::{ChainState, VaultAccounting, VaultAccountingClient};

    struct Harness {
        e: Env,
        admin: Address,
        fee_recipient: Address,
        controller: MintRedeemControllerClient<'static>,
        vault: VaultAccountingClient<'static>,
        fusd: FusdTokenClient<'static>,
        usdc: token::Client<'static>,
        usdc_admin: StellarAssetClient<'static>,
    }

    fn setup(mint_fee_bps: u32, redeem_fee_bps: u32) -> Harness {
        let e = Env::default();
        e.mock_all_auths();

        let admin = Address::generate(&e);
        let fee_recipient = Address::generate(&e);

        let usdc_id = e.register_stellar_asset_contract_v2(admin.clone());
        let usdc_addr = usdc_id.address();
        let usdc = token::Client::new(&e, &usdc_addr);
        let usdc_admin = StellarAssetClient::new(&e, &usdc_addr);

        let fusd_id = e.register_contract(None, FusdToken);
        let vault_id = e.register_contract(None, VaultAccounting);
        let controller_id = e.register_contract(None, MintRedeemController);

        let fusd = FusdTokenClient::new(&e, &fusd_id);
        let vault = VaultAccountingClient::new(&e, &vault_id);
        let controller = MintRedeemControllerClient::new(&e, &controller_id);

        fusd.initialize(&admin, &controller_id);
        vault.initialize(&admin, &controller_id, &27, &1000); // 10% reserve
        controller.initialize(
            &admin,
            &fusd_id,
            &vault_id,
            &usdc_addr,
            &mint_fee_bps,
            &redeem_fee_bps,
            &fee_recipient,
        );

        Harness { e, admin, fee_recipient, controller, vault, fusd, usdc, usdc_admin }
    }

    fn zero_bytes32(e: &Env) -> BytesN<32> {
        BytesN::from_array(e, &[0u8; 32])
    }

    // ── deposit_usdc ───────────────────────────────────────────────────────────

    #[test]
    fn deposit_credits_gross_amount_as_idle_and_nets_fee_off_liability() {
        let h = setup(100, 100); // 1% mint fee, 1% redeem fee
        let user = Address::generate(&h.e);
        h.usdc_admin.mint(&user, &1_000_000_000); // 100 USDC (7-dec)

        h.controller.deposit_usdc(&user, &1_000_000_000, &0, &1);

        // Full gross amount left the user's wallet and is held by the controller.
        assert_eq!(h.usdc.balance(&user), 0);
        assert_eq!(h.usdc.balance(&h.controller.address), 1_000_000_000);

        let gs = h.vault.global_state();
        // Gross amount is tracked as real idle collateral...
        assert_eq!(gs.settled_idle_usdc_6, 100_000_000, "idle tracks the full real USDC balance");
        // ...but only net-of-fee became a liability.
        assert_eq!(gs.total_liabilities_6, 99_000_000, "liability excludes the mint fee");
        // The fee (1M) is real, tracked idle collateral that was never turned into a
        // liability — i.e. it is retained protocol income, not lost/unaccounted.
        assert_eq!(gs.mint_allowance_6, 1_000_000, "fee remains as unconsumed allowance backing");

        assert_eq!(h.fusd.balance(&user), 99_000_000, "user receives fUSD net of fee");
        assert_eq!(h.fusd.total_supply(), 99_000_000);
    }

    #[test]
    fn deposit_with_zero_fee_matches_1to1() {
        let h = setup(0, 0);
        let user = Address::generate(&h.e);
        h.usdc_admin.mint(&user, &500_000_000); // 50 USDC

        h.controller.deposit_usdc(&user, &500_000_000, &0, &1);

        let gs = h.vault.global_state();
        assert_eq!(gs.settled_idle_usdc_6, 50_000_000);
        assert_eq!(gs.total_liabilities_6, 50_000_000);
        assert_eq!(gs.mint_allowance_6, 0, "no fee means no leftover allowance");
        assert_eq!(h.fusd.balance(&user), 50_000_000);
    }

    #[test]
    fn deposit_returns_seventh_decimal_dust() {
        let h = setup(0, 0);
        let user = Address::generate(&h.e);
        h.usdc_admin.mint(&user, &1_000_000_007); // 100.0000007 USDC

        h.controller.deposit_usdc(&user, &1_000_000_007, &0, &1);

        assert_eq!(h.usdc.balance(&user), 7, "dust returned to depositor");
        assert_eq!(h.fusd.balance(&user), 100_000_000);
    }

    #[test]
    #[should_panic(expected = "FeeVersionMismatch")]
    fn deposit_rejects_stale_fee_version() {
        let h = setup(0, 0);
        let user = Address::generate(&h.e);
        h.usdc_admin.mint(&user, &1_000_000_000);
        h.controller.deposit_usdc(&user, &1_000_000_000, &0, &2);
    }

    #[test]
    #[should_panic(expected = "slippage: min_fusd not met")]
    fn deposit_enforces_slippage() {
        let h = setup(100, 0);
        let user = Address::generate(&h.e);
        h.usdc_admin.mint(&user, &1_000_000_000);
        h.controller.deposit_usdc(&user, &1_000_000_000, &100_000_000, &1);
    }

    // ── redeem_local ───────────────────────────────────────────────────────────

    #[test]
    fn redeem_local_round_trips_and_retains_fee_in_idle() {
        let h = setup(0, 100); // no mint fee, 1% redeem fee
        let user = Address::generate(&h.e);
        h.usdc_admin.mint(&user, &1_000_000_000);
        h.controller.deposit_usdc(&user, &1_000_000_000, &0, &1);
        assert_eq!(h.fusd.balance(&user), 100_000_000);

        h.controller.redeem_local(&user, &100_000_000, &0, &1);

        let gs = h.vault.global_state();
        assert_eq!(gs.total_liabilities_6, 0, "all liability burned");
        assert_eq!(gs.pending_outbound_usdc_6, 0, "outbound cleared after send");
        assert_eq!(gs.settled_idle_usdc_6, 1_000_000, "1% redeem fee retained in idle");

        assert_eq!(h.fusd.balance(&user), 0);
        // User received back 99% of their original USDC (100 - 1% fee).
        assert_eq!(h.usdc.balance(&user), 990_000_000);
        assert_eq!(h.usdc.balance(&h.controller.address), 10_000_000);
    }

    #[test]
    #[should_panic]
    fn redeem_local_cannot_exceed_balance() {
        let h = setup(0, 0);
        let user = Address::generate(&h.e);
        h.usdc_admin.mint(&user, &1_000_000_000);
        h.controller.deposit_usdc(&user, &1_000_000_000, &0, &1);
        h.controller.redeem_local(&user, &100_000_001, &0, &1);
    }

    // ── receive_cctp_settlement (local mint recipient fix) ───────────────────

    #[test]
    fn receive_cctp_settlement_local_mints_to_the_supplied_recipient_not_the_contract() {
        let h = setup(0, 0);
        let relayer = Address::generate(&h.e);
        let recipient = Address::generate(&h.e);
        h.controller.set_relayer(&h.admin, &relayer);

        h.controller.receive_cctp_settlement(
            &relayer,
            &zero_bytes32(&h.e),
            &6,
            &zero_bytes32(&h.e),
            &0, // Stellar-local destination
            &zero_bytes32(&h.e),
            &recipient,
            &true,
            &5_000_000,
        );

        assert_eq!(h.fusd.balance(&recipient), 5_000_000, "recipient minted to directly");
        assert_eq!(h.fusd.balance(&h.controller.address), 0, "controller must never hold the mint");
    }

    #[test]
    fn receive_cctp_settlement_remote_issues_mint_authorization() {
        let h = setup(0, 0);
        let relayer = Address::generate(&h.e);
        h.controller.set_relayer(&h.admin, &relayer);

        let chain = ChainState {
            chain_id: 6,
            axelar_chain_name: soroban_sdk::Bytes::from_array(&h.e, b"base"),
            cctp_domain: 6,
            remote_router: zero_bytes32(&h.e),
            remote_vault: zero_bytes32(&h.e),
            max_mint_6: i128::MAX,
            local_collateral_cap_6: i128::MAX,
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
        h.vault.register_chain(&h.admin, &chain);

        h.controller.receive_cctp_settlement(
            &relayer,
            &zero_bytes32(&h.e),
            &6,
            &zero_bytes32(&h.e),
            &6, // remote destination chain
            &zero_bytes32(&h.e),
            &h.admin, // ignored for the remote path
            &true,
            &3_000_000,
        );

        let chain_after = h.vault.chain_state(&6);
        assert_eq!(chain_after.pending_mint_auth_6, 3_000_000, "remote mint authorized, not yet executed");
        assert_eq!(h.fusd.total_supply(), 0, "no local fUSD minted for a remote destination");
    }

    #[test]
    #[should_panic(expected = "relayer not set")]
    fn receive_cctp_settlement_fails_before_relayer_configured() {
        let h = setup(0, 0);
        let someone = Address::generate(&h.e);
        let recipient = Address::generate(&h.e);
        h.controller.receive_cctp_settlement(
            &someone,
            &zero_bytes32(&h.e),
            &6,
            &zero_bytes32(&h.e),
            &0,
            &zero_bytes32(&h.e),
            &recipient,
            &true,
            &5_000_000,
        );
    }

    #[test]
    #[should_panic(expected = "not relayer")]
    fn receive_cctp_settlement_rejects_non_relayer_caller() {
        // Regression test: before this gate existed, `receive_cctp_settlement` was fully
        // permissionless, and combined with the local-mint-recipient fix, any caller
        // could mint themselves arbitrary fUSD by supplying their own address and a
        // fabricated mock_net_received_6. This must now be rejected for anyone other
        // than the configured Relayer.
        let h = setup(0, 0);
        let relayer = Address::generate(&h.e);
        h.controller.set_relayer(&h.admin, &relayer);

        let attacker = Address::generate(&h.e);
        h.controller.receive_cctp_settlement(
            &attacker,
            &zero_bytes32(&h.e),
            &6,
            &zero_bytes32(&h.e),
            &0,
            &zero_bytes32(&h.e),
            &attacker,
            &true,
            &1_000_000_000,
        );
    }

    // ── fee governance ─────────────────────────────────────────────────────────

    #[test]
    fn manager_set_fees_bumps_version() {
        let h = setup(50, 50);
        let manager = Address::generate(&h.e);
        assert_eq!(h.controller.fee_version(), 1);

        h.controller.manager_set_fees(&manager, &10, &20);
        assert_eq!(h.controller.fee_version(), 2, "version bumps so stale quotes revert");
        let _ = &h.fee_recipient;
    }

    #[test]
    #[should_panic(expected = "FeeVersionMismatch")]
    fn stale_fee_version_rejected_after_manager_set_fees() {
        let h = setup(50, 50);
        let manager = Address::generate(&h.e);
        h.controller.manager_set_fees(&manager, &10, &20);

        let user = Address::generate(&h.e);
        h.usdc_admin.mint(&user, &1_000_000_000);
        // Still quoting fee_version 1, which is now stale.
        h.controller.deposit_usdc(&user, &1_000_000_000, &0, &1);
    }

    #[test]
    #[should_panic(expected = "mint fee exceeds governance-set max")]
    fn manager_set_fees_rejects_excessive_mint_fee() {
        let h = setup(0, 0);
        let manager = Address::generate(&h.e);
        h.controller.manager_set_fees(&manager, &101, &0);
    }

    #[test]
    #[should_panic(expected = "not admin")]
    fn only_admin_can_set_fee_recipient() {
        let h = setup(0, 0);
        let attacker = Address::generate(&h.e);
        h.controller.set_fee_recipient(&attacker, &attacker);
    }

    // ── pause ──────────────────────────────────────────────────────────────────

    #[test]
    #[should_panic(expected = "controller paused")]
    fn paused_blocks_deposit() {
        let h = setup(0, 0);
        h.controller.pause(&h.admin);
        let user = Address::generate(&h.e);
        h.usdc_admin.mint(&user, &1_000_000_000);
        h.controller.deposit_usdc(&user, &1_000_000_000, &0, &1);
    }

    #[test]
    fn unpause_restores_deposit() {
        let h = setup(0, 0);
        h.controller.pause(&h.admin);
        h.controller.unpause(&h.admin);
        let user = Address::generate(&h.e);
        h.usdc_admin.mint(&user, &1_000_000_000);
        h.controller.deposit_usdc(&user, &1_000_000_000, &0, &1);
        assert_eq!(h.fusd.balance(&user), 100_000_000);
    }

    // ── AllocationManager integration primitive ───────────────────────────────

    #[test]
    fn move_idle_to_allocator_sends_real_usdc() {
        let h = setup(0, 0);
        let user = Address::generate(&h.e);
        h.usdc_admin.mint(&user, &1_000_000_000);
        h.controller.deposit_usdc(&user, &1_000_000_000, &0, &1);

        let allocator = Address::generate(&h.e);
        let strategy_adapter = Address::generate(&h.e);
        h.controller.set_allocator(&h.admin, &allocator);
        h.controller.move_idle_to_allocator(&allocator, &strategy_adapter, &400_000_000);

        assert_eq!(h.usdc.balance(&strategy_adapter), 400_000_000);
        assert_eq!(h.usdc.balance(&h.controller.address), 600_000_000);
    }

    #[test]
    #[should_panic(expected = "not allocator")]
    fn only_allocator_can_move_idle() {
        let h = setup(0, 0);
        let user = Address::generate(&h.e);
        h.usdc_admin.mint(&user, &1_000_000_000);
        h.controller.deposit_usdc(&user, &1_000_000_000, &0, &1);

        let allocator = Address::generate(&h.e);
        let attacker = Address::generate(&h.e);
        h.controller.set_allocator(&h.admin, &allocator);
        h.controller.move_idle_to_allocator(&attacker, &attacker, &1_000_000);
    }

    #[test]
    #[should_panic(expected = "allocator not set")]
    fn move_idle_fails_before_allocator_configured() {
        let h = setup(0, 0);
        let someone = Address::generate(&h.e);
        h.controller.move_idle_to_allocator(&someone, &someone, &1_000_000);
    }

    #[test]
    #[should_panic(expected = "controller paused")]
    fn move_idle_to_allocator_respects_pause() {
        // Regression test: this fund-egress path previously skipped the pause check
        // every other mutating entry point enforces, so an emergency pause did not
        // actually stop the Allocator from draining idle USDC.
        let h = setup(0, 0);
        let user = Address::generate(&h.e);
        h.usdc_admin.mint(&user, &1_000_000_000);
        h.controller.deposit_usdc(&user, &1_000_000_000, &0, &1);

        let allocator = Address::generate(&h.e);
        h.controller.set_allocator(&h.admin, &allocator);
        h.controller.pause(&h.admin);
        h.controller.move_idle_to_allocator(&allocator, &allocator, &400_000_000);
    }

    #[test]
    #[should_panic(expected = "amount exceeds tracked idle USDC")]
    fn move_idle_to_allocator_respects_tracked_idle_bound() {
        let h = setup(0, 0);
        let user = Address::generate(&h.e);
        h.usdc_admin.mint(&user, &1_000_000_000); // 100 USDC deposited
        h.controller.deposit_usdc(&user, &1_000_000_000, &0, &1);

        let allocator = Address::generate(&h.e);
        h.controller.set_allocator(&h.admin, &allocator);
        // Only 100 USDC is tracked as idle — asking for 110 must be rejected even
        // though the Allocator role itself is legitimate.
        h.controller.move_idle_to_allocator(&allocator, &allocator, &1_100_000_000);
    }
}
