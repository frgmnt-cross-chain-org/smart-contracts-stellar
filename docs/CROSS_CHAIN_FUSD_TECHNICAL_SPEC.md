# Cross-Chain Frgmnt fUSD Technical Specification

Version: 0.6  
Date: 2026-06-14  
Target stack: Stellar Soroban, Circle CCTP v2, Axelar GMP, custom EVM remote routers, Solana programs, cross-chain dApp/backend

Canonical status: this is the single technical architecture document for the cross-chain Stellar hub version of Frgmnt fUSD. The earlier full-stack overview and Stellar architecture study have been merged into this specification to avoid duplicate sources of truth.

## 1. Executive Technical Summary

Cross-chain Frgmnt fUSD is a native-USDC-backed stablecoin and yield protocol with Stellar as the canonical accounting and governance layer.

Users deposit native USDC on Stellar or any supported spoke chain and can receive fUSD on any supported destination chain. fUSD is minted 1:1 against settled USDC collateral, but the mint can execute on Stellar, an EVM spoke, or a Solana spoke after Stellar hub authorization. USDC can then be allocated into approved yield venues on any supported investment chain, including same-chain lending and DEX/LP venues guarded by chain-local contract/instruction guards and asset guards. Cross-chain value movement uses Circle CCTP v2 for native USDC burn/mint. Cross-chain instructions use Axelar GMP for governance, acknowledgements, allocation signals, and remote router coordination. Version 0.4 incorporated 24 additional findings from the second independent senior audit cycle. Version 0.5 incorporated 12 additional findings from the third independent strict audit cycle covering fee-recipient authority separation, cross-chain depositor identity verification, governance role-delay enforcement, CCTP amount contradiction, allocation-id replay protection, route expiry enforcement, epoch yield accumulation, daily-limit rollover semantics, rollback refund routing, and monitoring guidance. Version 0.6 incorporates 6 cross-chain design verification findings: relayer-injectable CCTP amount removed from settlement interface, route_version binding added to all allocation execution methods, redeem_local pseudocode corrected to match trait, spoke collateral release guard against pending mint auths, fast-credit finalization path specified, and stuck RemoteMintExecuted ack recovery path added.

The core rule is simple:

```text
fUSD may be minted only against verified, settled native USDC.
GMP messages can request or attest workflow state, but they cannot create collateral by themselves.
Strategy value can back existing liabilities after conservative valuation, but it must not create new mint capacity by itself.
```

The architecture intentionally separates:

- value transport: CCTP v2,
- message transport: Axelar GMP,
- canonical accounting: Soroban contracts on Stellar,
- remote execution: thin routers/adapters on each connected chain,
- yield decisions: risk-approved allocation policy and strategy adapters.

## 1A. Product Thesis And Technical Assumptions

Stablecoin holders usually receive no reserve yield. fUSD should let users deposit native USDC on any supported chain, mint fUSD at par on any activated destination chain, and access sustainable yield sourced from risk-managed lending and liquidity allocations.

Stellar is the hub for:

- canonical fUSD supply accounting,
- collateral and liabilities accounting,
- allocation governance,
- cross-chain mint/burn authorization,
- yield aggregation and reporting,
- protocol fee and strategy-whitelist policy.

EVM chains and Solana are spokes. They execute local deposits, burns, mints, and strategies only after hub authorization. A spoke can hold native USDC in an approved vault for same-chain investment, but that local custody becomes canonical backing only after the hub accepts the canonical spoke-vault lock payload.

The protocol should avoid:

- wrapped collateral,
- third-party custodial bridges as canonical collateral,
- undercollateralized or algorithmic minting,
- opaque off-chain NAV accounting,
- backend/indexer-controlled supply transitions.

Verified ecosystem constraints to preserve during implementation:

- SEP-41 defines the Soroban token interface used by `FusdToken`.
- Native Stellar USDC is accessed through the Stellar Asset Contract.
- Stellar USDC uses seven decimals; CCTP message amounts use six-decimal USDC units.
- Stellar CCTP integrations must handle 32-byte CCTP address payloads and recipient forwarding carefully.
- CCTP carries native USDC value movement.
- Axelar GMP carries authenticated instructions and acknowledgements, not value by itself.
- Stellar-specific CCTP, Axelar, Blend, Aquarius, and external security or partnership claims must be pinned from official or governance-supplied sources before public launch materials.

## 1B. Hub-And-Spoke Security Model

Stellar is the hub. Every other chain is a spoke. A spoke can custody local native USDC during an in-flight user action, burn USDC through CCTP, mint or burn a local fUSD representation after hub authorization, receive USDC liquidity from any supported chain, and execute approved local lending or DEX/LP strategies after hub allocation approval and chain-local guard approval. A spoke must never be treated as an independent source of truth.

Non-negotiable hub rules:

- Stellar `VaultAccounting` is the only canonical liability ledger.
- Stellar governance is the only authority that can add chains, caps, remote routers, remote token representations, and strategies.
- Spokes can mint fUSD for users, but only from an unexpired Stellar hub mint authorization backed by settled native USDC.
- Spokes cannot reduce canonical liabilities from local burns alone.
- Spokes cannot transfer fUSD supply directly to another spoke unless the movement is routed through, or synchronously authorized by, the Stellar hub.
- Spokes can receive USDC for investment from any supported source chain, but only under a Stellar-approved allocation route.
- CCTP settlement proves native USDC value movement.
- Axelar GMP proves instruction authenticity, not collateral by itself.
- Spoke-local collateral can create mint allowance only when an approved remote router/vault atomically locks native USDC and sends a canonical `SpokeCollateralLocked` proof to Stellar.
- Remote strategy reports can adjust conservative backing value, but not user mint allowance.

Canonical supply model:

```text
canonical_liabilities_6
  = stellar_fusd_supply_6
  + sum(spoke_fusd_supply_6)
  + pending_remote_mint_authorizations_6
  - pending_remote_burns_not_yet_accepted_6
```

Canonical backing model:

```text
canonical_backing_6
  = settled_idle_usdc_on_stellar_6
  + settled_spoke_escrow_usdc_6
  + conservative_strategy_value_6
  - pending_outbound_redemptions_6
  - required_reserve_6
```

Mint allowance model:

```text
new_mint_allowance_6 =
    finalized_cctp_settlement_6
  + local_stellar_deposit_6
  + accepted_spoke_collateral_lock_6
```

Strategy appreciation may improve collateral ratio and fund rewards after realization policy, but it cannot be used as a substitute for user-deposited native USDC when minting new fUSD.

`accepted_spoke_collateral_lock_6` is not a backend or indexer observation. It is an authenticated state transition from an approved remote router/vault that has already transferred native USDC into protocol custody on that spoke. The hub trusts only approved contract code and only up to chain-level local collateral caps.

User-facing mint rule:

```text
deposit USDC on any supported chain
  -> USDC settles by finalized CCTP on Stellar, or locks in an approved spoke vault
  -> Stellar records mint allowance
  -> fUSD mints on the user's chosen supported destination chain
```

Allocation rule:

```text
settled USDC on any supported chain
  -> Stellar approves allocation route and strategy cap
  -> USDC moves via CCTP to the target investment chain if needed
  -> approved spoke strategy deploys into guarded lending or DEX/LP venue
  -> strategy value reports back to Stellar for conservative backing
```

## 2. Relationship To Existing Base fUSD

The Base implementation has useful primitives:

- fUSD minted against collateral,
- vault/staking separation,
- guarded strategy execution,
- manager/governance controls,
- withdrawal and cooldown protections.

The Stellar version should not be a literal port. The Base implementation is chain-local. The cross-chain version must use Stellar as the source of truth and treat every remote chain as an execution domain.

Mapping:

| Base concept | Cross-chain Stellar equivalent |
| --- | --- |
| `TokenLogic` | `FusdToken` + `MintRedeemController` |
| `PoolLogic` | `VaultAccounting` + optional `SfUsdVault` |
| `PoolManagerLogic` | `AllocationManager` + `RiskConfig` |
| Contract/asset guards | per-chain `GuardRegistry` + `StrategyAdapterRegistry` + contract/asset guards |
| Chainlink asset handler | Reflector/SAC/adapter valuation + remote proofs/signals |
| Timelock/governance | `GovernanceController` on Stellar + Axelar-controlled remote executors |
| Manager/trader roles | Soroban role registry, bounded Trader execution, chain-local guarded executors, and timelocked operators |

Base guard pattern to preserve:

- `Governance` stores contract guard and asset guard registrations.
- Contract guards validate external protocol calls before execution and may run post-transaction checks.
- Asset guards compute balances, valuation, withdrawal transactions, and safe unwind paths.
- `PoolLogic.execTransaction()` / `PoolTxExecutor` style execution prevents the vault from calling arbitrary external contracts.
- Aave, Morpho, and Uniswap are accessed only through registered guards such as `AaveLendingPoolGuardV3`, `MorphoBlueContractGuard`, `UniswapV3RouterGuard`, `AaveV3LendingPoolAssetGuard`, `MorphoBlueAssetGuard`, and `UniswapV3AssetGuard`.

Trader role pattern to preserve:

- A trader can choose among approved routes and approved guarded strategies.
- A trader can execute guarded bridge/rebalance/strategy transactions.
- A trader cannot register new assets, chains, routes, guards, or external protocol targets.
- A trader cannot bypass caps, reserves, health-factor checks, slippage checks, or post-transaction guards.
- A trader cannot create mint allowance or modify canonical liabilities.

Manager role pattern to preserve:

- A manager can configure protocol fees only inside governance-approved maximums.
- A manager can select, pause, or unpause whitelisted strategies from a timelock-approved strategy universe.
- A manager cannot add arbitrary strategies, guards, external targets, routers, token mints, chains, or bridge domains.
- A manager cannot raise hard caps, lower reserves, upgrade contracts, mint, burn, settle collateral, or alter canonical liabilities.
- Manager fee and whitelist changes must emit events, be versioned, and be observable by the backend and proof-of-backing systems.

## 3. Design Goals

1. Mint fUSD 1:1 against native USDC.
2. Redeem fUSD at par whenever liquidity is available.
3. Keep Stellar as canonical source of truth for liabilities and allocation policy.
4. Avoid wrapped collateral and custodial bridges.
5. Use CCTP for native USDC movement between any supported source and destination chain.
6. Use Axelar for authenticated cross-chain messages only.
7. Preserve clear accounting invariants across chains.
8. Provide a guarded strategy system similar to the current Frgmnt Base model.
9. Allow deposited USDC on each chain to be invested in same-chain lending and DEX protocols only through registered guard contracts/programs.
10. Make every strategy position independently valuated and exit-capable.
11. Let fUSD remain a stable payment asset; expose yield through `sfUSD` or a reward module.

## 4. Non-Goals

- No algorithmic minting.
- No undercollateralized fUSD.
- No wrapped USDC as protocol collateral.
- No third-party bridge custody as canonical collateral.
- No minting solely from remote event logs without Stellar-side settlement or authorized reconciliation.
- No spoke-local minting without Stellar hub authorization.
- No opaque off-chain NAV accounting.

## 5. Verified Protocol Constraints

### 5.1 Soroban Token Compatibility

fUSD should implement the SEP-41/Soroban token interface:

- `allowance`
- `approve`
- `balance`
- `transfer`
- `transfer_from`
- `burn`
- `burn_from`
- `decimals`
- `name`
- `symbol`

Minting is not part of the required SEP-41 user interface, but the token implementation must emit compatible mint events and restrict mint authority to the controller.

### 5.2 Stellar USDC Through SAC

Native Stellar USDC is accessed by contracts through the Stellar Asset Contract. The protocol should treat the USDC SAC as the only local collateral token interface. There is no separate wrapped representation for Stellar-local USDC.

### 5.3 CCTP v2 on Stellar

Circle's Stellar CCTP stack contains:

- `TokenMessengerMinter`
- `MessageTransmitter`
- `CctpForwarder`

Stellar CCTP domain id: `27`.

Important constraints:

- CCTP address fields are 32-byte payloads.
- CCTP treats `mintRecipient` on Stellar as a contract address.
- When sending to Stellar accounts or muxed accounts, use `CctpForwarder`.
- `CctpForwarder.mint_and_forward(message, attestation)` atomically receives the CCTP message, mints USDC, and forwards it to the hook-specified recipient.
- CCTP messages use six-decimal USDC subunits even though Stellar USDC displays seven decimals.

### 5.4 Axelar GMP

Axelar provides cross-chain message routing through gateways, validator verification, relayers, and destination execution. For fUSD, Axelar is used for authenticated instruction passing, never as proof of USDC collateral by itself.

The production implementation must pin:

- source chain name as Axelar expects it,
- gateway address or Stellar gateway contract id,
- gas service address where applicable,
- executable interface on each chain,
- remote sender allowlist.

## 6. High-Level System Architecture

```text
                           +---------------------------+
                           |     Governance / Risk     |
                           | Timelock + risk policies  |
                           +-------------+-------------+
                                         |
                                         v
+-------------------+        +-----------+-----------+        +-------------------+
| Remote Chain A    |        | Stellar Canonical     |        | Remote Chain B    |
| EVM/Solana/etc.   |        | Soroban Layer         |        | EVM/Solana/etc.   |
|                   |        |                       |        |                   |
| RemoteRouter      |<--GMP--| GovernanceController  |--GMP-->| RemoteRouter      |
| RemoteFusd        |        | AllocationManager     |        | RemoteFusd        |
| CCTP Messenger    |--CCTP->| MintRedeemController  |<-CCTP--| CCTP Messenger    |
| Strategy Adapter  |        | VaultAccounting       |        | Strategy Adapter  |
+-------------------+        | FusdToken             |        +-------------------+
                             | SfUsdVault optional   |
                             | StrategyRegistry      |
                             | XycloansAdapter       |
                             | DefindexAdapter       |
                             +-----------+-----------+
                                         |
                                         v
                           +--------------------------------+
                           | Stellar DeFi                   |
                           | xycLoans, deFindex, Aquarius,   |
                           | SAC USDC                        |
                           | (Blend retained, not active —   |
                           |  see §8 status note)            |
                           +--------------------------------+
```

## 6A. Supported Chain Strategy

The protocol should not connect every available chain by default. Each chain must be selected by native USDC liquidity, CCTP support, Axelar/GMP availability, DeFi venue quality, oracle quality, operational reliability, wallet support, and expected user demand.

### 6A.1 Launch Set

Recommended first production set:

| Runtime | Chain | Purpose | Value rail | Message rail | Yield venues |
| --- | --- | --- | --- | --- | --- |
| Stellar | Stellar | Canonical accounting, governance, local mint/redeem, first yield venue | SAC USDC + CCTP domain 27 | Axelar GMP where configured | xycLoans and deFindex first (oracle-free, no undercollateralized-borrow surface); Aquarius for peg liquidity only; Blend retained but not active (see §8 status note) |
| EVM | Base | Mint/redeem spoke, existing traction, low-cost USDC onboarding, Aave/Morpho/Uniswap depth | CCTP domain 6 | Axelar chain id `base` | Guarded Aave/Morpho lending and Uniswap-style DEX/LP after guard validation |
| EVM | Ethereum | Mint/redeem spoke, highest-security settlement and deep USDC liquidity source | CCTP domain 0 | Axelar chain id `ethereum` | Guarded Aave/Morpho/DEX venues with conservative caps |
| EVM | Arbitrum | Mint/redeem spoke, large DeFi market and USDC liquidity | CCTP domain 3 | Axelar chain id `arbitrum` | Guarded lending and DEX venues after risk approval |
| EVM | OP Mainnet | Mint/redeem spoke with CCTP/Axelar-supported L2 liquidity | CCTP domain 2 | Axelar chain id `optimism` | Guarded lending and DEX venues after risk approval |
| Solana | Solana | Mint/redeem spoke with high-throughput native USDC user base | CCTP domain 5 | Axelar chain id `solana` where configured | Solana lending adapters in later phase |

Phase-2 candidates:

- Polygon PoS, Avalanche, Unichain, Linea, Sonic, and Monad.
- A chain should be added only after passing the onboarding checks below.

### 6A.2 Chain Onboarding Checks

Each chain needs an on-chain `ChainState` entry and an off-chain risk dossier.

Minimum checks:

- native USDC is supported by CCTP v2,
- CCTP domain id is pinned from Circle's official registry,
- Axelar chain id and gateway configuration are pinned from official Axelar config, if GMP is enabled,
- remote router or program has been deployed and security-validated,
- remote fUSD representation is either disabled or supply-mirrored,
- destination-chain fUSD minting is enabled only after remote mint authorization tests pass,
- cross-chain USDC allocation routes are explicitly approved source -> destination -> strategy,
- chain-specific mint cap and daily mint limit are configured,
- emergency pause has been tested,
- indexer can reconcile CCTP, GMP, and token events,
- at least two RPC providers and one archive/indexing path exist,
- no yield strategy is enabled until its adapter passes separate security validation.

### 6A.3 Runtime Matrix

| Runtime | Contract type | Token standard | Native USDC handling | Remote fUSD handling |
| --- | --- | --- | --- | --- |
| Stellar | Soroban contracts | SEP-41/SAC-compatible | USDC SAC, seven decimals locally | SEP-41 fUSD canonical |
| EVM | Solidity contracts | ERC-20 | Native USDC, six decimals | ERC-20 controlled by custom hub-authorized router |
| Solana | Rust programs | SPL Token or Token-2022 | Native USDC SPL mint, six decimals | SPL Token or Token-2022 representation |

All runtimes normalize accounting to `USDC_6` on Stellar.

### 6A.4 Spoke State Machine

Every spoke chain has one canonical state entry on Stellar:

```rust
struct SpokeState {
    chain_id: u32,
    runtime: Symbol, // "evm", "solana", "stellar"
    cctp_domain: u32,
    axelar_id: Symbol,
    remote_router: BytesN<32>,
    remote_fusd: BytesN<32>,
    mint_cap_6: i128,
    daily_mint_limit_6: i128,
    outstanding_supply_6: i128,
    pending_mint_auth_6: i128,
    pending_burn_acceptance_6: i128,
    deposits_paused: bool,
    redeems_paused: bool,
    remote_mint_paused: bool,
    active: bool,
    daily_redeem_limit_6: i128,        // max redemption per 24h window for this chain
    redeemed_today_6: i128,            // redeemed so far in current day window
    redeem_day_start_ledger: u32,      // ledger at which current day window started
}
```

Allowed transitions:

```text
inactive -> active                         governance only
active -> chain_paused                     guardian or governance
chain_paused -> active                     governance only
settled_usdc -> pending_remote_mint_auth   hub only
pending_remote_mint_auth -> remote_supply  spoke execution + authenticated hub acknowledgement
remote_burn_notice -> pending_burn_acceptance
pending_burn_acceptance -> liability_reduce hub acceptance only
settled_usdc -> pending_allocation_route    hub only
pending_allocation_route -> strategy_value  target strategy execution + hub report acceptance
```

Forbidden transitions:

```text
spoke_observed_deposit -> remote_supply without hub settlement
spoke_observed_burn -> liability_reduce without hub acceptance
strategy_report -> mint_allowance
spoke_A -> spoke_B direct supply transfer without hub accounting
spoke_idle_usdc -> strategy_value without hub allocation approval
```

Redemption rate limit:

`redeem_window_ledgers: u32` is a governance-settable parameter stored in `GlobalState`
(default: `69,120` ledgers ≈ 24 hours at 1.25s/ledger).

```text
Before accepting any spoke burn or local Stellar redemption:

  // Rollover check — MUST happen before comparing accumulated totals.
  // Without this reset, the first day's limit permanently blocks all future redemptions
  // until governance manually resets the counter.
  if current_ledger >= redeem_day_start_ledger[chain] + redeem_window_ledgers:
    reset redeemed_today_6[chain] = 0, redeem_day_start_ledger[chain] = current_ledger
  if current_ledger >= global_redeem_day_start_ledger + redeem_window_ledgers:
    reset global_redeemed_today_6 = 0, global_redeem_day_start_ledger = current_ledger

  // Limit check after rollover.
  check: redeemed_today_6[chain] + amount <= daily_redeem_limit_6[chain]
  check: global_redeemed_today_6 + amount <= global_daily_redeem_limit_6

If limit exceeded:
  Option A (queue): add redemption to RedemptionQueue; process FIFO as limit resets.
  Option B (reject): return error RedemptionDailyLimitExceeded with next available window ledger.
  Governance configures which mode is active per chain.
```

Add `redeem_window_ledgers: u32` to `GlobalState`.

## 7. Core Contracts On Stellar

### 7.1 `FusdToken`

Purpose:

- SEP-41-compatible fUSD token.
- Stable transfer asset.
- Mint/burn restricted to protocol controller.

Recommended decimals:

- 6 decimals for the cross-chain fUSD token supply, matching CCTP message amounts and native USDC on EVM/Solana.
- 7-decimal Stellar USDC remains supported at the collateral interface through explicit conversion.

Recommendation: use 6 decimals for fUSD on all chains, including Stellar. Stellar-native USDC has seven decimals, but fUSD should not. The Stellar deposit path floors or returns the seventh-decimal dust before minting. This avoids remote supply mirror drift and makes `1 fUSD unit == 1 USDC_6 unit` across Stellar, EVM, and Solana.

Required admin methods:

```rust
pub trait FusdAdmin {
    fn init(e: Env, admin: Address, controller: Address, decimals: u32);
    fn set_controller(e: Env, admin: Address, controller: Address);
    fn mint(e: Env, controller: Address, to: Address, amount: i128);
    fn controller_burn(e: Env, controller: Address, from: Address, amount: i128);
    fn pause(e: Env, admin: Address);
    fn unpause(e: Env, admin: Address);
}
```

Required properties:

- `mint` requires controller auth.
- `controller_burn` requires controller auth.
- User `burn` and `burn_from` should follow SEP-41 semantics.
- Transfers should not mutate collateral accounting.
- Optional denylist/compliance hooks should be separated from core accounting.

Storage:

```rust
enum DataKey {
    Admin,
    Controller,
    Paused,
    TotalSupply,
    Balance(Address),
    Allowance(Address, Address),
    Nonce(Address),
}
```

Events:

- `mint(to, amount)`
- `burn(from, amount)`
- `transfer(from, to, amount)`
- `controller_set(old, new)`
- `paused`
- `unpaused`

### 7.2 `VaultAccounting`

Purpose:

- Canonical state machine for liabilities, collateral, strategy value, reserves, pending messages, and chain-level exposures.

Core invariant:

```text
total_fusd_liabilities <= verified_collateral_value - pending_redemptions - required_reserve
```

Accounting units:

- Use `i128` for Soroban token amounts.
- Use an internal `u128` or checked `i128` domain for positive quantities.
- Store canonical USDC value in `USDC_6` units.
- Convert Stellar SAC USDC 7-decimal amounts to CCTP-compatible 6-decimal units by flooring the final digit when crossing CCTP boundaries.

Key state:

```rust
struct GlobalState {
    total_liabilities_6: i128,
    settled_idle_usdc_6: i128,
    settled_spoke_escrow_usdc_6: i128,
    total_strategy_value_6: i128,
    mint_allowance_6: i128,
    pending_inbound_usdc_6: i128,
    pending_fast_credit_6: i128,
    pending_outbound_usdc_6: i128,
    fast_credit_insurance_reserve_6: i128,
    required_reserve_bps: u32,
    protocol_dust_usdc_7: i128,
    cancel_timeout_ledgers: u32,   // ledgers after mint_auth issuance after which depositor can cancel
    hub_cctp_domain: u32,          // Stellar CCTP domain id (27 at mainnet); must not be hardcoded
    guardian_self_revoke_window_ledgers: u32,  // window for guardian to self-revoke false-alarm pause
    remote_strategy_cap_bps: u32,  // max remote (non-Stellar) strategy value as % of total backing
    global_daily_redeem_limit_6: i128,
    global_redeemed_today_6: i128,
    global_redeem_day_start_ledger: u32,
    redeem_window_ledgers: u32,         // length of one redemption rate-limit window (default: 69,120 ≈ 24h)
    fast_credit_min_headroom_6: i128,
    max_yield_per_epoch_6: i128,   // max yield notifiable to SfUsdVault per epoch
    yield_credited_this_epoch_6: i128, // accumulates notify_realized_yield calls in current epoch
    epoch_start_ledger: u32,       // ledger at which the current yield epoch started
    epoch_length_ledgers: u32,     // governance-set epoch length (default: ~1 day)
    mint_auth_ack_timeout_ledgers: u32, // ledgers after mint_auth issuance before force_reconcile allowed (default: ~7 days = 483,840)
    release_timeout_ledgers: u32,  // ledgers after ReleaseRequest.initiated_ledger before retry/rollback allowed
    paused: bool,
}

struct ChainState {
    chain_id: u32,
    axelar_name: Symbol,
    cctp_domain: u32,
    remote_router: BytesN<32>,
    remote_vault: BytesN<32>,
    remote_fusd: BytesN<32>,
    max_mint_6: i128,
    local_collateral_cap_6: i128,
    minted_6: i128,
    outstanding_supply_6: i128,
    pending_mint_auth_6: i128,
    pending_burn_acceptance_6: i128,
    pending_in_6: i128,
    pending_out_6: i128,
    idle_usdc_6: i128,
    settled_spoke_escrow_usdc_6: i128,
    deployed_strategy_value_6: i128,
    reserved_redemption_6: i128,
    withdrawable_usdc_6: i128,
    chain_reserve_bps: u32,
    short_horizon_redemption_6: i128,
    cctp_capacity_in_6: i128,
    cctp_capacity_out_6: i128,
    local_collateral_enabled: bool,
    active: bool,
}

struct StrategyState {
    strategy_id: BytesN<32>,
    adapter: Address,
    chain_id: u32,
    deployed_value_6: i128,
    debt_ceiling_6: i128,
    target_bps: u32,
    max_bps: u32,
    liquidity_score: u32,
    risk_score: u32,
    paused: bool,
    last_report_ledger: u32,
}
```

Fast-finality credit is never mint allowance by default. It is a separate risk bucket and can be used for instant user UX only if fully covered by `fast_credit_insurance_reserve_6` and enabled by governance caps.

Spoke-local collateral is tracked separately from CCTP-settled Stellar collateral. It can back fUSD only while native USDC remains in an approved protocol vault or approved guarded strategy on that spoke.

Required methods:

```rust
pub trait VaultAccounting {
    fn init(e: Env, admin: Address);

    fn record_local_deposit(e: Env, caller: Address, amount_6: i128);
    fn record_inbound_settlement(
        e: Env,
        caller: Address,
        msg_hash: BytesN<32>,
        // §10.2A: This MUST be the balance-delta value computed by MintRedeemController
        // as (usdc_sac.balance(after) - usdc_sac.balance(before)) bracketing the CCTP
        // receive call in the same transaction. It is NOT the CCTP message amount and
        // must never be supplied by a relayer or external caller.
        net_received_6: i128,
        source_domain: u32,
        finality_threshold: u32,
        finalized: bool
    );
    fn record_spoke_collateral_locked(e: Env, caller: Address, lock_id: BytesN<32>, payload_hash: BytesN<32>, payload: Bytes);
    fn record_spoke_collateral_released(e: Env, caller: Address, release_id: BytesN<32>, payload_hash: BytesN<32>, payload: Bytes);
    fn mint_liability_from_settled_usdc(e: Env, caller: Address, to: Address, amount_6: i128);
    fn burn_liability_for_redemption(e: Env, caller: Address, from: Address, amount_6: i128);
    fn authorize_remote_mint(e: Env, caller: Address, payload_hash: BytesN<32>, payload: Bytes);
    fn accept_remote_burn(e: Env, caller: Address, payload_hash: BytesN<32>, payload: Bytes);

    fn reserve_outbound(e: Env, caller: Address, redeem_id: BytesN<32>, amount_6: i128, destination_domain: u32);
    fn mark_outbound_sent(e: Env, caller: Address, redeem_id: BytesN<32>, cctp_nonce: BytesN<32>);
    fn cancel_outbound(e: Env, caller: Address, redeem_id: BytesN<32>);

    fn report_strategy_value(e: Env, caller: Address, report_hash: BytesN<32>, report: Bytes);
    fn move_idle_to_strategy(e: Env, caller: Address, strategy_id: BytesN<32>, amount_6: i128);
    fn move_strategy_to_idle(e: Env, caller: Address, strategy_id: BytesN<32>, amount_6: i128);

    // Internal enforcement — panics with InvariantViolated if solvency condition fails.
    // Must be called as the final action in every state-mutating method.
    fn assert_invariant(e: Env);

    // External monitoring — returns bool without panicking.
    // For read-only clients, indexers, and proof-of-reserves tools.
    fn check_invariant(e: Env) -> bool;

    fn global_state(e: Env) -> GlobalState;
}
```

Access:

- Only `MintRedeemController` can record deposits, burns, and mints.
- Only approved strategy adapters can report strategy value.
- Only `AllocationManager` can move idle/strategy accounting.
- Only governance can change caps/reserve/risk settings.

Atomicity rules:

- A user deposit must record settled USDC and mint liability in the same successful transaction where possible.
- If a cross-chain deposit requires multiple transactions, settlement creates `mint_allowance_6`; minting consumes that allowance.
- A strategy value report must never increase `mint_allowance_6`.
- Every state-changing method must call `assert_invariant` as its final action before returning. `assert_invariant` calls `env.panic_with_error(InvariantViolated)` if the solvency condition is not satisfied, reverting the entire transaction.
- `check_invariant` is a read-only view function for monitoring only. It must never be used as a gate in state-changing code. State-changing code must always call `assert_invariant`.
- Pre-condition checks (input validation, cap checks, pause checks) must be performed before state writes to minimize the cases where `assert_invariant` catches an issue.

### 7.3 `MintRedeemController`

Purpose:

- User entry point for minting and redeeming.
- Coordinates USDC SAC transfer, CCTP receive/send, and fUSD mint/burn.

Local mint flow:

```rust
fn deposit_usdc(e: Env, user: Address, amount_7: i128, min_fusd_7: i128, min_fee_version: u32) {
    user.require_auth();
    assert_not_paused();
    assert(amount_7 > 0);
    let fees = fee_manager.active_fees(&e);
    assert(fees.version == min_fee_version, FeeVersionMismatch);

    usdc_sac.transfer(user, current_contract, amount_7);
    let amount_6 = usdc7_to_usdc6_floor(amount_7);
    assert(amount_6 > 0);

    accounting.record_local_deposit(current_contract, amount_6);
    accounting.mint_liability_from_settled_usdc(current_contract, user, amount_6);
    fusd.mint(current_contract, user, usdc6_to_fusd_units(amount_6));
}
```

Local redeem flow:

```rust
fn redeem_local(e: Env, user: Address, fusd_amount: i128, min_usdc_7: i128, min_fee_version: u32) {
    user.require_auth();
    assert_not_paused();
    assert(fusd_amount > 0);
    let fees = fee_manager.active_fees(&e);
    assert(fees.version == min_fee_version, FeeVersionMismatch); // stale-quote protection

    let amount_6 = fusd_to_usdc6(fusd_amount);
    assert(available_idle_6() >= amount_6);

    fusd.controller_burn(current_contract, user, fusd_amount);
    accounting.burn_liability_for_redemption(current_contract, user, amount_6);

    let amount_7 = usdc6_to_usdc7(amount_6);
    assert(amount_7 >= min_usdc_7);
    usdc_sac.transfer(current_contract, user, amount_7);
}
```

Inbound CCTP mint flow:

```text
Remote user deposits USDC
  -> remote router burns USDC with CCTP
  -> Circle Iris attests
  -> Stellar CctpForwarder or protocol receiver executes receive_message
  -> native USDC arrives at protocol
  -> controller verifies message hash not consumed
  -> accounting records inbound settlement
  -> fUSD is minted on Stellar or the hub emits a bounded remote mint authorization
```

Redeem to remote chain has two separate state machines. A local Stellar burn can reserve USDC immediately after the burn. A spoke burn cannot reserve or release USDC until the hub accepts the remote burn proof.

Stellar burn redeem:

```text
User burns fUSD on Stellar
  -> canonical liability is reduced
  -> canonical Stellar accounting reserves USDC
  -> controller calls Stellar TokenMessengerMinter.deposit_for_burn
  -> CCTP sends native USDC to destination
  -> remote receiver releases/mints destination USDC
```

Spoke burn redeem:

```text
User burns remote fUSD on spoke
  -> remote router emits local burn event
  -> remote router sends authenticated RemoteBurnNotice GMP to Stellar
  -> hub validates source chain, router, token, burn id, amount, recipient, and nonce
  -> hub accepts burn id exactly once
  -> canonical liability is reduced
  -> Stellar accounting reserves USDC
  -> controller sends native USDC through CCTP
```

Key methods:

```rust
pub trait MintRedeemController {
    fn init(e: Env, admin: Address, fusd: Address, usdc_sac: Address, accounting: Address);

    fn deposit_usdc(e: Env, user: Address, amount_7: i128, min_fusd: i128, min_fee_version: u32);
    fn redeem_local(e: Env, user: Address, fusd_amount: i128, min_usdc_7: i128, min_fee_version: u32);

    // §10.2A / ISSUE-FIX: `amount_6` is NOT accepted from the relayer caller.
    // The amount credited to mint_allowance_6 is computed internally as the balance
    // delta (usdc_sac.balance(after) - usdc_sac.balance(before)) in the same transaction
    // as the CCTP receive call. The CCTP message amount is parsed from the Circle-attested
    // message by Circle's own contract and used only for validation and event emission.
    // Relayers cannot influence the credited amount by passing any parameter.
    fn receive_cctp_settlement(
        e: Env,
        caller: Address,
        cctp_message_hash: BytesN<32>,
        source_domain: u32,
        source_sender: BytesN<32>,
        recipient: Address
        // amount_6 removed: do NOT accept from relayer. Compute internally via balance delta.
        // See §10.2A. cctp_message_amount_6 is only available via the Circle-attested message.
    );
    // Implementation status (2026-09-02): `mint-redeem-controller`'s current
    // `receive_cctp_settlement` still accepts a `mock_net_received_6` parameter
    // directly — the balance-delta computation this trait describes requires a real
    // Stellar CCTP `receive_message` integration this repo does not yet vendor. What
    // IS implemented: the function is gated to a single admin-appointed `Relayer`
    // address (`set_relayer`) rather than being fully permissionless, closing the
    // "anyone can mint themselves fUSD" path that existed once the local-mint
    // recipient became caller-directable. Do not treat the credited amount as
    // trustworthy until the real balance-delta computation this trait specifies is
    // implemented.

    fn redeem_remote(
        e: Env,
        user: Address,
        fusd_amount: i128,
        destination_domain: u32,
        mint_recipient: BytesN<32>,
        max_fee_6: i128,
        min_finality_threshold: u32,
        min_fee_version: u32        // reverts with FeeVersionMismatch if fees changed since quote
    );

    fn authorize_spoke_mint(
        e: Env,
        caller: Address,
        mint_auth_id: BytesN<32>,
        payload_hash: BytesN<32>,
        payload: Bytes
    );

    fn accept_spoke_burn(
        e: Env,
        caller: Address,
        burn_id: BytesN<32>,
        payload_hash: BytesN<32>,
        payload: Bytes
    );

    // Cancels an expired or permanently stuck remote mint authorization.
    //
    // Caller authorization:
    // - Governance: may cancel at any time.
    // - Original depositor (Stellar-native only): may cancel after cancel_timeout_ledgers when
    //   MintAuthRecord.depositor_chain_id == 0. The caller must be the Stellar address recorded
    //   in MintAuthRecord.depositor_address. This is verifiable on-chain.
    // - Cross-chain depositors (depositor_chain_id != 0): self-service cancellation is NOT
    //   permitted because the hub cannot authenticate a cross-chain address as a Stellar signer.
    //   These cases must go through governance. The indexer should expose a governance-proxy
    //   cancellation flow for these users.
    //
    // Effect: releases pending_mint_auth_6, and either restores mint_allowance_6
    // for re-mint or dispatches a CCTP USDC refund to refund_recipient.
    fn cancel_mint_authorization(
        e: Env,
        caller: Address,
        mint_auth_id: BytesN<32>,
        recovery_mode: Symbol, // "remint" or "refund"
        refund_recipient: BytesN<32>, // destination address for CCTP refund (if mode=refund)
        refund_domain: u32,   // CCTP destination domain for refund (if mode=refund)
    );
}
```

Fee version rule: `min_fee_version` is the fee config version the user saw when their transaction was built. If `active_fees().version != min_fee_version`, the call reverts with `FeeVersionMismatch`. The frontend must query `active_fees()` to get the current version and include it in the signed transaction. This ensures stale-quote protection is enforced on-chain, not just in the UI.

### 7.4 `AllocationManager`

Purpose:

- Stores allocation policy and triggers strategy movements.
- Converts risk outputs into enforceable on-chain caps.

Allocation input:

```rust
struct AllocationTarget {
    strategy_id: BytesN<32>,
    source_chain_id: u32,
    destination_chain_id: u32,
    destination_cctp_domain: u32,
    guard_set_id: BytesN<32>,
    target_bps: u32,
    max_bps: u32,
    min_liquidity_6: i128,
    max_delta_per_rebalance_6: i128,
    valid_until_ledger: u32,
    risk_epoch: u64,
}
```

Allocation routes:

```rust
struct AllocationRoute {
    route_id: BytesN<32>,
    version: u32,                       // increments on each governance modification
    source_chain_id: u32,
    destination_chain_id: u32,
    destination_cctp_domain: u32,
    strategy_id: BytesN<32>,
    guard_set_id: BytesN<32>,
    route_kind: Symbol,                 // "local", "cctp_remote"
    max_in_flight_6: i128,
    in_flight_timeout_ledgers: u32,     // in-flight USDC that never arrives is timed out after this
    max_daily_move_6: i128,
    min_post_move_liquidity_6: i128,
    valid_until_ledger: u32,
    active: bool,
}
```

USDC may be invested locally on the chain where it was deposited or moved from any supported source chain to any supported investment chain, but only through an active `AllocationRoute` approved by Stellar governance. A route is not a mint permission: moving USDC into a lending or DEX venue can improve backing after settlement and valuation, but it cannot create user mint allowance.

Route versioning: When governance modifies a route's parameters (caps, chain, domain, strategy, guard set), a new `version` value is set. All allocation execution methods bind `(route_id, version, route_nonce)` as the idempotency key. In-flight allocations from the previous version are processed to completion or timed out before the new version activates. Old `(route_id, old_version, nonce)` tuples cannot be replayed against the new version.

In-flight allocation tracking:

```rust
struct InFlightAllocation {
    allocation_id: BytesN<32>,          // hash(route_id, route_version, route_nonce) — ledger is NOT included to prevent duplicate IDs on resubmission
    route_id: BytesN<32>,
    route_version: u32,
    amount_6: i128,
    destination_chain_id: u32,
    destination_cctp_domain: u32,
    strategy_id: BytesN<32>,
    initiated_ledger: u32,
    timeout_ledger: u32,               // initiated_ledger + in_flight_timeout_ledgers
    status: InFlightStatus,
}

enum InFlightStatus {
    Sent,         // USDC burned on source, not yet confirmed arrived at destination
    Arrived,      // destination confirmed receipt (GMP ack or on-chain proof)
    Failed,       // destination confirmed failure
    TimedOut,     // timeout_ledger passed without ack; recovery available
}
```

Recovery:
- `timeout_in_flight_allocation(allocation_id)` callable by governance or authorized operator
  after `timeout_ledger` has passed.
- Restores `max_in_flight_6` capacity for the route.
- Triggers a write-off to strategy loss accounting or initiates a CCTP refund recovery path.
- `AllocationManager` must expose `timeout_in_flight_allocation` and `acknowledge_allocation_arrived`.

Manager-configurable strategy policy:

```rust
struct StrategyWhitelistEntry {
    strategy_id: BytesN<32>,
    chain_id: u32,
    adapter: Address,
    guard_set_id: BytesN<32>,
    strategy_type: Symbol,
    manager_selectable: bool,
    deposit_enabled: bool,
    withdraw_enabled: bool,
    max_bps: u32,
    debt_ceiling_6: i128,
    min_liquidity_6: i128,
    valid_until_ledger: u32,
    version: u32,
}

struct FeeConfig {
    mint_fee_bps: u32,
    redeem_fee_bps: u32,
    management_fee_bps: u32,
    performance_fee_bps: u32,
    protocol_fee_recipient: Address,
    max_total_user_fee_bps: u32,
    version: u32,
}
```

Managers can choose among strategies that governance has already registered as `manager_selectable`. Managers can enable deposits, disable deposits, pause a selectable strategy, and set active target weights inside the hard `max_bps`, `debt_ceiling_6`, and reserve limits. Managers cannot create the whitelist universe, register adapters, register guards, change token addresses, or raise hard caps.

Route kinds:

- `local`: source chain equals destination chain; no CCTP movement is required. The local guarded executor deploys native USDC into same-chain lending or DEX venues.
- `cctp_remote`: source chain differs from destination chain; USDC moves through CCTP before the destination guarded executor can deploy it.

Methods:

```rust
pub trait AllocationManager {
    fn set_target(e: Env, gov: Address, target: AllocationTarget);
    fn set_route(e: Env, gov: Address, route: AllocationRoute);
    fn manager_select_strategy(e: Env, manager: Address, strategy_id: BytesN<32>, active_target_bps: u32, valid_until_ledger: u32);
    fn manager_pause_selectable_strategy(e: Env, manager: Address, strategy_id: BytesN<32>);
    fn manager_unpause_selectable_strategy(e: Env, manager: Address, strategy_id: BytesN<32>);
    fn pause_strategy(e: Env, gov: Address, strategy_id: BytesN<32>);
    fn unpause_strategy(e: Env, gov: Address, strategy_id: BytesN<32>);

    // route_version: the caller commits to the specific AllocationRoute version they are
    // executing against. The contract MUST reject if AllocationRoute.version != route_version.
    // This prevents a governance route update racing with an in-flight execution from silently
    // using a stale route. The allocation_id = hash(route_id, route_version, route_nonce).
    fn execute_bridge_route(e: Env, trader: Address, route_id: BytesN<32>, route_version: u32, amount_6: i128, destination_chain_id: u32, route_nonce: u64);
    fn rebalance_to_strategy(e: Env, operator: Address, route_id: BytesN<32>, route_version: u32, strategy_id: BytesN<32>, amount_6: i128, route_nonce: u64);
    fn rebalance_from_strategy(e: Env, operator: Address, route_id: BytesN<32>, route_version: u32, strategy_id: BytesN<32>, amount_6: i128, route_nonce: u64);
    fn emergency_exit(e: Env, guardian: Address, strategy_id: BytesN<32>, max_loss_bps: u32);
}
```

**Implementation status (2026-09-02):** the `allocation-manager` crate implements a
scoped-down subset of this trait — single-strategy `allocate`/`deallocate`/
`deallocate_all`/`report_value`/`emergency_exit` with debt-ceiling and active/enabled
flags, but no `AllocationTarget`/`AllocationRoute`/guard-set/route-nonce machinery, no
manager-selectable-strategy weighting, and no CCTP-remote route execution — those remain
aspirational. Its actual `emergency_exit(e, caller, strategy_id, min_out_6)` takes an
absolute USDC floor from the admin caller rather than this trait's `max_loss_bps`
percentage; `caller` must be the shared governance address that also administers every
registered adapter's own `Admin`, since `emergency_exit` relays that caller through to
the adapter's own Admin-gated `emergency_exit` rather than substituting
AllocationManager's own contract address (an earlier version of this code had that bug —
see `../README.md` "Key security properties demonstrated" §9).

Rules:

- Cannot allocate below required liquidity reserve.
- Cannot exceed chain cap or strategy cap.
- Cannot rebalance more than `max_delta_per_rebalance`.
- Cannot allocate to paused strategies.
- Trader can choose and execute among approved `AllocationRoute` entries, including local same-chain strategy execution and CCTP remote movement.
- Cannot execute if `route_id` is inactive (`active == false`), route is expired (`current_ledger > route.valid_until_ledger`), over in-flight cap, over daily movement cap, or not bound to the target adapter.
- Source chain, destination chain, destination CCTP domain, strategy id, guard set id, route kind, and route nonce must match the approved `AllocationRoute`.
- Local same-chain investment still requires Stellar hub route approval and local guard approval.
- Trader cannot create, update, or activate routes; only governance can.
- Trader cannot increase caps, lower reserves, register guards, change CCTP domains, or alter strategy debt ceilings.
- Manager can adjust active strategy selection and target weights only for timelock-whitelisted `manager_selectable` strategies and only within existing caps.
- Manager cannot whitelist a new strategy, register a new adapter/guard, increase `max_bps`, increase `debt_ceiling_6`, reduce reserves, or change route destinations.
- Emergency exit should work while normal allocation is paused.

### 7.5 `StrategyAdapterRegistry`

Purpose:

- Soroban equivalent of Base contract/asset guard registry.
- Keeps strategy adapters explicit and replaceable.

```rust
struct AdapterInfo {
    adapter: Address,
    strategy_type: Symbol,
    version: u32,
    active: bool,
    can_deposit: bool,
    can_withdraw: bool,
    can_value: bool,
}
```

Methods:

```rust
fn register_adapter(e: Env, gov: Address, strategy_id: BytesN<32>, info: AdapterInfo);
fn disable_adapter(e: Env, gov: Address, strategy_id: BytesN<32>);
fn set_manager_selectable(e: Env, gov: Address, strategy_id: BytesN<32>, selectable: bool);
fn adapter(e: Env, strategy_id: BytesN<32>) -> AdapterInfo;
```

Governance owns adapter registration and the outer whitelist. A Manager may only choose among already registered and manager-selectable strategies through `AllocationManager`; this prevents a Manager key from turning arbitrary external protocols into fUSD backing.

### 7.5A `FeeManager`

Purpose:

- Store protocol fee policy.
- Let a Manager tune fees inside governance-approved bounds.
- Keep user-facing fees deterministic and independently verifiable.

Fee surfaces:

- mint fee,
- redeem fee,
- cross-chain route fee,
- management fee on yield vault assets,
- performance fee on realized yield.

Default user mint fee should be zero if the product commitment is zero-fee minting. If future fees are enabled, the maximum fee bounds must be approved by timelock and displayed by the dApp before transaction signing.

State:

```rust
struct FeeBounds {
    max_mint_fee_bps: u32,
    max_redeem_fee_bps: u32,
    max_route_fee_bps: u32,
    max_management_fee_bps: u32,
    max_performance_fee_bps: u32,
    fee_change_cooldown_ledgers: u32,
    active: bool,
}

struct ActiveFeeConfig {
    mint_fee_bps: u32,
    redeem_fee_bps: u32,
    route_fee_bps: u32,
    management_fee_bps: u32,
    performance_fee_bps: u32,
    fee_recipient: Address,
    version: u32,
    effective_ledger: u32,
}
```

Methods:

```rust
// Submitted by Manager — contains only rate fields. Does NOT include fee_recipient.
// fee_recipient is governance-only and is stored/set separately via set_fee_recipient.
struct ManagerFeeConfig {
    mint_fee_bps: u32,
    redeem_fee_bps: u32,
    route_fee_bps: u32,
    management_fee_bps: u32,
    performance_fee_bps: u32,
}

pub trait FeeManager {
    fn set_fee_bounds(e: Env, gov: Address, bounds: FeeBounds);
    fn set_fee_recipient(e: Env, gov: Address, recipient: Address);
    fn manager_set_fees(e: Env, manager: Address, config: ManagerFeeConfig);
    fn active_fees(e: Env) -> ActiveFeeConfig;   // combines ManagerFeeConfig + governance-set recipient/version
}
```

Rules:

- Governance sets maximum fee bounds and fee recipient via `set_fee_bounds` and `set_fee_recipient`.
- Manager can set active fee RATES only (via `ManagerFeeConfig`) inside those maximums and only after `fee_change_cooldown_ledgers` from the last active config's `effective_ledger`.
- Manager CANNOT change `fee_recipient`. The `fee_recipient` field in `ActiveFeeConfig` is populated by the contract from the governance-set value at time of read — it is not a field the Manager submits.
- A second `manager_set_fees` call before `effective_ledger` of the pending config replaces the pending config atomically. Only the most recently submitted pending config takes effect.
- Fee increases above current user-visible quote must invalidate the quote.
- Fees are charged from realized amounts, not by minting unbacked fUSD.
- Fee collection must never reduce required reserves below the configured floor.
- Emergency guardian can set fees to zero or pause fee collection if fee logic is suspected faulty.

### 7.6 Strategy Adapter Interface

Every adapter must implement:

```rust
pub trait StrategyAdapter {
    fn strategy_id(e: Env) -> BytesN<32>;
    fn asset(e: Env) -> Address;

    // Always returns 6 for all adapters (Stellar and remote).
    // Stellar USDC 7->6 conversion occurs at MintRedeemController/AllocationManager boundary.
    fn underlying_decimals(e: Env) -> u32;

    fn balance_underlying_6(e: Env) -> i128;   // 6-decimal balance
    fn value_usdc_6(e: Env) -> i128;

    fn deposit(e: Env, caller: Address, amount_6: i128, min_shares: i128);
    fn withdraw(e: Env, caller: Address, amount_6: i128, min_out_6: i128);
    fn withdraw_all(e: Env, caller: Address, min_out_6: i128);

    fn emergency_exit(e: Env, caller: Address, min_out_6: i128);
}
```

Decimal boundary rule:

Stellar-native USDC has 7 decimals (SAC). The 7→6 conversion (`usdc7_to_usdc6_floor`)
MUST occur at the `MintRedeemController` and `AllocationManager` layer, BEFORE calling any
strategy adapter. Adapters always receive and return USDC-6 amounts. This ensures adapters
do not need to know whether they are being invoked from a Stellar-local or a remote-chain
context, and prevents decimal-handling bugs at adapter boundaries.

Adapters must:

- be non-upgradeable or timelock-upgradeable,
- expose deterministic valuation,
- enforce authorized callers,
- trap on unsupported actions,
- emit standardized events,
- not retain unaccounted yield outside accounting.

### 7.7 Per-Chain Guarded Strategy Execution

Every supported chain can invest deposited native USDC into lending protocols and DEX/LP venues on that same chain, but only through a local guard system derived from the existing Frgmnt Base model.

Principle:

```text
Stellar hub approves allocation route and caps
  -> chain-local guarded executor receives permission
  -> contract guard validates the external protocol call
  -> executor performs the call
  -> post-transaction guard validates resulting position
  -> asset guard reports value, withdrawal path, and risk data back to Stellar
```

Per-chain components:

```rust
struct GuardRegistry {
    chain_id: u32,
    governance: BytesN<32>,
    guarded_executor: BytesN<32>,
    active: bool,
}

struct ContractGuardConfig {
    chain_id: u32,
    target_contract: BytesN<32>,
    guard: BytesN<32>,
    protocol: Symbol, // "aave", "morpho", "uniswap", "blend", "aquarius", etc.
    active: bool,
}

struct AssetGuardConfig {
    chain_id: u32,
    asset_or_position_type: BytesN<32>,
    guard: BytesN<32>,
    valuation_mode: Symbol,
    withdrawal_mode: Symbol,
    active: bool,
}
```

Guard interfaces by runtime:

```text
EVM:
  contract_guard.txGuard(vault, target, calldata) -> checked calldata / transaction type
  contract_guard.afterTxGuard(vault, target, calldata) -> post-state validation
  asset_guard.getBalance(vault, asset_or_position) -> position balance
  asset_guard.withdrawProcessing(vault, asset, amount, slippage) -> safe unwind txs

Solana:
  instruction_guard.pre_validate(program_id, accounts, data)
  instruction_guard.post_validate(program_id, accounts, data)
  asset_guard.value_position(position_accounts)
  asset_guard.withdraw_instructions(position_accounts, amount, slippage)

Stellar:
  adapter_guard.validate_call(target, function, args)
  adapter_guard.validate_after(target, position_id)
  asset_guard.value_usdc_6(position_id)
  asset_guard.withdraw_plan(position_id, amount_6)
```

Allowed guarded venues:

- EVM lending: Aave V3, Morpho Blue, and future approved lending markets.
- EVM DEX/LP: Uniswap V3-style swaps and LP positions, primarily for rebalancing, unwind, and protocol-owned peg liquidity.
- Stellar lending/liquidity: xycLoans through `XycloansAdapter`, and deFindex through
  `DefindexAdapter`, plus guard checks. `BlendAdapter` exists in the codebase but is not
  an active venue (see §8 status note) — do not route capital to it against a live Blend
  V1/V2 pool.
- Stellar DEX/LP: Aquarius through guard checks, treated as peg liquidity or haircutted backing only after validation.
- Solana lending/DEX strategy execution: disabled in first release, then enabled only through Solana instruction guards and asset guards after separate security validation. Solana mint/redeem and CCTP routing remain in scope for activated Solana spokes.

Mandatory guard checks:

- target contract/program is registered,
- calldata/instruction matches an allowlisted function,
- asset/token mint is supported and native, not wrapped collateral,
- recipient is the protocol vault/router or approved position account,
- slippage and price impact are within cap,
- health factor or collateralization remains above threshold for lending positions,
- no unsupported debt asset can be opened,
- no arbitrary approval to an unguarded spender,
- DEX prices use TWAP/oracle sanity checks, not spot-only valuation,
- post-transaction position value and withdrawability are checked,
- emergency unwind path exists before capital is allocated.

DEX policy:

- DEXes may be used for swaps, rebalancing, liquidation/unwind, and protocol-owned liquidity.
- DEX LP positions must be haircutted and cannot be first-line mint collateral until the specific asset guard is validated.
- DEX execution cannot bypass lending/strategy caps or per-chain liquidity reserves.

Lending policy:

- Same-chain deposited USDC can be supplied into same-chain lending protocols without first routing to Stellar, but only after the Stellar hub approves the chain, route, strategy id, guard set, and caps.
- Lending positions must remain withdrawable under the configured stress assumptions.
- Borrowing is disabled by default. If enabled later, contract guards must enforce supported debt assets, variable/fixed-rate policy, health factor floor, and post-transaction solvency.

## 8. Stellar Strategy Adapters

**Status (2026-09-02):** Blend V2's backstop (the Comet AMM BLND-USDC pool) was exploited
on 2026-08-22 and cannot be repaired; Blend V2 is being wound down protocol-wide, and the
Stellar Community Fund has withdrawn Blend from its Integration Track. `blend-adapter`
(§8.1) is **not an active integration** — its code is retained (it is generic across
Blend pool versions and nothing is deployed against it) purely so a future, independently
audited Blend V3 can be evaluated later without a rewrite. It must not be registered with
`AllocationManager` against a live Blend V1/V2 pool. The active strategy adapters are
`xycloans-adapter` (§8.2) and `defindex-adapter` (§8.3), chosen specifically because
neither depends on a price oracle or an undercollateralized-borrow/liquidation surface —
the two failure modes behind both Stellar DeFi incidents in 2026 (the YieldBlox Reflector
oracle manipulation in February, and the Comet accounting-bug exploit in August, which
used a flash loan sourced from a Blend pool to drain Comet's BLND-USDC reserve).

### 8.1 Blend Adapter — retained, not active (see status note above)

Purpose:

- Deploy Stellar USDC into approved Blend lending pools.
- Track bToken/shares or Blend position accounting.
- Report withdrawable and total value.

State:

```rust
struct BlendConfig {
    blend_pool: Address,
    usdc_sac: Address,
    collateral_asset_id: BytesN<32>,
    max_pool_utilization_bps: u32,
    max_protocol_exposure_6: i128,
    min_withdraw_liquidity_6: i128,
    oracle: Address,
    max_oracle_staleness_ledgers: u32,     // reject oracle data older than this
    oracle_price_deviation_bps: u32,       // circuit break if price deviates from USDC par (1.0) by more
    min_oracle_sources: u32,               // minimum Reflector quorum (0 = no quorum check)
    paused: bool,
}
```

Valuation:

```text
value_usdc_6 = floor(adapter_claim_on_blend_usdc_7 / 10)
```

Risk checks:

- pool is allowlisted,
- asset is native Stellar USDC,
- utilization under cap,
- oracle freshness: `current_ledger - oracle.last_update_ledger <= max_oracle_staleness_ledgers`,
- oracle price deviation: `abs(oracle_price_usdc - 1_000_000) <= oracle_price_deviation_bps * 10`,
- oracle quorum: if `min_oracle_sources > 0`, require at least that many reporting sources,
- pool not paused,
- adapter not exceeding exposure.

Oracle circuit breaker:
- If any oracle check fails, `value_usdc_6` MUST return
  `min(claimable_underlying_conservative, withdrawal_liquidity_adjusted_value)` WITHOUT the oracle
  term, effectively using the more conservative of on-chain claim and withdrawal quote.
- New deposits into the Blend adapter are blocked when the oracle circuit breaker is active.
- Emit `OracleCircuitBreakerTriggered(pool, reason, staleness_or_deviation)` when triggered.

Withdraw:

- Prefer exact amount withdraw.
- If Blend liquidity is insufficient, return partial only if caller explicitly requested emergency mode.
- Normal withdraw must either satisfy `min_out` or trap.

### 8.2 xycLoans Adapter (active — implemented in `xycloans-adapter`)

Purpose:

- Deploy Stellar USDC into an xycLoans flash-loan liquidity pool
  ([github.com/xycloo/xycloans](https://github.com/xycloo/xycloans)).
- Report a live, exact valuation — xycLoans mints shares 1:1 with deposits (no
  bToken/exchange-rate approximation), so unlike Blend this adapter never needs a
  conservative valuation fallback for its principal component.

Why this shape of protocol: xycLoans is flash-loan-only. A borrower must repay principal
plus fee within the same transaction or the entire transaction reverts — there is no
concept of an open-duration, collateralized borrow position. This structurally rules out
bad debt and price-oracle manipulation as attack surfaces, at the cost of yield being
flash-loan fee income (smaller, choppier) rather than term-loan interest.

State:

```rust
struct XycloansConfig {
    pool: Address,
    usdc_token: Address,
    asset_decimals: u32,               // 7 for Stellar SAC USDC
    max_protocol_exposure_6: i128,
    paused: bool,
}
```

Accounting model: xycLoans tracks principal (`shares`, 1:1 with deposited underlying) and
accrued-but-unclaimed fee income (`matured`) as two **separate** balances — depositing
does not compound yield into share value. `matured` is a snapshot that only advances when
`update_fee_rewards` is called for an address; it is not continuously accruing in storage.

Valuation:

```text
value_usdc_6 = floor((pool.shares(adapter) + pool.matured(adapter)) / 10)
```

Because `matured` can be stale until harvested, this can only ever **under-report** real
value, never over-report it — the same safe direction as Blend's V1 conservative
fallback (§8.1).

Risk checks:

- pool is the governance-registered pool for this adapter (no discovery/upgrade path),
- adapter not exceeding `max_protocol_exposure_6` (checked against principal only, not
  including unrealized matured fees),
- adapter paused flag blocks new deposits only — withdrawals always remain available.

No oracle, no liquidation, no utilization cap — none of those concepts exist for this
protocol, which is precisely why it was chosen as a lower-risk primitive.

Withdraw:

- Every withdraw path (partial, full, or emergency) first calls `update_fee_rewards` and
  then `withdraw_matured` (if any matured fees are present) before withdrawing principal
  — a partial withdraw can therefore legitimately return more than the requested amount
  if matured fees ride along; this is intentional and mirrors the "measure the real
  balance delta, never trust the requested figure" rule used for CCTP settlement and the
  Blend adapter elsewhere in this spec.
- A full exit (`withdraw_all` / `emergency_exit`) claims both matured fees and all
  remaining principal in one call.
- The realized amount is always the token balance delta measured immediately around the
  pool call, never the pool's return value or the requested amount.

### 8.3 deFindex Adapter (active — implemented in `defindex-adapter`)

Purpose:

- Deploy Stellar USDC into a single-asset [deFindex](https://github.com/defindex-io/stellar-contracts)
  vault and report a live valuation.
- deFindex is a multi-strategy vault aggregator, not a lending pool itself: a vault
  routes deposits into whichever strategy contracts it is configured with (their public
  strategy set includes Blend, xycLoans, Soroswap LP, and non-market strategies). This
  adapter only talks to the vault's own share-token interface, so it is agnostic to
  which strategy(ies) a given vault routes to internally — **governance is responsible
  for confirming out-of-band, before registering a vault address, that it does not route
  to Blend.** The adapter itself has no way to inspect a vault's internal strategy
  configuration.

State:

```rust
struct DefindexConfig {
    vault: Address,                    // a single-asset deFindex vault
    usdc_token: Address,               // the vault's sole configured asset
    asset_decimals: u32,               // 7 for Stellar SAC USDC (also the vault
                                        // share token's own decimals)
    max_protocol_exposure_6: i128,
    paused: bool,
}
```

Accounting model: a deFindex vault is itself an SEP-41 fungible token representing vault
shares — this adapter reads its own share balance via a plain token `balance` call, not a
bespoke accessor. Share price (`get_asset_amounts_per_shares`) moves with whatever the
vault's underlying strategy(ies) report, including gains or losses.

Valuation:

```text
value_usdc_6 = floor(vault.get_asset_amounts_per_shares(adapter_share_balance)[0] / 10)
```

Risk checks:

- vault is the governance-registered single-asset vault for this adapter,
- adapter not exceeding `max_protocol_exposure_6` (checked against deployed principal,
  tracked locally by this adapter — not the live, yield-inclusive position value),
- adapter paused flag blocks new deposits only — withdrawals always remain available.

Deposits always pass `invest = false` to the vault: whether and when to deploy idle vault
funds into the vault's underlying strategies is the vault's own manager/rebalancer's
decision, not this adapter's, since a mere depositor should not be able to force a
vault-level investment action.

Withdraw:

- Because deFindex's `withdraw` takes a **share** amount, not an underlying-asset amount,
  a partial withdraw for `amount_6` first computes the proportional share count via the
  vault's own current share price, **flooring** the result — this always rounds in favor
  of the vault's other depositors, never the withdrawer, so a request may legitimately
  return slightly less than asked (bounded by unit-level rounding dust); `min_out_6`
  protects the caller against anything worse than that.
- The realized amount is always the token balance delta measured immediately around the
  vault call, never the vault's return value or the requested amount.

### 8.4 Aquarius Adapter

Purpose:

- Maintain protocol-owned liquidity for peg support if Aquarius is used.

Recommendation:

- Do not use AMM LP as first-line collateral backing for fUSD mints.
- Treat Aquarius liquidity as protocol-owned peg liquidity funded by retained yield or treasury allocation.
- Keep LP valuation haircutted because LP includes inventory and price risk.

Required haircuts:

```text
lp_value_for_backing = min(oracle_value, withdrawal_quote) * haircut_bps / 10000
```

Default: do not count Aquarius LP toward primary redemption backing until validated.

### 8.5 Known Tradeoffs and Audit Notes (2026-09-02)

A self-audit of the strategy-allocation layer (`allocation-manager`, `blend-adapter`,
`defindex-adapter`, `xycloans-adapter`, plus the `vault-accounting`/
`mint-redeem-controller` changes that support it) found and fixed several real
correctness/security bugs — see the "Key security properties demonstrated" §1 and §9
entries in `../README.md` for the two most severe (a fully permissionless mint path, and
a broken `emergency_exit` caller relay). This subsection records residual, deliberate
trade-offs that were reviewed and documented rather than code-patched:

- **The three adapters (`blend-adapter`, `defindex-adapter`, `xycloans-adapter`) are
  independently self-contained rather than sharing a common crate**, even though their
  `initialize`/`set_paused`/`set_max_exposure`/`sweep`/config/auth-helper scaffolding is
  near-identical. This is a deliberate choice, not an oversight: extracting a shared
  `strategy-adapter-common` crate this late would touch security-critical,
  money-moving code in all three at once for a maintenance-cost improvement, trading a
  known, already-tested risk profile for an unknown one. Isolation also means a bug
  introduced while adding a fourth adapter (or patching a shared helper) cannot
  simultaneously compromise integrations that were already live and audited
  independently. Revisit if a fourth adapter is added and the duplication cost
  clearly outweighs this isolation benefit.
- **`AllocationManager.allocate` hardcodes a 7-decimal USDC conversion**
  (`amount_6 * 10`) rather than deriving it from each adapter's own configured
  `asset_decimals`. This is safe today — every registered adapter's config asserts
  `asset_decimals >= 6` and all three shipped adapters are configured for 7 (Stellar
  SAC USDC) — but it is a latent assumption, not an enforced invariant. Registering a
  future adapter configured for a different-decimals token would desynchronize real
  token movement from `VaultAccounting`'s 6-decimal books. `register_strategy` does
  cross-check that an adapter's `asset()` matches the hub's actual USDC SAC address
  (added in this audit pass), which catches a wrong-token misconfiguration but not a
  correct-token-wrong-decimals one.
- **`VaultAccounting.report_strategy_value` is not bounded by a strategy's
  `debt_ceiling_6`**, by design — the ceiling caps how much new principal
  `move_idle_to_strategy` may *deploy*, not how large a position's *value* may grow from
  real, externally-verified yield. See the inline doc comment on that function.
- **`MintRedeemController` and `VaultAccounting` each store their own independent
  `Allocator` address** (set via separate admin calls) rather than one shared
  configuration. In the intended usage pattern — `AllocationManager.allocate()` calling
  both atomically in the same transaction — a mismatch just reverts the whole
  transaction rather than silently desyncing accounting, so this is safe in practice but
  worth a governance runbook note: misconfiguring only one of the two `set_allocator`
  calls after a redeploy bricks allocation rather than failing loudly at config time.

### 8.6 Live Testnet Verification (2026-09-02)

`defindex-adapter`'s hand-written `defindex_vault.rs` interface (§8.3) was verified
against a real, currently-live deFindex vault on Stellar testnet — not just read from
source, actually deployed and called on-chain:

- **Interface diff.** `stellar contract info interface --id
  CBMVK2JK6NTOT2O4HNQAIQFJY232BHKGLIMXDVQVHIIZKDACXDFZDWHN --network testnet` pulled the
  real, deployed contract's spec directly from the ledger. Every function this adapter
  calls (`deposit`, `withdraw`, `fetch_total_managed_funds`,
  `get_asset_amounts_per_shares`) and every struct it decodes
  (`AssetInvestmentAllocation`, `CurrentAssetInvestmentAllocation`, `StrategyAllocation`)
  matched field-for-field. (Parameter *names* differ in one place — the real contract
  calls its withdraw-shares argument `withdraw_shares`, this adapter's interface calls
  it `df_amount` — which does not affect on-chain compatibility: Soroban dispatches
  cross-contract calls positionally by name+type, not by parameter name.)
- **Real deployment.** `defindex-adapter` and `xycloans-adapter` were both deployed to
  Stellar testnet from this repo's actual build output and initialized (`defindex-adapter`
  pointed at the vault above). `defindex-adapter.asset()` and `.value_usdc_6()` were
  called live, exercising real cross-contract calls into the vault
  (`token::Client(vault).balance(this)`, `get_asset_amounts_per_shares`), and returned
  correct results (`0`, since the freshly-initialized adapter holds no shares yet).
  `get_asset_amounts_per_shares` was also called directly against the vault to confirm
  the `Vec<i128>` return type decodes correctly.
- **What this does NOT cover:** a full `deposit`/`withdraw` round trip through the
  adapter. That vault's configured asset is a specific testnet USDC SAC controlled by a
  third-party issuer this session has no mint authority over, and there is no public
  faucet for it; synthesizing a brand-new test vault with a self-controlled asset would
  require also deploying a deFindex-compatible strategy contract, which is out of scope
  for verifying this adapter specifically. The share-accounting math this exercises
  (`deposit`/`withdraw`'s balance-delta handling, proportional-redemption rounding) is
  covered by `defindex-adapter`'s own mock-based unit test suite instead (§ README
  "Running Soroban tests").
- **Real bug found and fixed by this exercise:** the workspace's build target
  (`wasm32-unknown-unknown`, with or without the `.cargo/config.toml`
  `-reference-types` workaround) produced a WASM module the Soroban testnet host
  rejected at deploy time — invisible to `cargo test`, which runs natively rather than
  through the wasm host, so 129 passing unit tests gave no signal either way. Fixed by
  switching the whole workspace to the `wasm32v1-none` target (see `docs/POC_GUIDE.md`
  "Build reproducibility"). This is exactly the kind of gap that only surfaces by
  actually deploying, which is why this verification pass was worth doing beyond the
  existing mock-based test suites.

## 9. Remote Chain Contracts

### 9.1 EVM `RemoteRouter`

Purpose:

- Accept native USDC on remote chains.
- Initiate CCTP burn to Stellar.
- Send/receive Axelar GMP messages.
- Mint or burn remote fUSD representation if enabled.
- Hold or route same-chain native USDC to guarded local strategy executors after Stellar hub allocation approval.

Interfaces:

```solidity
interface IRemoteRouter {
    function depositAndBridge(
        uint256 amount,
        uint32 destinationFusdChainId,
        bytes32 destinationFusdRecipient,
        uint256 maxFee,
        uint32 minFinalityThreshold,
        bytes calldata hookData
    ) external;

    function depositLocalAndRequestMint(
        uint256 amount,
        uint32 destinationFusdChainId,
        bytes32 destinationFusdRecipient,
        bytes32 routeId,
        uint64 userNonce,
        uint64 expiry,
        bytes calldata hookData
    ) external;

    function burnRemoteFusdAndRedeem(
        uint256 amount,
        uint32 destinationDomain,
        bytes32 usdcRecipient,
        uint256 maxFee
    ) external;

    function execute(
        bytes32 commandId,
        string calldata sourceChain,
        string calldata sourceAddress,
        bytes calldata payload
    ) external;
}
```

State:

```solidity
struct RemoteConfig {
    address usdc;
    address tokenMessengerV2;
    address messageTransmitterV2;
    address axelarGateway;
    address gasService;
    address remoteFusd;
    address spokeVault;
    address guardedExecutor;
    address guardRegistry;
    string stellarChainName;
    string stellarControllerAddress;
    uint32 localChainId;
    uint32 localCctpDomain;
    uint256 mintCap;
    uint256 minted;
    uint256 pendingMintAuthorizations;
    uint256 localCollateralCap;
    uint256 localCollateralLocked;
    bool depositsPaused;
    bool redeemsPaused;
    bool remoteMintPaused;
    bool localCollateralEnabled;
    bool localStrategiesPaused;
}
```

Remote deposit:

```text
user -> approve USDC
user -> RemoteRouter.depositAndBridge
router -> transferFrom USDC
router -> approve TokenMessengerV2
router -> depositForBurnWithHook(destinationDomain=27, mintRecipient=CctpForwarder/protocol, ...)
router -> emit DepositInitiated
optional -> Axelar send deposit intent metadata to Stellar with desired fUSD destination chain/recipient
```

Remote local deposit:

```text
user -> approve USDC
user -> RemoteRouter.depositLocalAndRequestMint
router -> transferFrom native USDC
router -> transfer USDC to protocol SpokeVault before any message is sent
router -> checks localCollateralLocked + amount <= localCollateralCap
router -> emits SpokeCollateralLocked locally
router -> sends canonical SpokeCollateralLocked GMP payload to Stellar
hub -> verifies router, vault, token, route, cap, nonce, and amount
hub -> records accepted spoke escrow and creates mint allowance
hub -> consumes mint allowance for local Stellar mint or bounded remote mint authorization
```

Important:

- Remote router must not mint remote fUSD until Stellar confirms canonical settlement.
- Remote mint must be controlled by Stellar hub authorization and custom remote routers in production v1.
- Remote router must not deploy USDC into local lending or DEX venues unless the Stellar hub has approved the `AllocationRoute` and the local guard registry approves the exact target/calldata.
- Remote router must not claim local spoke collateral unless native USDC has already been transferred into the approved `SpokeVault`.
- Local spoke collateral lock messages must be replay-protected and capped separately from CCTP settlement.

### 9.1A EVM `GuardedStrategyExecutor`

Purpose:

- Execute same-chain USDC lending, DEX swap, LP, unwind, and harvest actions.
- Mirror the Base `PoolLogic.execTransaction()` / `PoolTxExecutor` guard pattern.
- Prevent remote routers, operators, or strategy adapters from calling arbitrary external contracts.

Interfaces:

```solidity
interface IGuardedStrategyExecutor {
    // routeVersion: caller commits to the specific AllocationRoute version. The executor
    // MUST reject if the on-chain route version does not match. This binds the idempotency
    // key to (routeId, routeVersion, routeNonce) and prevents stale-route execution.
    function executeGuarded(
        bytes32 routeId,
        uint32 routeVersion,
        bytes32 strategyId,
        address target,
        bytes calldata data,
        uint256 value,
        uint256 minOutOrHealthFloor,
        uint64 routeNonce
    ) external returns (bytes memory result);

    function emergencyWithdrawGuarded(
        bytes32 routeId,
        bytes32 strategyId,
        address assetOrPosition,
        uint256 amount,
        uint256 minOut,
        bytes calldata withdrawData
    ) external;
}
```

Execution rule:

```text
executor receives hub-approved allocation payload
  -> loads contract guard for target
  -> contract_guard.txGuard(vault, target, calldata)
  -> executes target call
  -> contract_guard.afterTxGuard(vault, target, calldata)
  -> loads asset guard for resulting asset/position
  -> asset_guard.getBalance/value confirms expected position
  -> emits GuardedStrategyExecuted(route_id, strategy_id, target, position, amount_6)
```

Required EVM guards for first guarded-spoke release:

- `ERC20Guard` for approvals and direct USDC custody.
- `AaveLendingPoolGuardV3` + `AaveV3LendingPoolAssetGuard` for Aave supply/withdraw and health checks.
- `MorphoBlueContractGuard` + `MorphoBlueAssetGuard` for Morpho supply/withdraw and market validation.
- `UniswapV3RouterGuard`, `UniswapV3NonfungiblePositionGuard`, and `UniswapV3AssetGuard` for swaps, LP accounting, and TWAP-based valuation.

DEX and lending calls must be impossible without both:

- Stellar hub route/cap approval, and
- chain-local contract/asset guard approval.

### 9.2 Remote fUSD Representation

fUSD is a hub-authorized omnichain stablecoin. Users may receive fUSD on any activated supported chain. Stellar remains the hub, but the user-facing mint can execute on Stellar, Base, Ethereum, Arbitrum, OP Mainnet, Solana, or any future activated spoke.

- Stellar is hub/canonical.
- Remote fUSD is minted/burned on connected chains.
- Remote supply is mirrored in Stellar accounting.
- Production v1 uses custom hub-authorized remote routers for EVM and Solana.
- Axelar GMP carries authenticated messages; Axelar ITS direct token movement is out of scope until separate design validation proves hub-accounted transfer semantics.
- All remote fUSD movement must be hub-routed or hub-authorized. Direct spoke-to-spoke transfers are forbidden unless the hub is part of the atomic accounting path.

Recommendation:

- MVP can mint fUSD on Stellar plus one validated EVM spoke, with the same hub authorization flow used for all later spokes.
- Production enables fUSD mint/redeem on every activated spoke after accounting, replay protection, remote pause, and rate limits are validated.
- A chain that has CCTP but has not completed remote fUSD validation can still be a USDC source/destination route, but cannot mint local fUSD until activated.

Remote fUSD rules:

- Remote minted amount must be included in canonical liabilities.
- Remote burn must be acknowledged before canonical liability is reduced or redemption is processed.
- Remote chain caps are enforced on Stellar and locally.
- Remote token contracts must have a guardian pause.
- Every remote mint authorization has a unique `mint_auth_id` derived from the full canonical payload below.
- Remote routers must reject expired or replayed mint authorizations.
- Remote routers must emit `RemoteMintExecuted(mint_auth_id, recipient, amount_6)`.
- The remote router must send an authenticated `RemoteMintExecuted` GMP acknowledgement to Stellar. The indexer can display status but is never an accounting input.
- The hub reconciles `pending_mint_auth_6` to `outstanding_supply_6` only after it verifies the authenticated remote execution acknowledgement.
- If destination execution never happens, governance can expire the authorization and release `pending_mint_auth_6` without minting supply.

Remote supply representation:

```rust
struct RemoteSupplyPosition {
    chain_id: u32,
    representation_type: Symbol, // "erc20", "spl", "token2022", "its"
    token_address: BytesN<32>,
    mint_authority: BytesN<32>,
    burn_authority: BytesN<32>,
    outstanding_supply_6: i128,
    pending_mint_auth_6: i128,
    pending_burn_acceptance_6: i128,
    last_observed_nonce: u64,
    paused: bool,
}
```

Remote mint authorization:

```solidity
struct RemoteMintAuthorization {
    uint32 protocolVersion;
    uint32 hubChainId;
    bytes32 hubController;
    uint32 sourceChainId;
    uint32 sourceCctpDomain;
    bytes32 sourceRouter;
    bytes32 settlementHash;
    uint256 settlementAmount6;
    uint32 destinationChainId;
    bytes32 destinationRouter;
    bytes32 destinationFusdToken;
    bytes32 destinationRecipient;
    bytes32 routeId;
    bytes32 mintAuthId;
    uint64 hubNonce;
    uint32 finalityThreshold;
    uint64 expiryLedgerOrTimestamp;
}
```

`mintAuthId` is:

```text
mint_auth_id = hash("FUSD_REMOTE_MINT_AUTH_V1" || canonical_scale_encoded(RemoteMintAuthorization without mintAuthId))
```

Every runtime must use the same field order, integer size, byte encoding, and domain separator in tests.

Execution rule:

```text
remote router receives RemoteMintAuthorize from Stellar hub
  -> verifies Axelar gateway/source address and configured GMP route
  -> checks mintAuthId equals canonical payload hash and is unused
  -> checks payload not expired
  -> checks source chain, source CCTP domain, source router, route id, finality threshold, and settlement amount
  -> checks destinationChainId == localChainId
  -> checks destinationRouter == this router
  -> checks destinationFusdToken == configured fUSD token/mint
  -> checks amount within local cap
  -> mints local fUSD to destinationRecipient
  -> emits RemoteMintExecuted
  -> sends authenticated RemoteMintExecuted GMP acknowledgement to Stellar
  -> Stellar verifies acknowledgement and moves pending_mint_auth_6 to outstanding_supply_6
```

### 9.3 Solana `remote_router` Program

Purpose:

- Accept native Solana USDC from users.
- Initiate CCTP burn to Stellar.
- Receive authenticated remote-mint authorizations if remote Solana fUSD is enabled.
- Burn Solana fUSD and request redemption through Stellar.
- Enforce local caps, pause flags, replay protection, and PDA authority boundaries.

Solana must not be modeled as an EVM chain. The implementation is account-driven and every instruction must validate all accounts explicitly.

Program-derived authorities:

```rust
// PDA seeds are illustrative. Final seeds must be versioned and documented.
router_authority = PDA("frgmnt_router", config.chain_id)
usdc_vault_authority = PDA("frgmnt_usdc_vault", config.chain_id)
fusd_mint_authority = PDA("frgmnt_fusd_mint", config.chain_id)
message_authority = PDA("frgmnt_message", source_domain, nonce)
```

Core accounts:

```rust
#[account]
pub struct SolanaRouterConfig {
    pub admin: Pubkey,
    pub guardian: Pubkey,
    pub cctp_token_messenger: Pubkey,
    pub cctp_message_transmitter: Pubkey,
    pub usdc_mint: Pubkey,
    pub fusd_mint: Pubkey,
    pub stellar_controller: [u8; 32],
    pub stellar_cctp_domain: u32,
    pub local_cctp_domain: u32,
    pub mint_cap_6: u64,
    pub minted_6: u64,
    pub daily_mint_limit_6: u64,
    pub deposits_paused: bool,
    pub redeems_paused: bool,
    pub remote_mint_enabled: bool,
    pub bump: u8,
}

#[account]
pub struct ConsumedMessage {
    pub message_id: [u8; 32],
    pub source_domain: u32,
    pub amount_6: u64,
    pub consumed_at_slot: u64,
}

#[account]
pub struct PendingDeposit {
    pub id: [u8; 32],
    pub user: Pubkey,
    pub amount_6: u64,
    pub destination_fusd_chain_id: u32,
    pub destination_fusd_recipient: [u8; 32],
    pub destination_recipient_type: u8,
    pub route_id: [u8; 32],
    pub cctp_nonce: [u8; 32],
    pub user_nonce: u64,
    pub expiry_slot: u64,
    pub created_slot: u64,
    pub status: u8,
}
```

Instructions:

```rust
pub enum SolanaRouterInstruction {
    Initialize {
        stellar_controller: [u8; 32],
        stellar_cctp_domain: u32,
        local_cctp_domain: u32,
        mint_cap_6: u64,
        daily_mint_limit_6: u64,
    },
    SetPause {
        deposits_paused: bool,
        redeems_paused: bool,
    },
    DepositAndBurnUsdc {
        amount_6: u64,
        destination_fusd_chain_id: u32,
        destination_fusd_recipient: [u8; 32],
        destination_recipient_type: u8,
        route_id: [u8; 32],
        user_nonce: u64,
        expiry_slot: u64,
        max_fee_6: u64,
        min_finality_threshold: u32,
    },
    AuthorizeRemoteMint {
        mint_auth_id: [u8; 32],
        payload_hash: [u8; 32],
        payload: Vec<u8>,
    },
    BurnRemoteFusdAndRedeem {
        amount_6: u64,
        destination_domain: u32,
        usdc_recipient: [u8; 32],
        max_fee_6: u64,
    },
    ConsumeGmpMessage {
        source_chain_hash: [u8; 32],
        source_address_hash: [u8; 32],
        nonce: u64,
        payload_hash: [u8; 32],
    },
}
```

Validation rules:

- The user's USDC token account mint must equal configured native Solana USDC.
- All token transfers use SPL Token or Token-2022 CPI with explicit account validation.
- `DepositAndBurnUsdc` must transfer USDC from the user to the router PDA and call Circle's Solana CCTP burn flow.
- `DepositAndBurnUsdc` must commit destination fUSD chain, destination recipient, recipient type, route id, user nonce, and expiry slot into the pending deposit and CCTP/GMP metadata.
- Remote fUSD minting is disabled for a Solana spoke until hub authorization, replay protection, cap checks, pause behavior, and acknowledgement reconciliation are validated for that program id.
- `AuthorizeRemoteMint` can be executed only from an authenticated Stellar-origin message path.
- Every `mint_auth_id` and GMP message id must create a `ConsumedMessage` account and cannot be reused.
- The canonical `RemoteMintAuthorization` payload must match configured Solana router PDA, fUSD mint, destination chain id, route id, amount, expiry, and recipient before minting.
- `minted_6 + amount_6 <= mint_cap_6`.
- Pause flags block deposits and remote mints, but not emergency burns.
- Program upgrade authority must be controlled by a multisig/timelock and separated from guardian pause.

Solana CCTP route:

```text
User wallet
  -> Solana remote_router.deposit_and_burn_usdc
  -> router transfers native USDC from user's token account
  -> router invokes Circle CCTP burn to Stellar domain 27
  -> Circle attestation is delivered on Stellar
  -> Stellar MintRedeemController records settlement
  -> fUSD minted on Stellar or remote Solana mint authorization is sent
```

Solana remote strategy note:

- Solana is not deactivated. Solana mint/redeem and CCTP routing remain part of the cross-chain design for activated Solana spokes.
- Solana lending strategies should not be enabled in the first cross-chain release until the Solana-specific instruction guard and asset guard validation is complete.
- When enabled, they need a separate `solana_strategy_router` program and a Stellar-side remote strategy valuation adapter.
- Strategy value must be haircutted and freshness-limited before counting toward backing.
- The Solana strategy router must use instruction guards and asset guards equivalent to the Base contract/asset guard model.
- Solana DEX and lending protocols must be enabled one market/program at a time through Stellar governance and Solana guard registry updates.

Future Solana guarded execution:

```rust
pub enum SolanaStrategyInstruction {
    ExecuteGuarded {
        route_id: [u8; 32],
        strategy_id: [u8; 32],
        target_program: Pubkey,
        instruction_data_hash: [u8; 32],
        amount_6: u64,
        min_out_or_health_floor: u64,
        route_nonce: u64,
    },
    EmergencyWithdrawGuarded {
        route_id: [u8; 32],
        strategy_id: [u8; 32],
        position_id: [u8; 32],
        amount_6: u64,
        min_out_6: u64,
    },
}
```

Solana guard requirements:

- `instruction_guard.pre_validate` checks target program, accounts, token mints, owner PDAs, route id, nonce, amount, and allowlisted instruction discriminator.
- `instruction_guard.post_validate` checks resulting balances, health factor/collateralization if applicable, and no unexpected token accounts gained authority.
- `asset_guard.value_position` reports only independently verifiable balances and withdraw quotes.
- `asset_guard.withdraw_instructions` must exist before any position can receive capital.

## 10. CCTP Integration Specification

### 10.1 Domains

Store CCTP domain ids in `ChainState`.

```rust
struct CctpDomain {
    domain: u32,
    name: Symbol,
    usdc_decimals: u32,
    active: bool,
}
```

Known:

- Stellar domain: `27` (stored in `GlobalState.hub_cctp_domain`, NOT hardcoded in contract logic).

Other domains must be loaded from Circle's official registry at deployment time.

Implementation rule: All inbound CCTP message validation must check
`message.destination_domain == global_state.hub_cctp_domain`. Contract code must read this
value from storage; the literal `27` must not appear as a constant in validation logic.
`hub_cctp_domain` is changeable only by governance in case of Circle domain ID changes.

### 10.2 Amount Precision

CCTP message amount is six-decimal USDC.

Stellar USDC via SAC is seven-decimal.

Conversion:

```rust
fn usdc7_to_usdc6_floor(amount_7: i128) -> i128 {
    amount_7 / 10
}

fn usdc6_to_usdc7(amount_6: i128) -> i128 {
    amount_6 * 10
}
```

Dust policy:

- For Stellar -> remote CCTP sends, only burn/send six-decimal-compatible amount.
- Seventh-decimal dust is returned to the user immediately whenever possible.
- If immediate return is impossible, dust is recorded as `protocol_dust_usdc_7`, emitted in `DustRetained(user, amount_7, context)`, excluded from `USDC_6` mint allowance, and sweepable only by timelocked governance to treasury.
- Returning dust emits `DustReturned(user, amount_7, context)`.
- All fUSD liabilities are minted in 6-decimal fUSD units and reconciled to `USDC_6`.

### 10.2A CCTP Fee Treatment

CCTP v2 charges a variable relay fee. The amount field in the Circle attestation may represent
the gross burn amount before fees are deducted, depending on the CCTP version and fee model.

Implementation rule:

- The protocol must credit `mint_allowance_6` only for the net USDC physically received in the
  protocol-controlled address, not the gross amount in the CCTP message.
- Before calling `accounting.record_inbound_settlement()`, the controller MUST compute the
  balance delta:
```text
net_received_6 = usdc_sac.balance(protocol_address, after)
               - usdc_sac.balance(protocol_address, before)
```
  where `before` and `after` bracket the CCTP receive/forward call in the same transaction.
- `mint_allowance_6` increases by `net_received_6`, NOT by `message.amount_6`.
- If `net_received_6 < message.amount_6` (fees deducted), record:
```text
protocol_cctp_fee_usdc_6 += (message.amount_6 - net_received_6)
emit CctpFeeObserved(message_hash, gross_6 = message.amount_6, net_6 = net_received_6, fee_6)
```
- Relayers must never pass the CCTP message amount as a user-controlled parameter that could
  override the balance-delta verification.
- This requirement applies to both finalized and fast-finality CCTP paths.

### 10.3 Inbound To Stellar

Preferred production path:

1. Remote router burns USDC via CCTP with destination domain `27`.
2. `mintRecipient` is either:
   - protocol CCTP receiver contract, if it directly implements the receive path, or
   - `CctpForwarder`, with hook forwarding to the protocol deposit receiver.
3. Off-chain relayer fetches Circle Iris attestation.
4. Relayer calls Stellar CCTP receive/forward method.
5. USDC lands in protocol-controlled address.
6. Controller records finalized settlement and either mints fUSD locally or emits a bounded remote mint authorization.

Security checks:

- source domain is enabled,
- source sender/router is allowlisted,
- nonce/message hash not consumed,
- amount > 0,
- destination recipient is this protocol,
- finality threshold policy satisfied,
- minted fUSD amount does not exceed caps.
- finalized settlement increases `mint_allowance_6` by `net_received_6` (the balance-delta measured in the settlement transaction) as defined in §10.2A — NOT by the parsed CCTP message amount,
- fast/confirmed settlement can only increase `pending_fast_credit_6` and never `mint_allowance_6` unless the amount is covered by explicit fast-credit insurance reserve and governance-enabled caps,
- local or remote fUSD minting consumes `mint_allowance_6`,
- strategy reports cannot be netted against failed or missing CCTP settlements.

### 10.4 Spoke-Local Collateral Settlement

Spoke-local collateral settlement is the required path when USDC stays on the source chain for same-chain investment instead of being burned through CCTP to Stellar.

This is not a bridge substitute. It is a vault-custody proof path:

```text
user deposits native USDC into approved remote router
  -> router transfers native USDC into approved protocol SpokeVault
  -> router emits local lock event
  -> router sends canonical SpokeCollateralLocked GMP payload to Stellar
  -> Stellar validates source router, vault, native USDC token, route id, guard set, amount, cap, and nonce
  -> Stellar records settled_spoke_escrow_usdc_6
  -> Stellar increases mint_allowance_6
  -> fUSD mints on the requested activated destination chain by normal hub authorization
```

Hub validation:

- source chain is active,
- local collateral is enabled for the chain,
- source router equals `ChainState.remote_router`,
- source vault equals `ChainState.remote_vault`,
- USDC token/mint equals the configured native USDC asset for that chain,
- route id exists, is active, and has `route_kind == "local"` or another explicitly approved local-collateral route type,
- guard set id is active for that chain,
- lock id and GMP message id are unused,
- amount is positive and six-decimal normalized,
- Cap check uses hub's own canonical accounting (NOT the self-reported payload field):
  `settled_spoke_escrow_usdc_6[chain] + amount_6 <= local_collateral_cap_6[chain]`,
- route and daily movement limits are not exceeded,
- mint allowance and liabilities remain within chain and global caps.

IMPORTANT: `vault_balance_after_6` in `SpokeCollateralLockedPayload` is self-reported by the spoke
router and must NOT be used for cap enforcement. It is used for monitoring and event emission only.
The hub enforces caps using `settled_spoke_escrow_usdc_6[chain]` from its own canonical state.

Required invariant:

```text
settled_spoke_escrow_usdc_6[chain]
  = idle_spoke_vault_usdc_6[chain]
  + accepted_guarded_strategy_value_6[chain]
  + reserved_redemption_6[chain]
  - pending_release_6[chain]

settled_spoke_escrow_usdc_6[chain] <= local_collateral_cap_6[chain]
```

After the lock is accepted, a Trader or AllocationOperator may deploy that USDC into same-chain guarded lending or DEX venues only through the approved route and guard set. That deployment moves value from idle spoke vault balance into accepted guarded strategy value; it must not create additional mint allowance.

Release path — two-phase protocol:

Phase 1 — Hub creates release:
- Hub accepts burn or allocation release from source (spoke burn GMP or allocation manager).
- Hub increments `pending_release_6[chain]` and decrements `idle_spoke_vault_usdc_6[chain]`
  or `accepted_guarded_strategy_value_6[chain]` accordingly.
- Hub records `ReleaseRequest{release_id, lock_id, amount_6, status: ReleaseSent, timeout_ledger}`.
- Hub sends `SpokeCollateralRelease` GMP with `release_id`, `amount_6`, and `release_timeout_ledger`.

Phase 2 — Spoke acknowledges:
- Remote router executes local USDC release (transfer to user or CCTP burn to destination).
- Remote router sends `SpokeCollateralReleased` GMP ack to hub with `release_id`.
- Hub receives ack, decrements `pending_release_6[chain]`, marks `ReleaseRequest.status = ReleaseComplete`.

Timeout and recovery (if Phase 2 ack never arrives):
- If `current_ledger > ReleaseRequest.timeout_ledger` and `status == ReleaseSent`:
  - Governance or the original user calls `retry_spoke_collateral_release(release_id)`.
  - Hub re-sends `SpokeCollateralRelease` GMP (idempotent: `release_id` is unique and
    the spoke router must reject duplicates it has already processed).
  - OR governance calls `rollback_stuck_release(release_id)` after N additional timeout periods.
  - Rollback restores `pending_release_6` back into `settled_spoke_escrow_usdc_6` without
    double-counting. `ReleaseRequest.status = RolledBack`.
  - The original burn or allocation release must also be rolled back or the user refunded.

```rust
struct ReleaseRequest {
    release_id: BytesN<32>,
    lock_id: BytesN<32>,
    chain_id: u32,
    amount_6: i128,
    destination_cctp_domain: u32,
    usdc_recipient: BytesN<32>,
    requester_chain_id: u32,    // chain_id of the entity that requested the release (for rollback refund routing)
    requester_address: BytesN<32>, // address of the requesting user or governance contract (for rollback refund)
    status: ReleaseStatus,
    initiated_ledger: u32,
    timeout_ledger: u32,
}

enum ReleaseStatus {
    ReleaseSent,
    ReleaseComplete,
    RolledBack,
    RetryPending,
}
```

Rollback refund rule: when `rollback_stuck_release` sets status to `RolledBack`, the hub MUST
also attempt to return USDC to `requester_address` on `requester_chain_id` via CCTP, or to a
governance-controlled recovery account if CCTP is unavailable for that chain.

Add `retry_spoke_collateral_release` and `rollback_stuck_release` to `VaultAccounting` trait.
`release_timeout_ledgers: u32` is stored in `GlobalState`.

Pending-mint-auth release guard:

A `SpokeCollateralReleased` GMP payload MUST be rejected by the hub unless the chain's pending
mint authorizations are fully resolved. Minimum conservative requirement (option 2):

```text
Hub MUST reject SpokeCollateralReleased for chain_id C if:
    pending_mint_auth_6[C] > 0

That is: there must be zero MintAuthRecords with status == Pending for the same chain_id
before the hub accepts the matching collateral release.
```

Rationale: a `SpokeCollateralLockedPayload` increases `mint_allowance_6`, from which
`RemoteMintAuthorize` messages are issued (creating Pending `MintAuthRecord`s backed by
that collateral). If the lock is released while a `MintAuthRecord` is still Pending, the
backing for the pending authorization disappears, enabling a spoke to execute a mint against
already-released collateral.

Option 1 (precise): track which `mint_auth_id`s were funded from which `lock_id` and require
all linked records to be in status Executed, Expired, or Cancelled before accepting the
matching `SpokeCollateralReleased`. This is preferred for implementations that support
per-lock linkage.

Option 2 (conservative, minimum requirement): block release if `pending_mint_auth_6[chain] > 0`
for the originating chain. This is the required minimum guard for any implementation.

Forbidden:

- backend/indexer lock observations as accounting input,
- a GMP collateral claim from a non-approved router,
- a lock payload where the vault address, USDC token, route id, guard set, or chain id does not match hub config,
- using strategy appreciation to increase spoke collateral lock amount,
- releasing local spoke USDC before the hub has accepted the matching burn/release state,
- accepting a `SpokeCollateralReleased` GMP while any Pending MintAuthRecord exists for the same chain_id (see pending-mint-auth release guard above).

### 10.5 Outbound From Stellar

Stellar burn redeem:

1. User burns fUSD on Stellar.
2. Controller reduces canonical liability.
3. Controller reserves USDC.
4. Controller calls Stellar `TokenMessengerMinter.deposit_for_burn` or `deposit_for_burn_with_hook`.
5. Store `RedeemRequest`.
6. Remote chain receives attested CCTP mint.
7. Authenticated destination acknowledgement marks request complete; indexer observation is display-only.

If the source fUSD is on a spoke, the hub must first accept a spoke burn:

```text
spoke burn executed
  -> RemoteBurnNotice GMP sent to Stellar
  -> hub validates source chain, router, burn id, amount, and nonce
  -> pending_burn_acceptance_6 is reduced
  -> canonical liability is reduced
  -> Stellar reserves/sends USDC through CCTP
  -> authenticated destination acknowledgement marks request complete
```

The hub must not release USDC for a spoke redemption from a UI request alone.
The hub must not reserve or send USDC for a spoke redemption until the remote burn id is accepted exactly once.

State:

```rust
struct RedeemRequest {
    id: BytesN<32>,
    user: Address,
    amount_6: i128,
    destination_domain: u32,
    destination_recipient: BytesN<32>,
    max_fee_6: i128,
    status: RedeemStatus,
    cctp_nonce: Option<BytesN<32>>,
    created_ledger: u32,
}

enum RedeemStatus {
    BurnPending,
    BurnAccepted,
    BurnAcceptedSendFailed,  // CCTP send failed after burn accepted; retry or remint required
    Reserved,
    Sent,
    Completed,
    Cancelled,
    RemintIssued,            // user chose remint recovery instead of CCTP retry
}
```

### 10.6 Finality Thresholds

Policy:

- Large transfers: finalized threshold.
- Default for all user mints: finalized threshold only.
- Small transfers may use confirmed/fast threshold only as an explicit premium feature funded by `fast_credit_insurance_reserve_6`.
- Protocol accounting must distinguish soft-finality pending transfers from finalized collateral.
- `mint_allowance_6` may increase only from finalized CCTP settlement, local final Stellar settlement, or accepted spoke-local vault lock from an approved remote router/vault.
- Fast/confirmed CCTP transfers create `pending_fast_credit_6`, never ordinary mint allowance.

Fast-credit caps:

```text
pending_fast_credit_6[source_chain] <= fast_credit_chain_cap_6[source_chain]
sum(pending_fast_credit_6) <= fast_credit_global_cap_6
pending_fast_credit_6[user] <= fast_credit_user_cap_6[user]
pending_fast_credit_6 <= fast_credit_insurance_reserve_6
```

If a fast-finality transfer later fails final settlement, losses are absorbed by the fast-credit insurance reserve before any protocol reserve or user-facing solvency metric is affected.

Reserve management:

```rust
fn deposit_fast_credit_reserve(e: Env, caller: Address, amount_6: i128);
fn withdraw_fast_credit_reserve(e: Env, gov: Address, amount_6: i128);

struct FastCreditReserveStatus {
    reserve_balance_6: i128,
    pending_fast_credit_6: i128,
    available_headroom_6: i128,
    is_active: bool,    // false = fast credit globally paused (reserve below min_headroom)
}
fn fast_credit_reserve_status(e: Env) -> FastCreditReserveStatus;
```

Rules:
- Fast-credit issuance is automatically paused when `reserve_balance_6 - pending_fast_credit_6 < fast_credit_min_headroom_6`.
- It resumes automatically when headroom is restored.
- `deposit_fast_credit_reserve` can be called by anyone (governance, treasury, or voluntary contributors).
- `withdraw_fast_credit_reserve` requires governance auth and is subject to 48h timelock.
- `fast_credit_min_headroom_6` is governance-configurable; default is 10% of `fast_credit_global_cap_6`.

Fast-credit finalization path:

When a CCTP message that was previously credited as fast-finality receives FINALIZED attestation:

```text
1. Check that cctp_message_hash is in the ConsumedCctpMessages set (it was already consumed
   as fast-finality). If so, this is a finalization event, not a new settlement.
2. Reduce pending_fast_credit_6 by the amount of the original fast-credit.
3. The fUSD was already minted against the insurance reserve when fast-credit was issued.
   Do NOT increase mint_allowance_6 or mint additional fUSD — that would be a double-mint.
4. Emit FastCreditFinalized(message_hash, amount_6, finalized_at_ledger).
5. The insurance reserve headroom is restored by the reduced pending_fast_credit_6.
```

If FINALIZED attestation never arrives (Circle failure), the insurance reserve remains
committed and governance must decide to write off or extend the fast-credit via an explicit
governance action.

NOTE: The distinction between a new CCTP settlement and a finalization event for an already
fast-credited message is determined solely by the presence of `cctp_message_hash` in
`ConsumedCctpMessages`. Implementations that skip this check risk double-minting fUSD.

### 10.7 Route-Level Flows

#### EVM -> Stellar Mint

```text
1. User connects EVM wallet and approves native USDC to `RemoteRouter`.
2. User chooses a destination fUSD chain and recipient.
3. User calls `RemoteRouter.depositAndBridge`.
4. Router transfers USDC and calls Circle `TokenMessengerV2.depositForBurnWithHook`.
5. Deposit metadata commits to destination fUSD chain, recipient, amount, expiry, and user nonce.
6. Destination CCTP domain is the hub-approved settlement recipient, usually Stellar `27` for mint issuance.
7. Circle Iris observes the burn and emits attestation.
8. CCTP relayer submits message and attestation to Stellar.
9. Stellar CCTP receiver mints native USDC to protocol-controlled address.
10. `MintRedeemController` validates source domain, source router, amount, recipient, destination chain, and message hash.
11. `VaultAccounting` records settled collateral.
12. Hub mint allowance is consumed.
13. If the destination is Stellar, `FusdToken` mints locally; otherwise the hub emits a bounded `RemoteMintAuthorize` GMP message to the activated destination spoke.
```

Required checks:

- EVM router is allowlisted in Stellar `ChainState`.
- CCTP amount is parsed from Circle message, not frontend input.
- Message hash is consumed exactly once.
- Remote chain mint cap is checked before remote mint authorization.
- Destination chain must be active and `remote_mint_paused == false`.

#### Solana -> Stellar Mint

```text
1. User connects Solana wallet and selects native USDC SPL token account.
2. User chooses a destination fUSD chain and recipient.
3. User calls `remote_router.deposit_and_burn_usdc`.
4. Solana program validates token account mint, amount, user signature, destination chain, recipient, and pause state.
5. Program invokes Circle Solana CCTP burn to the hub-approved settlement recipient.
6. Circle Iris attests burn.
7. Relayer submits attestation on Stellar.
8. Stellar receives native USDC through CCTP receiver/forwarder.
9. Stellar accounting records settlement and consumes mint allowance.
10. If the destination is Stellar, `FusdToken` mints locally; otherwise the hub emits a bounded `RemoteMintAuthorize` GMP message to the activated destination spoke.
```

Required checks:

- Solana program id is allowlisted as source sender.
- Solana CCTP domain is configured as `5`.
- PDA authorities match expected seeds.
- Token account owner and mint are validated.

#### Stellar Burn -> EVM Redeem

```text
1. User burns fUSD on Stellar.
2. `MintRedeemController` reduces canonical liability and reserves USDC.
3. Controller calls Stellar CCTP `deposit_for_burn` or hook-enabled variant.
4. Destination domain is EVM chain domain, for example Base `6`, Arbitrum `3`, Ethereum `0`, OP Mainnet `2`.
5. Circle attestation is delivered on destination chain.
6. Destination receiver mints native USDC to user's recipient address.
7. Destination router or receiver sends authenticated completion acknowledgement to Stellar.
8. Indexer displays completion after on-chain acknowledgement; it cannot complete accounting by itself.
```

#### Spoke Burn -> EVM Redeem

```text
1. User burns remote fUSD on the spoke router/token path.
2. Spoke router sends `RemoteBurnNotice` GMP to Stellar.
3. Stellar validates source router, remote token, burn id, user, amount, destination domain, recipient, and nonce.
4. Hub accepts burn id exactly once and reduces canonical liability.
5. `MintRedeemController` reserves USDC.
6. Controller sends native USDC to the EVM destination through CCTP.
7. Destination completion acknowledgement marks redeem request complete.
```

#### Redemption Failure Recovery

If the CCTP send in either the Stellar burn redeem or spoke burn redeem flows fails after
the canonical liability has been reduced and the burn id accepted:

```rust
// Retry the CCTP send for a failed redemption. Idempotent: does not re-accept the burn.
fn retry_redeem_cctp_send(
    e: Env,
    caller: Address,  // user, relayer, or governance
    redeem_id: BytesN<32>,
);

// Restore fUSD to the user on Stellar instead of retrying CCTP.
// Restores canonical_liability_6. The burn_id remains consumed; cannot be double-processed.
fn remint_on_redeem_failure(
    e: Env,
    caller: Address,  // user only, or governance
    redeem_id: BytesN<32>,
    remint_recipient: Address,
);
```

Rules:
- `retry_redeem_cctp_send` is callable when `RedeemRequest.status == BurnAcceptedSendFailed`.
- `remint_on_redeem_failure` is callable when status is `BurnAcceptedSendFailed`; it mints fUSD
  back to `remint_recipient`, restores `canonical_liability_6`, and sets status to `RemintIssued`.
- Only one of retry or remint can be executed per `redeem_id`. After either succeeds, the request
  is marked `Completed` or `RemintIssued` and cannot be processed again.
- `burn_id` remains in the consumed set regardless of recovery path.

#### Stellar Burn -> Solana Redeem

```text
1. User burns fUSD on Stellar or the hub has already accepted a spoke burn id.
2. Stellar controller reduces liability, reserves USDC, and calls CCTP with Solana destination domain `5`.
3. Solana recipient must be represented in the CCTP-compatible address format.
4. Circle attestation is delivered to Solana.
5. Native Solana USDC is minted to the recipient or protocol receiver, depending on selected route.
6. Destination acknowledgement marks redeem request complete only after delivery is verified.
```

Required checks:

- Solana recipient token account must be explicitly provided and validated by frontend/backend preflight.
- The user must understand that invalid Solana account encoding can strand funds.
- Redemption UI must show finality status and retry/claim path.

## 11. Axelar GMP Specification

### 11.1 Message Types

All GMP payloads should be versioned and domain-separated.

```rust
enum GmpMessageType {
    DepositIntent,
    DepositSettledAck,
    RemoteMintAuthorize,
    RemoteBurnNotice,
    RemoteRedeemAck,
    AllocationUpdate,
    SpokeCollateralLocked,
    SpokeCollateralReleased,
    EmergencyPause,
    EmergencyUnpause,
    StrategyReport,
    RemoteMintExecuted,
    RemoteMintExpired,
    RemoteBurnAccepted,
}

struct GmpEnvelope {
    version: u32,
    source_chain_id: u32,
    destination_chain_id: u32,
    nonce: u64,
    message_type: GmpMessageType,
    payload_hash: BytesN<32>,
    payload: Bytes,
}
```

Canonical payloads:

```rust
struct MintAuthorizationPayload {
    protocol_version: u32,
    hub_chain_id: u32,
    hub_controller: BytesN<32>,
    source_chain_id: u32,
    source_cctp_domain: u32,
    source_router: BytesN<32>,
    settlement_hash: BytesN<32>,
    settlement_amount_6: i128,
    destination_chain_id: u32,
    destination_router: BytesN<32>,
    destination_fusd_token: BytesN<32>,
    destination_recipient: BytesN<32>,
    route_id: BytesN<32>,
    mint_auth_id: BytesN<32>,
    hub_nonce: u64,
    finality_threshold: u32,
    expiry_ledger_or_timestamp: u64,
}

struct RemoteBurnNoticePayload {
    protocol_version: u32,
    hub_chain_id: u32,
    source_chain_id: u32,
    source_router: BytesN<32>,
    source_fusd_token: BytesN<32>,
    burn_id: BytesN<32>,
    burner: BytesN<32>,
    amount_6: i128,
    destination_cctp_domain: u32,
    usdc_recipient: BytesN<32>,
    nonce: u64,
    expiry_ledger_or_timestamp: u64,
}

struct RemoteMintExecutedPayload {
    protocol_version: u32,
    source_chain_id: u32,
    source_router: BytesN<32>,
    destination_fusd_token: BytesN<32>,
    mint_auth_id: BytesN<32>,
    recipient: BytesN<32>,
    amount_6: i128,
    execution_tx_hash: BytesN<32>,
    nonce: u64,
}

struct AllocationExecutionPayload {
    protocol_version: u32,
    route_id: BytesN<32>,
    trader: BytesN<32>,
    source_chain_id: u32,
    destination_chain_id: u32,
    destination_cctp_domain: u32,
    strategy_id: BytesN<32>,
    guard_set_id: BytesN<32>,
    route_kind: Symbol,
    amount_6: i128,
    route_nonce: u64,
    expiry_ledger_or_timestamp: u64,
}

struct SpokeCollateralLockedPayload {
    protocol_version: u32,
    hub_chain_id: u32,
    source_chain_id: u32,
    source_cctp_domain: u32,
    source_router: BytesN<32>,
    source_vault: BytesN<32>,
    usdc_token: BytesN<32>,
    depositor: BytesN<32>,
    amount_6: i128,
    route_id: BytesN<32>,
    guard_set_id: BytesN<32>,
    lock_id: BytesN<32>,
    vault_balance_after_6: i128,
    local_collateral_cap_6: i128,   // INFORMATIONAL ONLY — self-reported by spoke; hub MUST use
                                     // its own canonical SpokeState.local_collateral_cap_6 for
                                     // cap enforcement. Indexers and monitoring MUST NOT display
                                     // this field as the authoritative cap to users.
    nonce: u64,
    expiry_ledger_or_timestamp: u64,
}

struct SpokeCollateralReleasedPayload {
    protocol_version: u32,
    hub_chain_id: u32,
    source_chain_id: u32,
    source_router: BytesN<32>,
    source_vault: BytesN<32>,
    usdc_token: BytesN<32>,
    release_id: BytesN<32>,
    lock_id: BytesN<32>,
    amount_6: i128,
    destination_cctp_domain: u32,
    usdc_recipient: BytesN<32>,
    nonce: u64,
    expiry_ledger_or_timestamp: u64,
}

struct StrategyReportPayload {
    protocol_version: u32,
    strategy_id: BytesN<32>,
    chain_id: u32,
    adapter_id: BytesN<32>,
    market_id: BytesN<32>,
    asset_token: BytesN<32>,
    block_or_slot: u64,
    onchain_underlying_balance_6: i128,
    withdraw_quote_after_slippage_6: i128,
    oracle_value_6: i128,
    risk_report_value_6: i128,
    haircut_bps: u32,
    expiry_ledger_or_timestamp: u64,
}
```

Payload ids are domain-separated hashes of the canonical encoded payload. Signed reports, GMP payloads, and backend records must use the same canonical encoding vectors.

### 11.2 Source Authentication

Every receiver must verify:

- caller is Axelar gateway/executable entrypoint,
- Axelar source chain name matches configured chain,
- source address matches configured remote router/controller,
- message nonce is unused,
- payload version is supported,
- message type is allowed for that source,
- payload hash equals the canonical encoded payload,
- payload chain ids, router/program ids, token addresses/mints, route ids, amount, nonce, and expiry match configured on-chain state.

### 11.3 Replay Protection

Storage:

```rust
enum DataKey {
    ConsumedGmp(BytesN<32>),
    LastNonce(u32, BytesN<32>),
}
```

Message id:

```text
message_id = hash(source_chain || source_address || nonce || payload_hash)
```

Reject if consumed.

### 11.3A GMP Message Ordering Policy

Axelar does not guarantee in-order delivery between the same source and destination pair.
Receivers must operate correctly under arbitrary delivery order.

Rules:

- Replay protection uses `message_id = hash(source_chain || source_address || nonce || payload_hash)`.
  A message is rejected only if its `message_id` is already in `ConsumedGmp`. Nonce values do NOT
  need to be monotonically increasing at the receiver. Any unused `message_id` is accepted.
- Receivers must NOT reject a message solely because its nonce is lower than a previously processed
  nonce. Only duplicate `message_id` rejection is required.
- `EmergencyPause` messages must be processed immediately upon receipt regardless of delivery order.
  A spoke must accept a pause message even if a mint authorization with a higher nonce has already
  been processed. Pause takes immediate effect.
- Time-sensitive ordering (e.g., pause must be effective before a later mint): enforced by payload
  content, not nonce ordering. `EmergencyPause` payload must include `hub_nonce: u64` (the hub's
  monotonic counter at time of issuance). Every spoke must store `latest_pause_hub_nonce: u64`.
  Any `RemoteMintAuthorize` or `SpokeCollateralLocked` with `hub_nonce >= latest_pause_hub_nonce`
  is rejected while the spoke is paused, even if it was issued before the pause message.
- `EmergencyUnpause` must include `hub_nonce` greater than the pause hub_nonce to be accepted.
- Update `EmergencyPause` and `EmergencyUnpause` payloads to include `hub_nonce: u64` and
  `hub_ledger_at_issue: u32`.

### 11.4 GMP Must Not Mint Collateral

Allowed:

- authorize remote token mint after Stellar has settled collateral,
- record spoke-local collateral locks from approved remote router/vault payloads after native USDC is already in protocol custody,
- update caps,
- pause remote router,
- acknowledge remote burn,
- request rebalance,
- report remote mint execution for supply reconciliation,
- report remote burn execution for hub acceptance.

Not allowed:

- count collateral from a remote message unless it is a finalized CCTP settlement path or an accepted `SpokeCollateralLocked` payload from an approved router/vault,
- bypass caps,
- bypass pause,
- bypass invariant checks,
- move supply directly from one spoke to another without hub authorization,
- reduce canonical liabilities before the hub accepts the remote burn.

Remote mint execution acknowledgement:

```text
remote router executes mint
  -> remote router emits local event
  -> remote router sends authenticated RemoteMintExecuted GMP acknowledgement to Stellar
  -> Stellar verifies source router, remote token, mint_auth_id, amount, destination chain, and nonce
  -> Stellar moves pending_mint_auth_6 to outstanding_supply_6
```

Indexer observations are read-only status hints and must never move `pending_mint_auth_6`, `outstanding_supply_6`, or liabilities.

RemoteMintExecuted ack recovery:

If a `RemoteMintExecuted` GMP acknowledgement is never received by the hub within
`mint_auth_ack_timeout_ledgers` (governance-settable, default: 7 days ≈ 483,840 ledgers),
governance may call:

```rust
fn force_reconcile_mint_auth(
    e: Env,
    gov: Address,
    mint_auth_id: BytesN<32>,
    evidence: Bytes, // signed evidence from an authorized oracle or multi-sig attesting execution
);
```

Effect: moves `pending_mint_auth_6[chain]` to `outstanding_supply_6[chain]` without the GMP ack.

Requirements:
- `MintAuthRecord.status == Pending`
- `current_ledger > MintAuthRecord.issued_ledger + mint_auth_ack_timeout_ledgers`
- `gov` must be the timelock-gated governance address

This is a governance-gated recovery of last resort — the normal path is GMP ack relayer retry.
`cancel_mint_authorization` MUST NOT be called in this scenario if the spoke mint did execute;
doing so would remove the canonical liability record for fUSD that exists on the spoke.

Add `mint_auth_ack_timeout_ledgers: u32` to `GlobalState`.

## 12. Accounting Invariants

### 12.1 Global Solvency

```text
liabilities_6 <= idle_usdc_6 + strategy_value_6 - pending_outbound_6 - required_reserve_6
```

`strategy_value_6` must be conservative, haircut-adjusted, freshness-checked, and never used as mint allowance.

Invariant breach response:

If `assert_invariant` detects a violation:
1. The current transaction reverts (all state changes rolled back via Soroban panic).
2. Because the transaction reverted, no state was mutated — the protocol remains in its
   pre-transaction state, which itself may be the source of the violation if a previous
   transaction caused it.
3. Off-chain monitoring must continuously call `check_invariant()` after every block/ledger.
4. If `check_invariant()` returns false, the monitoring system must immediately trigger an
   emergency governance proposal (`GovernanceController.propose(GlobalPause, Immediate)`).
5. Guardian can also directly call `pause(GlobalPause)` immediately upon detecting breach.
6. Recovery requires: root cause analysis, recapitalization or strategy exit, governance unpause.

Proactive enforcement note: because Soroban transaction reverts prevent panic-triggered storage
writes from committing, automatic breach-triggered pause is achieved through monitoring plus
the guardian's `Immediate` pause authority, not through in-contract storage mutation after panic.

### 12.2 No Double Mint

```text
For every CCTP message hash:
    consumed[hash] == true after first settlement
    no second mint may use the same hash
```

### 12.3 Remote Supply Mirror

```text
total_liabilities_6 =
    stellar_fusd_supply_6
  + sum(remote_fusd_supply_6)
  + sum(pending_remote_mint_authorizations_6)
  - sum(remote_burns_accepted_not_yet_reflected_6)
```

Per-authorization accounting:

`pending_mint_auth_6[chain]` is an aggregate. To prevent drift when individual authorizations
expire or are cancelled at different times, the hub must maintain a per-`mint_auth_id` record
alongside the aggregate:

```rust
// Stored in Persistent storage indexed by mint_auth_id
struct MintAuthRecord {
    mint_auth_id: BytesN<32>,
    chain_id: u32,
    amount_6: i128,
    status: MintAuthStatus,
    issued_ledger: u32,
    expiry_ledger: u32,
    depositor_chain_id: u32,      // 0 = Stellar-native deposit; non-zero = cross-chain origin
    depositor_address: BytesN<32>, // address of original depositor on depositor_chain_id
}

enum MintAuthStatus { Pending, Executed, Expired, Cancelled, RefundSent }
```

Invariant: `pending_mint_auth_6[chain] == sum(amount_6 for all MintAuthRecord where chain_id == chain and status == Pending)`.
This invariant must be tested after every expiry, cancellation, or execution.

If remote fUSD is not live:

```text
total_liabilities_6 = stellar_fusd_supply_6
```

### 12.4 Strategy Cap

```text
strategy_value_6[strategy] <= strategy_debt_ceiling_6[strategy]
strategy_value_6[strategy] <= total_assets_6 * max_bps[strategy] / 10000
```

### 12.5 Chain Cap

```text
remote_minted_6[chain] <= remote_mint_cap_6[chain]
remote_pending_6[chain] + remote_minted_6[chain] <= remote_mint_cap_6[chain]
```

### 12.6 Liquidity Reserve

```text
idle_usdc_6 >= total_liabilities_6 * required_reserve_bps / 10000
```

Can be relaxed only in emergency mode, never during normal minting.

Per-chain and per-route reserves:

```text
idle_usdc_6[chain] >= local_redemption_liability_6[chain] * chain_reserve_bps[chain] / 10000
available_cctp_capacity_6[source_chain,destination_chain] >= expected_short_horizon_redemptions_6[source_chain,destination_chain]
strategy_withdrawable_6[chain] >= stress_withdrawal_requirement_6[chain]
```

Normal rebalances must preserve both global and chain-local reserves.

### 12.7 Mint Allowance

```text
mint_allowance_6 <= settled_unminted_cctp_usdc_6 + accepted_unminted_spoke_vault_locks_6
```

Allowed increases:

- local Stellar USDC deposit,
- finalized inbound CCTP settlement after message consumption.
- accepted spoke-local collateral lock from an approved remote router/vault.

Allowed decreases:

- local Stellar fUSD mint,
- remote mint authorization.

Forbidden increases:

- strategy value report,
- oracle price update,
- generic GMP message without finalized CCTP settlement or accepted spoke vault lock,
- backend/indexer observation,
- trader or manager execution,
- governance override except explicit bad-debt recapitalization with real USDC transfer.

Fast-finality rule:

```text
pending_fast_credit_6 <= fast_credit_insurance_reserve_6
pending_fast_credit_6 never increases mint_allowance_6
```

### 12.8 Hub-Routed Spoke Supply

```text
for every spoke mint:
    exists unique hub mint_auth_id
    exists consumed settlement_hash or hub treasury source
    amount <= unexpired authorization amount
    destination execution acknowledged by authenticated RemoteMintExecuted GMP before pending supply becomes outstanding supply

for every spoke burn:
    exists unique burn_id
    burn_id accepted once by hub
    liability reduction <= burned amount
```

### 12.9 Dust Accounting

```text
protocol_dust_usdc_7 is excluded from mint_allowance_6
protocol_dust_usdc_7 is excluded from user liabilities
dust can be returned to user or swept by timelocked governance only after event emission
```

### 12.10 Spoke-Local Collateral

```text
accepted_spoke_collateral_lock_6[chain]
  <= local_collateral_cap_6[chain]

accepted_spoke_collateral_lock_6[chain]
  = idle_spoke_vault_usdc_6[chain]
  + accepted_guarded_strategy_value_6[chain]
  + reserved_redemption_6[chain]
  - pending_release_6[chain]
```

Rules:

- every spoke collateral lock has a unique `lock_id`,
- every release has a unique `release_id` and references an accepted lock or aggregate release bucket,
- only an approved router/vault pair can create accepted spoke collateral,
- the native USDC token/mint must match chain config,
- strategy deployment changes the location of accepted collateral, not the amount of mint allowance,
- local collateral caps are independent from CCTP inbound mint caps.

## 13. Yield Design

### 13.1 Recommended Product Model

Use two tokens:

- `fUSD`: par stablecoin, transferable, redeemable for USDC.
- `sfUSD`: yield-bearing staking receipt or vault share.

Rationale:

- fUSD stays clean for payments and integrations.
- Yield can be distributed to users who opt into staking.
- Loss/socialization rules are simpler.
- Remote fUSD portability remains easier.

### 13.2 `SfUsdVault`

Optional contract:

```rust
struct VaultState {
    total_shares: i128,
    total_staked_fusd_6: i128,
    reward_per_share_12: i128,     // 12-decimal precision prevents i128 overflow at scale
    pending_yield_6: i128,
    max_yield_per_epoch_6: i128,   // governance-set cap on yield per notification
    last_yield_epoch_ledger: u32,  // for epoch-based yield rate limiting
}
```

Precision note on reward_per_share_12:

Using 12-decimal fixed-point precision prevents i128 overflow:
- Maximum anticipated fUSD supply: 10^12 tokens (one trillion fUSD).
- In 6-decimal units: 10^18.
- reward_per_share_12 * total_shares_6 / 1e12 = result in fUSD-6.
- Worst case multiplication: 10^18 (shares) × 10^18 (high reward_per_share_12) = 10^36 < 1.7×10^38 (i128 max). Safe.
- Implementation must verify this bound holds against the configured maximum supply cap before deployment.

Methods:

```rust
fn stake(e: Env, user: Address, amount: i128, min_shares: i128);
fn unstake(e: Env, user: Address, shares: i128, min_fusd: i128);
fn harvest(e: Env, user: Address);

// Notifies the vault that yield has been realized and is physically present in the vault.
// Caller must be a registered and active StrategyAdapter in StrategyAdapterRegistry.
// USDC must have already been transferred into the vault before this call (same tx).
// The vault verifies its USDC balance increased by at least yield_amount_6 in this transaction.
fn notify_realized_yield(
    e: Env,
    strategy_adapter: Address,     // must match a registered active adapter
    strategy_id: BytesN<32>,       // must match adapter's registered strategy_id
    yield_amount_6: i128,          // must be > 0
    withdrawal_tx_hash: BytesN<32> // audit log only — informational, not verified on-chain
);
```

Per-epoch yield accumulation rule:

`notify_realized_yield` MUST check and enforce the epoch cap cumulatively, not per-call:

```text
// Roll epoch if needed (read from GlobalState.epoch_start_ledger + epoch_length_ledgers)
if current_ledger >= global_state.epoch_start_ledger + global_state.epoch_length_ledgers:
    global_state.yield_credited_this_epoch_6 = 0
    global_state.epoch_start_ledger = current_ledger

// Check cumulative cap — not just per-call
assert global_state.yield_credited_this_epoch_6 + yield_amount_6 <= global_state.max_yield_per_epoch_6

global_state.yield_credited_this_epoch_6 += yield_amount_6
```

A strategy that calls `notify_realized_yield` twice in the same epoch with half the max each time
would have both calls pass a per-call check but the second call MUST be rejected by the cumulative check.

Yield sources:

- realized xycLoans flash-loan fee income, realized deFindex vault yield,
- guarded local and remote lending interest,
- guarded DEX/LP fees after haircut and realization,
- retained protocol fees if enabled,
- treasury-directed rewards if explicitly approved.

Do not distribute unrealized or unwithdrawable yield as liquid fUSD unless it remains fully backed by conservative valuation.

## 14. Governance And Roles

Roles:

```rust
enum Role {
    Admin,
    Timelock,
    Guardian,
    Manager,
    RiskManager,
    Trader,
    AllocationOperator,
    CctpRelayer,
    AxelarRelayer,
    StrategyAdapter,
    RemoteRouter,
}
```

Role rules:

- `Admin`: deployment bootstrap only; should transfer to timelock.
- `Timelock`: upgrades, caps, strategy registration, chain registration.
- `Guardian`: pause, emergency exits, disable route.
- `Manager`: sets active fees inside timelock-approved bounds and selects among timelock-whitelisted strategies.
- `RiskManager`: propose allocation targets, cannot move funds by itself unless separately authorized.
- `Trader`: chooses and executes approved bridge routes and approved guarded investment strategies within caps.
- `AllocationOperator`: automation role for bounded rebalances; same limits as Trader unless governance grants narrower permissions.
- `CctpRelayer`: submits attestations, cannot alter amount/recipient.
- `AxelarRelayer`: operational role only; gateway verification is the security boundary.
- `StrategyAdapter`: can report/move only for its strategy id.
- `RemoteRouter`: allowlisted source for chain-specific GMP messages.

Trader limits:

- can execute `execute_bridge_route`, `rebalance_to_strategy`, `rebalance_from_strategy`, harvest, and guarded unwind calls only for active routes,
- can choose between active CCTP routes and active local strategy routes,
- can select target/calldata only if the target and calldata pass the chain-local contract/instruction guard,
- cannot register or upgrade a guard,
- cannot add or activate a chain,
- cannot change remote router/vault addresses,
- cannot increase caps or reduce reserves,
- cannot mark collateral as settled,
- cannot mint, burn, or alter canonical liabilities,
- cannot accept strategy value reports unless separately authorized as a reporting adapter.

Manager limits:

- can set mint, redeem, route, management, and performance fees only inside `FeeBounds`,
- can select, pause, or unpause only `manager_selectable` strategies already registered by timelock governance,
- can set active target weights only below strategy `max_bps`, chain caps, debt ceilings, and reserve constraints,
- cannot add a new strategy adapter, contract guard, asset guard, chain, route, router, vault, token address, or CCTP domain,
- cannot increase hard fee maximums, increase debt ceilings, increase chain caps, lower reserves, upgrade contracts, or change fee recipient,
- cannot mint, burn, mark CCTP as settled, mark spoke collateral as locked, or alter canonical liabilities,
- fee and strategy changes must emit versioned events and invalidate stale frontend/backend quotes.

Timelock:

- 48h standard delay for upgrades/cap increases.
- 0-6h delay for risk-reducing changes, including emergency unpause.
- Immediate guardian pause.
- Emergency unpause is classified as a risk-reducing action and is subject to the 0-6h timelock.
- Guardian false-alarm self-revoke: a guardian may cancel their own pause within
  `guardian_self_revoke_window_ledgers` (e.g., ~1 hour) if no governance proposal has been
  initiated for the same scope. After the self-revoke window expires, unpause requires governance.
- Guardian self-revoke emits `GuardianPauseSelfRevoked(guardian, scope, ledger)`.
- Guardian cannot upgrade, add chains, increase caps, or mint, even during an incident.

### 14.1 Runtime-Specific Upgrade Rules

Stellar hub:

- `GovernanceController` and `VaultAccounting` upgrades require timelock.
- `FusdToken` controller changes require timelock.
- Strategy adapter registration requires timelock unless disabling a strategy.
- Guardian can pause but cannot upgrade, add chains, increase caps, or mint.

EVM spokes:

- `RemoteRouter` proxy admin must be controlled by a timelock or multisig governed by Stellar hub policy.
- `RemoteFusd` mint authority must be the remote router controlled by hub authorization.
- EVM guardian can pause deposits, redemptions, and remote mint execution, but cannot upgrade or change mint caps.
- Emergency EVM implementation upgrade must begin with a hub-side chain pause and local router pause before the implementation is changed.
- Unpause requires bytecode verification, config verification, smoke checks, and governance approval.

Solana spokes:

- Program upgrade authority must be controlled by multisig/timelock.
- PDA mint authority must not be the same key as program upgrade authority.
- Guardian can pause instructions but cannot alter PDA seeds, mint authority, CCTP program ids, or caps.
- Any Solana program upgrade requires hub-side spoke pause before upgrade and hub-side unpause after verification.

Axelar/ITS:

- Production v1 does not use direct Axelar ITS token movement for fUSD.
- If ITS is introduced later, token manager/operator roles must be mapped into the same hub governance policy and validated separately.
- Any future ITS deployment salts, token ids, token managers, trusted chains, and operators must be stored in `SpokeState` or a linked config contract.
- Hub must be able to pause remote mint authorization independently of Axelar/ITS availability.

Upgrade runbook for every spoke:

```text
1. Pause spoke on Stellar.
2. Pause remote router/program locally.
3. Upgrade implementation or program.
4. Verify bytecode, program data, proxy/admin, config, token/mint authority, CCTP endpoints, and Axelar source addresses.
5. Run smoke checks for rejected mint, rejected burn, pause behavior, and canonical payload decoding.
6. Unpause only through governance.
```

### 14.2 `GovernanceController` Specification

Purpose: on-chain timelock and proposal execution engine for the Stellar hub.
Soroban has no native TimelockController; this contract implements timelock semantics explicitly.

```rust
struct Proposal {
    proposal_id: BytesN<32>,          // hash(proposer, target, call_data, proposed_ledger)
    proposer: Address,
    target_contract: Address,
    call_data: Bytes,
    proposed_ledger: u32,
    earliest_execute_ledger: u32,     // proposed_ledger + delay_in_ledgers(delay_class)
    executed: bool,
    cancelled: bool,
    delay_class: DelayClass,
}

enum DelayClass {
    Standard,       // 48h in ledgers (~138,240 at 1.25s/ledger)
    RiskReducing,   // 0-6h in ledgers (0-17,280 ledgers); includes emergency unpause
    Immediate,      // 0 ledgers; guardian-class actions only (pause, emergency exit)
}
```

Methods:

```rust
fn propose(
    e: Env,
    proposer: Address,
    target: Address,
    call_data: Bytes,
    delay_class: DelayClass
) -> BytesN<32>;                      // returns proposal_id

fn execute(e: Env, executor: Address, proposal_id: BytesN<32>);
fn cancel(e: Env, canceller: Address, proposal_id: BytesN<32>);

fn grant_role(e: Env, admin: Address, account: Address, role: Role);
fn revoke_role(e: Env, admin: Address, account: Address, role: Role);
fn has_role(e: Env, account: Address, role: Role) -> bool;
```

Rules:
- `delay_class` submitted by the proposer must match the proposer's role. The contract must enforce:
  ```text
  if delay_class == DelayClass::Immediate:
      require proposer has Guardian role (Timelock role also accepted)
  if delay_class == DelayClass::Standard or DelayClass::RiskReducing:
      require proposer has Timelock role
      reject if proposer has only Guardian role and no Timelock role
  ```
  A `Guardian`-only proposer passing `delay_class = Standard` or `RiskReducing` MUST be rejected.
- Only `Timelock` role may propose `Standard` and `RiskReducing` actions.
- `Guardian` role may propose `Immediate` actions (pauses, emergency exits) only.
- Only the proposer or `Admin` may cancel a proposal before execution.
- Execution requires: not cancelled, not executed, `current_ledger >= earliest_execute_ledger`.
- Executed proposals cannot be re-executed (`executed = true` is permanent).
- Role changes (`grant_role`, `revoke_role`) require `Admin` auth and are subject to `Standard` delay for adding roles, `RiskReducing` for removing roles.
- `Admin` role should be transferred to a multisig and the single deployer key revoked immediately after deployment bootstrap.
- All proposals emit `ProposalCreated(proposal_id, proposer, target, delay_class, earliest_execute_ledger)`.
- All executions emit `ProposalExecuted(proposal_id, executor, ledger)`.
- All cancellations emit `ProposalCancelled(proposal_id, canceller, ledger)`.

## 15. Pause Matrix

| Pause flag | Effect |
| --- | --- |
| Global pause | Disable mints, normal rebalances, and remote mint authorizations |
| Deposit pause | Disable new deposits only |
| Redeem pause | Disable non-emergency remote redemption; local redeem can remain open if safe |
| Strategy pause | Disable deposits to a strategy; withdrawals remain enabled |
| Chain pause | Disable remote mint/redeem for a chain |
| GMP pause | Ignore non-emergency GMP messages |
| CCTP fast pause | Disable fast-finality transfers; finalized remains available |

## 16. Oracle And Valuation

### 16.1 USDC

USDC is valued at par for mint/redeem, subject to emergency governance override if Circle/USDC breaks.

### 16.2 xycLoans and deFindex

Neither venue depends on an external price oracle. xycLoans values a position directly
from its own 1:1 share accounting plus its matured-fee snapshot (§8.2); deFindex values a
position via the vault's own `get_asset_amounts_per_shares` (§8.3), which reflects
whatever its underlying strategy(ies) report. No Reflector/oracle integration is needed
for either.

### 16.3 Blend (retained, not active)

Use Blend's accounting and Reflector/oracle data where applicable, if and when a future
Blend V3 integration is independently evaluated and approved (see §8 status note).

Valuation must be conservative:

```text
value = min(claimable_underlying, oracle_adjusted_value, withdrawal_liquidity_adjusted_value)
```

### 16.3 Remote Lending

Remote positions should be included only if:

- protocol adapter is allowlisted,
- remote chain is active,
- latest report is fresh,
- value can be independently checked from on-chain state; signed reports may cap value downward but cannot be the only positive proof,
- asset token/mint, market id, adapter id, oracle id, block/slot, liquidity, utilization, and expiry are bound in the report payload,
- strategy debt ceiling and chain exposure caps are enforced,
- haircut is applied.

Valuation — Stellar-local strategies:

```text
stellar_strategy_value = min(
    onchain_underlying_balance_6,    // verifiable via SAC balance call
    withdraw_quote_after_slippage_6,
    oracle_value_6,
    risk_report_value_6
) * haircut_bps / 10000
```

Valuation — remote (EVM / Solana) strategies:

Stellar cannot independently verify on-chain balances on EVM or Solana chains.
`onchain_underlying_balance_6` for remote positions can only be sourced from the same adapter
that could be compromised. It is therefore EXCLUDED from the minimum for remote positions.

```text
remote_strategy_accepted_value_6 = min(
    withdraw_quote_after_slippage_6,   // from GuardedStrategyExecutor balance/quote call
    oracle_value_6,                    // from verified oracle on the remote chain
    risk_report_value_6                // from signed risk agent report
) * haircut_bps / 10000
```

Additional caps for remote strategy exposure:

```text
remote_strategy_value_6[chain] / total_backing_6 <= remote_strategy_cap_bps / 10000
```

- `remote_strategy_cap_bps` is a governance-set hard cap (default: 3000 bps = 30%).
- This cap applies per-chain and in aggregate across all remote strategies.
- A remote strategy that would push the aggregate over the cap is valued at zero for the excess.
- Stale, unsigned, unverifiable, wrong-token, wrong-market, or over-cap reports are zero.

## 17. Off-Chain Services

### 17.1 CCTP Relayer

Responsibilities:

- watch CCTP burn events,
- query Circle Iris `/v2/messages`,
- wait for required attestation status,
- submit Stellar receive/forward transaction,
- retry and monitor stuck transfers,
- produce operational verification logs.

Must not:

- decide mint amount independently,
- bypass on-chain message parsing,
- custody funds.

### 17.2 Axelar Relayer/Executor

Responsibilities:

- estimate/pay gas where required,
- submit GMP payloads,
- monitor Axelar message state,
- retry destination execution.

### 17.3 Risk Agent

Responsibilities:

- ingest APY, liquidity, utilization, oracle health, borrow demand, cap data,
- compute risk-adjusted allocation targets,
- produce signed proposals,
- never directly move funds unless on-chain caps permit.

### 17.4 Indexer

Responsibilities:

- index mint/redeem events,
- reconcile CCTP messages,
- reconcile GMP messages,
- compute public proof of backing,
- monitor invariant drift.

## 17A. Backend Architecture

The backend is operational infrastructure, not a trusted accounting authority. All critical accounting outcomes must be enforced by contracts. Backend services provide routing, indexing, relaying, monitoring, risk computation, and user-facing status.

### 17A.1 Service Map

```text
apps/web
  -> api-gateway
       -> route-service
       -> portfolio-service
       -> proof-service
       -> status-service

workers/
  cctp-relayer
  axelar-relayer
  indexer
  risk-agent
  allocation-executor
  proof-publisher
  alerting-worker

storage/
  postgres
  redis/queue
  object storage for signed reports
```

### 17A.2 API Gateway

Responsibilities:

- expose read-only protocol data to the dApp,
- create route quotes,
- provide transaction build parameters,
- return CCTP/GMP transfer status,
- return portfolio balances and pending claims,
- never sign user transactions,
- never decide collateral accounting.

Suggested endpoints:

```text
GET  /v1/chains
GET  /v1/routes?fromChain=&toChain=&asset=USDC&amount=
GET  /v1/mint-authorizations/:mintAuthId
GET  /v1/allocation-routes
GET  /v1/user/:address/positions
GET  /v1/transfers/:transferId
GET  /v1/proof-of-reserves
GET  /v1/allocations
GET  /v1/yields
POST /v1/quotes/mint
POST /v1/quotes/redeem
POST /v1/tx/build
```

### 17A.3 Database Model

Minimum tables:

```sql
chains(id, name, runtime, cctp_domain, axelar_id, active, deposits_paused, redeems_paused, remote_mint_paused, local_collateral_enabled, local_collateral_cap_6)
contracts(chain_id, name, address, version, deployed_at, active)
guard_sets(id, chain_id, guarded_executor, active, version, created_at)
contract_guards(id, guard_set_id, target_contract, protocol, guard_address, active)
asset_guards(id, guard_set_id, asset_or_position_type, valuation_mode, withdrawal_mode, guard_address, active)
guarded_executions(id, trader, route_id, guard_set_id, strategy_id, target, calldata_hash, amount_6, route_nonce, status, tx_hash)
spoke_collateral_locks(id, chain_id, router, vault, usdc_token, depositor, route_id, guard_set_id, amount_6, vault_balance_after_6, status, tx_hash)
spoke_collateral_releases(id, chain_id, router, vault, usdc_token, release_id, amount_6, destination_cctp_domain, recipient, status, tx_hash)
transfers(id, user_address, source_chain, destination_chain, destination_recipient, amount_6, status, created_at, updated_at)
cctp_messages(hash, source_domain, destination_domain, nonce, amount_6, finality_threshold, finalized, status, attestation_status)
gmp_messages(id, source_chain, destination_chain, nonce, payload_hash, canonical_payload_type, status)
mint_authorizations(id, settlement_hash, route_id, source_chain, destination_chain, destination_router, destination_fusd_token, recipient, amount_6, finality_threshold, expiry, status, executed_tx_hash)
remote_execution_acks(id, mint_auth_id, source_chain, source_router, amount_6, execution_tx_hash, status)
balances(user_address, chain_id, fusd_balance_6, sfusd_balance_6, usdc_balance_6, updated_at)
strategy_reports(strategy_id, chain_id, adapter_id, market_id, asset_token, block_or_slot, onchain_underlying_6, withdraw_quote_6, oracle_value_6, risk_report_value_6, haircut_bps, accepted_value_6, report_hash, expires_at, created_at)
allocation_targets(strategy_id, target_bps, max_bps, risk_epoch, valid_until, status)
allocation_routes(route_id, source_chain, destination_chain, destination_cctp_domain, strategy_id, guard_set_id, route_kind, max_in_flight_6, max_daily_move_6, active)
trader_permissions(trader, chain_id, route_id, max_trade_6, daily_limit_6, active, expires_at)
manager_permissions(manager, max_fee_bps, can_select_strategies, active, expires_at)
fee_configs(version, mint_fee_bps, redeem_fee_bps, route_fee_bps, management_fee_bps, performance_fee_bps, fee_recipient, effective_at, changed_by)
strategy_whitelist(strategy_id, chain_id, adapter, guard_set_id, strategy_type, manager_selectable, deposit_enabled, withdraw_enabled, max_bps, debt_ceiling_6, min_liquidity_6, version, active)
fast_credit_exposures(id, user_address, source_chain, amount_6, insurance_reserved_6, status)
dust_events(id, user_address, amount_7, action, context, tx_hash)
proof_snapshots(id, liabilities_6, collateral_6, reserve_6, per_chain_reserves_hash, hash, published_at)
```

Idempotency:

- CCTP message hash is a unique key.
- GMP message id is a unique key.
- Mint authorization id is globally unique and single-use.
- Remote execution acknowledgement id is globally unique and single-use.
- Allocation route id is versioned and immutable after activation; changes create a new route version.
- Guard set id is versioned and immutable after activation; guard changes create a new guard set version.
- Guarded execution id is deterministic from route id, target, calldata hash, route nonce, and tx hash.
- Spoke collateral lock id is globally unique and single-use.
- Spoke collateral release id is globally unique and single-use.
- Trader execution id binds trader, route id, amount, nonce, target/calldata hash, and tx hash.
- Fee config versions are append-only.
- Strategy whitelist versions are append-only; changing guard, cap, adapter, or manager-selectable state creates a new version.
- User transfer id is deterministic from source tx hash, log index, and route version.
- Workers must be safe to retry.

### 17A.4 Relayer Security

Relayers are permissionless where possible. If a relayer role is used for cost control, contracts still verify message validity.

Rules:

- relayers cannot pass arbitrary amounts,
- relayers cannot choose recipients after source transaction,
- relayers cannot bypass consumed-message checks,
- relayers should use separate hot keys per network,
- relayer keys cannot upgrade contracts or change caps.

### 17A.5 Risk And Allocation Backend

The risk agent computes proposals. It does not directly change accounting.

Inputs:

- USDC liquidity by chain,
- lending APY,
- utilization,
- withdraw liquidity,
- oracle health,
- CCTP transfer times,
- Axelar delivery health,
- chain incident status,
- strategy loss events,
- redemption demand.

Outputs:

```json
{
  "riskEpoch": 42,
  "targets": [
    {
      "strategyId": "blend-usdc-main",
      "routeId": "stellar-usdc-to-blend-main-v1",
      "sourceChainId": 27,
      "destinationChainId": 27,
      "destinationCctpDomain": 27,
      "targetBps": 3500,
      "maxBps": 5000,
      "maxDeltaPerRebalance6": "250000000000",
      "validUntilLedger": 12345678
    }
  ],
  "signature": "..."
}
```

On-chain acceptance:

- governance or risk manager submits target,
- `AllocationManager` checks signature/role,
- timelock is required for risk-increasing cap changes,
- executor can only move within accepted bounds.

### 17A.6 Proof Of Reserves

Publish at least:

- total fUSD liabilities by chain,
- idle USDC by chain,
- required reserve by chain,
- CCTP route capacity by source/destination chain,
- strategy value by strategy,
- haircut applied per strategy,
- accepted strategy value inputs: on-chain balance, withdraw quote, oracle value, and risk report value,
- pending inbound CCTP,
- pending outbound CCTP,
- pending fast-credit exposure and insurance reserve,
- protocol dust in Stellar USDC 7-decimal units,
- reserve requirement,
- resulting collateral ratio.

The proof publisher should include source transaction hashes and contract calls used for each number.

On-chain anchoring:

The proof publisher must call an on-chain `ProofAnchor` Soroban contract to prevent backdating
or substituting proof data:

```rust
fn publish_proof_anchor(
    e: Env,
    caller: Address,         // proof_publisher role
    snapshot_hash: BytesN<32>,
    snapshot_ledger: u32,    // Stellar ledger at which the snapshot was computed
);

fn latest_anchor(e: Env) -> ProofAnchor;

struct ProofAnchor {
    snapshot_hash: BytesN<32>,
    snapshot_ledger: u32,
    published_at_ledger: u32,
    publisher: Address,
}
```

Rules:
- `snapshot_ledger` must satisfy:
  ```text
  snapshot_ledger >= current_ledger - max_anchor_lag_ledgers   // not stale
  snapshot_ledger <= current_ledger                            // not future-dated
  ```
  where `max_anchor_lag_ledgers` defaults to ~30 min (1,440 ledgers at 1.25s/ledger).
  Both bounds are enforced on-chain in `publish_proof_anchor`.
- External verifiers can check: `hash(snapshot_data) == onchain_anchor.snapshot_hash`
  and `snapshot_ledger` matches the claimed data time.
- Update DB schema: add `snapshot_ledger_sequence INTEGER NOT NULL` and `onchain_anchor_tx_hash TEXT`
  to the `proof_snapshots` table.

## 17B. dApp Frontend Architecture

The frontend must make cross-chain state visible. The user should never be left guessing whether funds are waiting for Circle attestation, Axelar execution, destination claim, or local confirmation.

### 17B.1 Stack

Recommended:

- Next.js or Vite React app,
- TypeScript SDK shared with backend,
- `wagmi`/WalletConnect/RainbowKit for EVM,
- Stellar Wallet Kit/Freighter/Lobstr-style wallet support for Stellar,
- Solana Wallet Adapter for Phantom/Solflare/backpack-style wallets,
- TanStack Query for chain/indexer state,
- a route/status service for CCTP and GMP status,
- Sentry/Datadog/OpenTelemetry for frontend errors and transaction funnel metrics.

### 17B.2 Frontend Modules

```text
apps/web/
  routes/
    mint
    redeem
    stake
    portfolio
    proof
    governance
  components/
    ChainSelector
    RouteQuote
    TransactionTimeline
    WalletSwitcher
    LiquidityWarning
    ProofOfBacking
  lib/
    evmClient
    stellarClient
    solanaClient
    cctpStatus
    axelarStatus
    amountConversion
```

### 17B.3 Wallet And Chain Sessions

The dApp needs a multi-wallet session model:

```ts
type WalletSession =
  | { runtime: "evm"; chainId: number; address: `0x${string}` }
  | { runtime: "stellar"; publicKey: string; contractAddress?: string }
  | { runtime: "solana"; publicKey: string };
```

Users may connect more than one wallet at once when moving funds across runtimes. The dApp should support:

- source wallet connection,
- destination wallet connection,
- destination address validation,
- route preflight,
- transaction simulation where available,
- recovery from partial completion.

### 17B.4 Transaction State Machine

```ts
type CrossChainTransferStatus =
  | "quote_created"
  | "source_approval_required"
  | "source_tx_pending_signature"
  | "source_tx_submitted"
  | "source_tx_confirmed"
  | "cctp_attestation_pending"
  | "cctp_attestation_ready"
  | "destination_receive_submitted"
  | "destination_receive_confirmed"
  | "stellar_accounting_confirmed"
  | "gmp_ack_pending"
  | "remote_mint_pending"
  | "completed"
  | "needs_user_claim"
  | "failed_recoverable"
  | "failed_terminal";
```

Each state must have:

- user-facing label,
- tx hash or message hash,
- retry action if applicable,
- support/debug payload,
- timeout threshold.

### 17B.5 User Flows

Mint from EVM:

```text
Connect EVM wallet
  -> choose destination fUSD chain and recipient
  -> approve USDC
  -> burn USDC through CCTP
  -> wait for attestation
  -> hub verifies native USDC settlement
  -> hub consumes mint allowance
  -> mint fUSD on Stellar or execute remote mint authorization on selected spoke
```

Mint from Solana:

```text
Connect Solana wallet
  -> select USDC token account
  -> choose destination fUSD chain and validate recipient
  -> call Solana router
  -> wait for CCTP attestation
  -> hub verifies native USDC settlement
  -> hub consumes mint allowance
  -> mint fUSD on Stellar or execute remote mint authorization on selected spoke
```

Redeem:

```text
Select fUSD source chain
  -> select USDC destination chain
  -> check liquidity and fees
  -> burn fUSD
  -> reserve USDC
  -> send CCTP transfer
  -> receive native USDC on destination
```

Stake:

```text
Hold fUSD on Stellar or bridge to Stellar
  -> approve/stake fUSD into sfUSD vault
  -> track pending rewards
  -> harvest or unstake
```

### 17B.6 UX Risk Requirements

The UI must show:

- fUSD is redeemable from available liquidity, not magically from all deployed strategies instantly,
- estimated CCTP finality path,
- destination chain receive requirements,
- Stellar USDC seven-decimal and CCTP six-decimal rounding,
- remote mint caps,
- paused routes,
- pending or stuck transfers,
- proof-of-backing link for every mint/redeem page.

## 18. Event Schema

Use stable, indexer-friendly events.

Examples:

```rust
event DepositLocal(user, amount_usdc_6, minted_fusd)
event DepositRemoteSettled(source_domain, message_hash, recipient, amount_usdc_6)
event RedeemLocal(user, burned_fusd, amount_usdc_6)
event RedeemRemoteInitiated(user, redeem_id, destination_domain, amount_usdc_6)
event CctpMessageConsumed(message_hash, source_domain, amount_usdc_6)
event GmpMessageConsumed(message_id, source_chain, message_type)
event SpokeCollateralLocked(chain_id, lock_id, router, vault, amount_usdc_6, route_id)
event SpokeCollateralReleased(chain_id, release_id, router, vault, amount_usdc_6, destination_domain)
event StrategyAllocated(strategy_id, amount_usdc_6)
event StrategyWithdrawn(strategy_id, amount_usdc_6)
event StrategyValueReported(strategy_id, value_usdc_6)
event TraderRouteExecuted(trader, route_id, route_kind, amount_usdc_6, route_nonce)
event GuardedStrategyExecuted(trader, route_id, strategy_id, target, calldata_hash, amount_usdc_6)
event FeesUpdated(manager, version, mint_fee_bps, redeem_fee_bps, route_fee_bps, management_fee_bps, performance_fee_bps)
event StrategyWhitelistUpdated(manager_or_governance, strategy_id, version, manager_selectable, deposit_enabled, withdraw_enabled)
event InvariantChecked(liabilities_6, assets_6, reserve_6)
event EmergencyPause(scope)
```

## 19. Failure Modes And Recovery

### 19.1 CCTP Attestation Delayed

State:

- remote USDC has been burned,
- Stellar has not received minted USDC,
- no fUSD minted yet.

Recovery:

- relayer retries Iris query,
- user sees pending deposit,
- no solvency risk.

### 19.2 CCTP Receive Succeeds But GMP Ack Fails

State:

- Stellar has USDC,
- canonical fUSD may be minted on Stellar,
- remote UI may be pending.

Recovery:

- retry GMP acknowledgement,
- user can claim on Stellar if configured,
- no double mint because message hash is consumed.

### 19.3 GMP Message Arrives Before CCTP Settlement

Action:

- store as intent only,
- do not mint,
- expire after configured ledger/time.

### 19.4 Strategy Withdrawal Fails

Action:

- pause new deposits to strategy,
- preserve redemptions from liquidity buffer,
- execute emergency exit if needed,
- apply haircut if valuation no longer withdrawable.

### 19.5 Remote Router Compromised

Action:

- pause chain in Stellar governance,
- reject all future GMP messages from router,
- pause remote token mint,
- cap losses to already-settled exposure if accounting rules are followed.

## 20. Security Requirements

### 20.1 Soroban

- Every user action must call `require_auth`.
- Admin paths require role auth.
- Use checked arithmetic; reject negative amounts.
- Use canonical message hashing for replay protection.
- Avoid storing unbounded vectors in hot paths.
- Keep storage TTL/archival strategy explicit.
- Separate pause from upgrade.

### 20.1A Soroban Storage TTL and Archival Policy

Soroban has three storage types: Persistent, Temporary, and Instance.
Temporary storage is automatically pruned after TTL expires. Persistent storage survives TTL
expiry but can be archived (removed from hot state) and requires ledger archival proof to restore.

Security-critical data — must NEVER be pruned without deliberate governance action:

| Data | Storage type | Rationale |
| --- | --- | --- |
| `ConsumedGmp(message_id)` | Persistent | Replay protection for all GMP messages |
| `CctpMessageConsumed(hash)` | Persistent | Replay protection for CCTP settlements |
| `ConsumedMintAuth(mint_auth_id)` | Persistent | Replay protection for remote mint authorizations |
| `SpokeBurnConsumed(burn_id)` | Persistent | Replay protection for spoke burn notices |
| `SpokeLockConsumed(lock_id)` | Persistent | Replay protection for spoke collateral locks |
| `SpokeReleaseConsumed(release_id)` | Persistent | Replay protection for spoke releases |
| `GlobalState` | Instance | Core protocol state; tied to contract lifetime |
| `ChainState(chain_id)` | Persistent | Per-chain accounting; must survive long inactivity |
| `StrategyState(strategy_id)` | Persistent | Strategy accounting |

Implementation requirements:

- All replay-protection entries MUST use Persistent storage.
- At the time of WRITING each consumed entry, the contract MUST call:
  `env.storage().persistent().extend_ttl(key, min_ttl, max_ttl)`
  where `min_ttl` and `max_ttl` ensure at least 5 years of storage (~13,140,000 ledgers
  at 1.25s/ledger on Stellar mainnet).
- `GlobalState` and role config MUST use Instance storage.
- Temporary storage MUST NOT be used for any security-critical or replay-protection state.
- All contracts MUST implement:
  `fn extend_all_replay_ttls(e: Env, gov: Address)` — callable during maintenance windows to
  bulk-extend TTLs on replay-protection entries before they approach archival threshold.
- Deployment scripts must verify storage type assignments before mainnet deploy.

If a replay-protection entry is archived and not restored, the corresponding message CAN be
replayed. This is a critical vulnerability. Proactive TTL management is mandatory.

### 20.2 CCTP

- Validate message hash/nonce uniqueness.
- Validate source domain and source sender.
- Validate amount from parsed message, not from relayer input.
- Track finality threshold.
- Handle six/seven decimal conversion explicitly.
- Never treat pending burns as collateral.

### 20.3 Axelar

- Verify gateway/executable caller.
- Verify source chain and source address.
- Use versioned payloads.
- Store consumed message ids.
- Rate-limit sensitive remote commands.
- Emergency messages should be minimal and deterministic.

### 20.4 Remote EVM

- Use `SafeERC20`.
- Use reentrancy guards.
- Avoid arbitrary external calls.
- Store immutable CCTP/Axelar addresses where possible.
- Add local pause.
- Require Stellar hub authorization before minting remote fUSD.

### 20.5 Remote Solana

- Validate every account passed to every instruction.
- Derive PDA authorities from versioned seeds.
- Require token account owner and mint checks for every SPL transfer.
- Reject unknown token programs unless explicitly supporting Token-2022.
- Store consumed message accounts with deterministic seeds.
- Separate upgrade authority, guardian pause authority, and mint authority.
- Disable remote fUSD minting for any spoke that has not completed hub authorization, replay, cap, pause, and reconciliation validation.

### 20.6 Hub-And-Spoke Specific Controls

- No spoke-to-spoke supply movement without hub authorization.
- No remote strategy deployment without hub cap, adapter registration, guard set registration, and guarded executor approval.
- No remote strategy value can create mint allowance.
- No remote burn can release USDC until accepted by hub.
- No remote router can change destination Stellar controller after deployment without hub governance.
- All cross-runtime message payloads must include `hub_domain`, `spoke_chain_id`, `route_id`, `nonce`, `amount_6`, and `expiry`.
- All cross-runtime message payloads must be domain-separated by protocol version and chain ids.

## 21. Testing Specification

### 21.1 Unit Tests

Soroban:

- token mint/burn/transfer/allowance,
- local deposit/redeem,
- invariant enforcement,
- mint allowance cannot increase from strategy reports,
- strategy value cannot create new fUSD supply,
- spoke-local collateral lock increases mint allowance only after approved router/vault payload,
- forged spoke-local collateral lock reverts,
- duplicate spoke collateral lock id reverts,
- wrong spoke vault/router/USDC token in lock payload reverts,
- local collateral cap exceeded reverts,
- CCTP message consumption,
- finalized CCTP settlement increases mint allowance,
- fast/confirmed CCTP settlement only increases pending fast credit,
- dust return/retention accounting,
- decimal conversion,
- role checks,
- manager can set fees only inside fee bounds using ManagerFeeConfig (no fee_recipient field),
- manager cannot change fee recipient (fee_recipient is governance-only via set_fee_recipient),
- manager cannot change fee recipient even by submitting a full ActiveFeeConfig struct,
- manager can select only manager-selectable strategies,
- manager cannot register adapter, guard, chain, route, or raise strategy cap,
- pause matrix,
- strategy cap checks,
- per-chain liquidity reserve enforcement.

EVM:

- remote deposit burn initiation,
- GMP receiver source validation,
- remote mint authorization,
- remote mint authorization canonical payload mismatch,
- remote mint authorization expiry/replay,
- remote mint execution acknowledgement,
- spoke burn cannot reduce hub liability before hub acceptance,
- guarded executor rejects unregistered target,
- contract guard `txGuard` and `afterTxGuard` are both enforced,
- asset guard valuation/withdraw path is required before allocation,
- Aave/Morpho/Uniswap guard-specific route tests,
- trader can execute approved route within limits,
- trader cannot execute expired, over-limit, or unauthorized route,
- trader cannot register guard or target,
- trader cannot mark local collateral as settled,
- local same-chain collateral deposit transfers USDC into SpokeVault before sending lock GMP,
- local collateral cannot be deployed into guarded strategy if reserves would be violated,
- replay protection,
- pause behavior.

Solana:

- PDA derivation and authority checks,
- native USDC token account validation,
- CCTP burn instruction account validation,
- remote mint authorization replay protection,
- remote mint authorization expiry,
- remote mint authorization destination chain/recipient/route mismatch,
- remote burn notice acceptance path,
- instruction guard rejects wrong program, wrong account owner, wrong token mint, wrong route id, or reused route nonce,
- asset guard must provide value and withdrawal instructions before allocation,
- local cap and pause behavior,
- burn-and-redeem instruction.

### 21.2 Integration Tests

- Stellar local deposit -> fUSD mint.
- Stellar local redeem -> USDC transfer.
- Remote deposit intent -> CCTP settlement -> fUSD mint on selected destination chain.
- Remote local deposit -> SpokeVault lock -> accepted spoke collateral -> fUSD mint on selected destination chain.
- Remote fUSD mint authorization after settlement.
- Remote mint execution acknowledgement -> pending supply becomes outstanding supply.
- Remote burn -> Stellar liability reduction -> CCTP redeem.
- Spoke burn cannot reserve USDC before hub burn acceptance.
- xycLoans/deFindex allocation -> yield report -> harvest.
- Allocation route execution rejects wrong route id, domain, strategy, or nonce.
- Trader-selected bridge route executes only if route is active and within limits.
- Local same-chain USDC deposit -> guarded lending allocation -> strategy report -> guarded withdraw.
- Accepted spoke collateral lock -> guarded local strategy deployment -> value report does not increase mint allowance.
- DEX swap/LP route cannot execute without registered contract/instruction guard and asset guard.
- Remote strategy report acceptance uses min(on-chain balance, withdraw quote, oracle value, risk report value).
- Solana deposit -> CCTP settlement on Stellar -> fUSD mint on selected destination chain.
- EVM deposit -> CCTP settlement on Stellar -> remote fUSD mint authorization.
- Frontend route state moves from `source_tx_confirmed` to `completed` without skipping required CCTP/GMP states.
- Backend workers retry idempotently after process restart.

### 21.3 Invariant Tests

Properties:

```text
liabilities never exceed collateral under all operation sequences
message hash cannot mint twice
remote supply mirror always equals canonical liability component
mint allowance equals settled unminted native USDC plus accepted unminted spoke-vault locks only
fast credit never increases mint allowance
strategy reports never increase mint allowance
generic GMP collateral claim never increases mint allowance
trader and manager actions never increase mint allowance
paused strategy cannot receive new funds
redeem cannot release more USDC than reserved
spoke redeem cannot reserve USDC before hub burn acceptance
per-chain reserves stay above configured floor during normal operations
protocol dust never contributes to mint allowance
```

### 21.4 Adversarial Tests

- duplicate CCTP attestation,
- forged GMP source,
- GMP arrives before CCTP,
- amount precision dust,
- remote router over-cap mint attempt,
- remote mint payload with wrong destination router/token/route id,
- remote mint acknowledgement from wrong source router,
- guarded executor direct unguarded call attempt,
- unauthorized trader route execution attempt,
- authorized trader attempts to lower reserves or increase caps,
- unauthorized trader attempts to settle spoke collateral,
- approved trader attempts route execution against wrong guard set,
- manager attempts to set fees above hard fee bounds,
- manager attempts to change fee recipient (must be rejected; fee_recipient absent from ManagerFeeConfig),
- guardian-only key attempts to propose Standard or RiskReducing governance action (must be rejected),
- same route_nonce submitted at two different ledgers does not produce two InFlightAllocation entries,
- execute_bridge_route rejects route where current_ledger > valid_until_ledger,
- cancel_mint_authorization by non-depositor Stellar address for cross-chain mint_auth_id reverts,
- notify_realized_yield called twice in same epoch accumulates against max_yield_per_epoch_6 cumulatively,
- daily redeem limit rolls over after redeem_window_ledgers without requiring manual governance reset,
- manager attempts to select non-whitelisted strategy,
- manager attempts to increase debt ceiling or chain cap,
- stale frontend quote after fee change is rejected,
- duplicate spoke collateral lock id,
- spoke collateral lock from wrong router, wrong vault, wrong USDC token, or wrong route kind,
- spoke collateral release before hub authorization,
- malicious approval to unguarded spender,
- DEX spot-price manipulation against LP valuation,
- lending health factor falls below guard threshold after transaction,
- direct spoke-to-spoke transfer attempt,
- strategy report tries to increase mint allowance,
- strategy report overstates one valuation input,
- stale strategy report,
- remote burn notice tries to release USDC before hub acceptance,
- fast-finality transfer tries to mint without insurance reserve,
- oracle stale,
- strategy withdrawal illiquidity,
- chain pause during pending deposit,
- emergency exit during partial failure.
- invalid Solana token account,
- invalid Stellar CCTP recipient encoding,
- Axelar message from correct gateway but wrong source address,
- frontend displays paused route and blocks transaction build,
- backend relayer submits duplicate attestation.

## 22. Deployment Phases

### Phase 0 - Repo And Tooling

- Soroban workspace.
- EVM workspace for remote routers.
- Solana Anchor/native Rust workspace for remote router program.
- Shared TypeScript SDK.
- Local test harness.
- dApp shell with EVM/Stellar/Solana wallet connectors.
- backend service skeleton with API, worker, indexer, and database migrations.

### Phase 1 - Stellar MVP

- `FusdToken`
- `VaultAccounting`
- `MintRedeemController`
- native Stellar USDC deposit/redeem through SAC
- pause/governance
- invariant tests

### Phase 2 - Stellar Yield

- `AllocationManager`
- `StrategyAdapterRegistry`
- `XycloansAdapter`, `DefindexAdapter` (`BlendAdapter` retained, not active — §8 status note)
- strategy valuation and withdrawals
- `SfUsdVault` or rewards module

### Phase 3 - CCTP

- inbound CCTP deposit
- outbound CCTP redeem
- CCTP relayer
- message hash registry
- six/seven decimal validation

### Phase 4 - Axelar

- GMP governance messages
- remote cap updates
- remote pause/unpause
- deposit settled acknowledgements
- remote mint authorization

### Phase 5 - Remote fUSD

- remote fUSD token controlled by custom hub-authorized routers
- remote routers
- Solana remote router program
- Solana remote fUSD SPL/Token-2022 mint if enabled
- remote supply mirror
- per-chain caps
- emergency remote burn/reconcile flows

### Phase 6 - Advanced Allocation

- guarded remote Aave/Morpho/DEX allocation
- Solana lending/DEX allocation after separate instruction-guard and asset-guard validation
- Gauntlet/risk-agent inputs
- automated bounded rebalancing
- public proof-of-reserves dashboard

### Phase 7 - Production dApp And Operations

- route quote service
- CCTP/GMP status service
- proof-of-reserves public dashboard
- frontend transfer recovery UI
- observability and alerting
- incident runbooks
- multi-region relayer deployment

## 23. Suggested Repository Structure

```text
contracts/
  soroban/
    fusd-token/
    vault-accounting/
    mint-redeem-controller/
    allocation-manager/
    strategy-adapter-registry/
    adapters/
      blend/
      aquarius/
      cctp/
      axelar/
    sfusd-vault/
  evm/
    src/
      RemoteRouter.sol
      RemoteFusd.sol
      AxelarReceiver.sol
      CctpBurner.sol
    test/
  solana/
    programs/
      remote-router/
      remote-fusd/
    tests/

packages/
  sdk/
  config/
  cctp-relayer/
  axelar-relayer/
  risk-agent/
  indexer/
  api/
  proof-publisher/

apps/
  web/
  admin/

docs/
  CROSS_CHAIN_FUSD_TECHNICAL_SPEC.md
  ARCHITECTURE_AUDIT_REVIEW.md
  threat-model.md
  accounting-invariants.md
  cctp-runbook.md
  axelar-runbook.md
  solana-runbook.md
  frontend-backend-architecture.md
```

## 24. Open Questions

1. Should fUSD itself earn yield, or should yield require staking into sfUSD?
2. Which exact xycLoans pool and deFindex vault(s) are approved for first deployment, and
   — for the deFindex vault specifically — what is its exact configured strategy set
   (must be confirmed to exclude Blend before registration)?
3. How should strategy losses be socialized: treasury reserve first, yield reserve second, then protocol pause?
4. What external proof should be published for Gauntlet/risk allocation updates?
5. Under what conditions (independent audit, backstop redesign) would a future Blend V3
   be re-evaluated as an additional strategy adapter?

## 25. Implementation Notes For Senior Review

- Start with one canonical precision and write conversion tests before any business logic.
- Use 6 decimals for fUSD cross-chain supply and keep Stellar USDC seven-decimal handling at the collateral boundary only.
- Make `VaultAccounting.check_invariant()` cheap enough to call after every state mutation.
- Do not allow adapters to mutate liability accounting.
- Do not allow relayers to pass user-controlled amounts that override parsed CCTP data.
- Keep remote chain minting disabled for each new spoke until canonical settlement, message replay protection, caps, pause controls, and supply reconciliation are validated.
- Use custom hub-authorized remote routers for production v1; do not enable Axelar ITS direct fUSD movement until separate design validation.
- Treat finalized CCTP settlement as the only default cross-chain source of mint allowance.
- Treat accepted spoke-vault lock proofs as the only non-CCTP local-spoke source of mint allowance, and only within chain-level local collateral caps.
- Require every remote mint, burn, allocation, and strategy report to use the canonical payload structs in section 11.
- Require every spoke collateral lock and release to use the canonical payload structs in section 11.
- Keep remote strategy reports separate from mint allowance.
- Route all spoke supply changes through hub authorization and reconciliation.
- Treat strategy valuation as adversarial until independently withdrawable.
- Keep Manager powers bounded to fee settings inside hard bounds and strategy selection inside an existing whitelist.
- Design every pending state with expiry and cancellation paths.
- Prefer boring, explicit role storage over clever generalized ACL for the first production version.

## 26. Source References

- Stellar token interface / SEP-41: https://developers.stellar.org/docs/tokens/token-interface
- Stellar Asset Contract: https://developers.stellar.org/docs/tokens/stellar-asset-contract
- Circle CCTP on Stellar: https://developers.circle.com/cctp/references/stellar
- Circle Stellar CCTP contracts/interfaces: https://developers.circle.com/cctp/references/stellar-contracts
- Circle CCTP technical guide: https://developers.circle.com/cctp/references/technical-guide
- Axelar message flow: https://docs.axelar.dev/learn/network/flow/
