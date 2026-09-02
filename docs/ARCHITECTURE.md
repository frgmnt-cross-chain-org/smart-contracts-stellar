# frgmnt fUSD — System Architecture

> This is the primary architecture reference. For the detailed contract specification see
> [`CROSS_CHAIN_FUSD_TECHNICAL_SPEC.md`](CROSS_CHAIN_FUSD_TECHNICAL_SPEC.md). For the
> live testnet deployment and demo transactions see [`POC_GUIDE.md`](POC_GUIDE.md).

---

## Table of Contents

1. [What is fUSD?](#1-what-is-fusd)
2. [Full Technical Stack](#2-full-technical-stack)
3. [Why Stellar as the Hub](#3-why-stellar-as-the-hub)
4. [Stellar Integration — Deep Dive](#4-stellar-integration--deep-dive)
   - 4.1 [Soroban Smart Contracts](#41-soroban-smart-contracts)
   - 4.2 [SEP-41 Token Standard](#42-sep-41-token-standard)
   - 4.3 [Stellar USDC via SAC](#43-stellar-usdc-via-sac)
   - 4.4 [CCTP v2 on Stellar](#44-cctp-v2-on-stellar)
   - 4.5 [Axelar GMP on Stellar](#45-axelar-gmp-on-stellar)
   - 4.6 [Decimal Handling](#46-decimal-handling)
5. [Smart Contract Architecture](#5-smart-contract-architecture)
   - 5.1 [Soroban Contracts (Hub)](#51-soroban-contracts-hub)
   - 5.2 [EVM Contracts (Spoke)](#52-evm-contracts-spoke)
   - 5.3 [Solana Programs (Spoke)](#53-solana-programs-spoke)
   - 5.4 [Contract Interaction Map](#54-contract-interaction-map)
6. [Cross-Chain Flows](#6-cross-chain-flows)
   - 6.1 [Deposit → Mint fUSD](#61-deposit--mint-fusd)
   - 6.2 [Redeem fUSD → USDC](#62-redeem-fusd--usdc)
   - 6.3 [Remote Mint (Stellar deposit → EVM fUSD)](#63-remote-mint-stellar-deposit--evm-fusd)
   - 6.4 [Strategy Allocation](#64-strategy-allocation)
7. [Solvency Invariant](#7-solvency-invariant)
8. [Governance and Roles](#8-governance-and-roles)
9. [dApp Architecture](#9-dapp-architecture)
   - 9.1 [Frontend Stack](#91-frontend-stack)
   - 9.2 [Backend and Indexer](#92-backend-and-indexer)
   - 9.3 [Transaction State Machine](#93-transaction-state-machine)
10. [Security Properties](#10-security-properties)

---

## 1. What is fUSD?

fUSD is a **native-USDC-backed stablecoin** that lives simultaneously on Stellar, EVM chains
(Base, Ethereum, Arbitrum, OP Mainnet), and Solana. Users deposit native USDC on any
supported chain and receive fUSD at 1:1. Idle USDC is deployed into risk-approved lending
and DEX strategies; yield is passed back to fUSD holders through the `sfUSD` vault.

**The key design choice**: Stellar is the single source of truth for all accounting. No spoke
chain — not even Base — can mint or burn fUSD without a verified Stellar hub authorization.

---

## 2. Full Technical Stack

```
┌─────────────────────────────────────────────────────────────────────────────────────┐
│                              LAYER 5 — GOVERNANCE                                   │
│  GovernanceController (Soroban)                                                     │
│  Timelock: 48h standard · 0-6h risk-reducing · immediate guardian                  │
│  Roles: Admin · Timelock · Guardian · Manager · Trader · AllocationOperator        │
└──────────────────────────────────────┬──────────────────────────────────────────────┘
                                       │ proposals / execution
┌──────────────────────────────────────▼──────────────────────────────────────────────┐
│                        LAYER 4 — STELLAR CANONICAL HUB                              │
│                                                                                     │
│  ┌─────────────────────┐  ┌───────────────────────┐  ┌────────────────────────┐   │
│  │   FusdToken         │  │   VaultAccounting     │  │  MintRedeemController  │   │
│  │   (SEP-41 token)    │  │   (accounting state   │  │  (user entry point)    │   │
│  │                     │  │    machine)           │  │                        │   │
│  │  mint / burn        │  │  GlobalState          │  │  deposit_usdc          │   │
│  │  transfer           │  │  ChainState[]         │  │  redeem_local          │   │
│  │  pause              │  │  StrategyState[]      │  │  authorize_remote_mint │   │
│  └─────────────────────┘  └───────────────────────┘  └────────────────────────┘   │
│                                                                                     │
│  Runtime: Soroban (Rust, wasm32v1-none, no_std)                                     │
│  Storage: Soroban Persistent + Instance ledger entries                              │
│  Token standard: SEP-41                                                             │
└──────────────────────┬──────────────────────────────────────────────────────────────┘
                       │                   │
          CCTP v2      │                   │  Axelar GMP
   (native USDC burn/  │                   │  (authenticated
    mint, domain 27)   │                   │   instructions)
                       │                   │
┌──────────────────────▼───────────────────▼──────────────────────────────────────────┐
│                        LAYER 3 — TRANSPORT LAYER                                    │
│                                                                                     │
│  Circle CCTP v2                             Axelar GMP                              │
│  ─────────────────                          ──────────────────────────              │
│  TokenMessengerMinter   (Stellar)           Axelar Gateway (each chain)             │
│  MessageTransmitter     (Stellar)           Axelar Gas Service (EVM/Solana)         │
│  CctpForwarder          (Stellar)           Relayer network (off-chain)             │
│  Circle Iris attestation API                                                        │
│                                                                                     │
│  Carries: native USDC value                 Carries: authenticated instructions     │
│  Does NOT carry: instructions               Does NOT carry: USDC collateral         │
└──────────────────────┬───────────────────────────────────────────────────────────────┘
                       │
          ┌────────────┴────────────┬──────────────────────┐
          │                         │                      │
┌─────────▼──────────┐   ┌──────────▼─────────┐  ┌────────▼────────────┐
│  EVM SPOKES        │   │  EVM SPOKES        │  │  SOLANA SPOKE       │
│  Base (domain 6)   │   │  Ethereum (dom 0)  │  │  (domain 5)         │
│  Arbitrum (dom 3)  │   │  OP Main (dom 2)   │  │                     │
│                    │   │                    │  │  remote_router      │
│  RemoteRouter.sol  │   │  RemoteRouter.sol  │  │  (Rust/Anchor)      │
│  RemoteFusd.sol    │   │  RemoteFusd.sol    │  │  SPL fUSD mint      │
│  StrategyAdapter   │   │  StrategyAdapter   │  │                     │
│  (Aave/Morpho/Uni) │   │  (Aave/Morpho/Uni) │  │  Circle CCTP burn   │
└────────────────────┘   └────────────────────┘  └─────────────────────┘
```

### Layer responsibilities

| Layer | Technology | Responsibility |
|-------|-----------|----------------|
| Hub contracts | Soroban (Rust → WASM) | All canonical accounting; mint/burn authority |
| Token standard | SEP-41 | Stellar-native fUSD interface |
| USDC collateral | Stellar Asset Contract (SAC) | Only source of Stellar-side USDC |
| Value transport | Circle CCTP v2 | Moving native USDC between chains |
| Message transport | Axelar GMP | Authenticated instructions, acks, allocation signals |
| EVM spokes | Solidity / OpenZeppelin | ERC-20 fUSD, deposit/redeem routing |
| Solana spoke | Rust/Anchor + SPL Token | SPL fUSD, CCTP burn, GMP receive |
| dApp | Next.js + TypeScript | Multi-wallet UX across all runtimes |
| Indexer | Node.js / TypeScript | CCTP attestation tracking, GMP status, proof-of-backing |

---

## 3. Why Stellar as the Hub

| Concern | Stellar answer |
|---------|---------------|
| Low-cost, high-throughput settlement | ~1.25 s ledger time, ~$0.00001/tx fees |
| Native USDC without wrapping | Stellar Asset Contract exposes Circle's native Stellar USDC |
| CCTP domain 27 | Circle officially supports Stellar in CCTP v2 |
| Soroban smart contracts | Sandboxed Rust/WASM contracts with deterministic gas, rich types, built-in storage TTLs |
| Immutable accounting authority | A single Soroban `VaultAccounting` contract is the only place that can change liability counts — no spoke can override it |
| SEP-41 composability | Stellar wallets (Freighter, Lobstr, xBull) natively handle SEP-41 tokens; no custom bridge UI needed for Stellar users |

---

## 4. Stellar Integration — Deep Dive

### 4.1 Soroban Smart Contracts

Soroban is Stellar's smart contract platform. Contracts are:

- Written in **Rust**, compiled to **wasm32v1-none** with `no_std`
- Deployed as immutable WASM blobs, identified by a 32-byte hash
- Invoked through Stellar transactions with the `InvokeHostFunction` operation
- Storage is keyed per-contract using Soroban's ledger entry model (Persistent / Instance / Temporary TTLs)

Build target:

```bash
cargo build --workspace --target wasm32v1-none --release
stellar contract optimize --wasm target/wasm32v1-none/release/<contract>.wasm
```

**`wasm32v1-none`, not `wasm32-unknown-unknown`** — see `docs/POC_GUIDE.md` "Build
reproducibility" for why (on current Rust, `wasm32-unknown-unknown` produces a module
the Soroban host rejects at deploy time; `cargo test` never catches this since it runs
natively). Verified by deploying to Stellar testnet — see
`docs/CROSS_CHAIN_FUSD_TECHNICAL_SPEC.md` §8.6.

No cross-crate build order dependency: every hub contract that calls another declares a
hand-written `contractclient` interface trait for the subset of that contract's public
interface it actually uses, rather than importing a compiled ABI via `contractimport!`
— so `fusd-token`, `vault-accounting`, `mint-redeem-controller`, and the strategy layer
(`allocation-manager` + adapters) all compile independently, in any order, from a clean
checkout.

### 4.2 SEP-41 Token Standard

SEP-41 is Stellar's token interface specification (analogous to ERC-20). The `FusdToken`
contract implements it fully:

```rust
// User-callable SEP-41 interface
fn balance(e: Env, id: Address) -> i128
fn transfer(e: Env, from: Address, to: Address, amount: i128)
fn transfer_from(e: Env, spender: Address, from: Address, to: Address, amount: i128)
fn allowance(e: Env, from: Address, spender: Address) -> i128
fn approve(e: Env, from: Address, spender: Address, amount: i128, expiration_ledger: u32)
fn burn(e: Env, from: Address, amount: i128)
fn burn_from(e: Env, spender: Address, from: Address, amount: i128)
fn decimals(e: Env) -> u32                 // returns 6
fn name(e: Env) -> String                  // "Frgmnt fUSD"
fn symbol(e: Env) -> String                // "fUSD"

// Controller-only (not SEP-41 required, but needed by protocol)
fn mint(e: Env, controller: Address, to: Address, amount: i128)
fn controller_burn(e: Env, controller: Address, from: Address, amount: i128)
fn pause(e: Env, admin: Address)
fn unpause(e: Env, admin: Address)
```

Key SEP-41 constraint: Stellar wallets call `transfer`, `burn`, etc. with the **user's
Stellar keypair as the authorizer**. Soroban's auth model enforces this natively — no
separate `approve + transferFrom` dance is required for simple wallet-to-wallet transfers.

### 4.3 Stellar USDC via SAC

USDC on Stellar is a **Stellar Asset Contract (SAC)** — a Soroban wrapper around a native
Stellar asset issued by Centre/Circle. There is no separate "wrapped USDC" token.

```
Circle Stellar issuer account
        │
        │ native Stellar asset: USDC:GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN
        │
        ▼
Stellar Asset Contract (SAC)
        │
        │ Soroban SEP-41 interface
        │
        ▼
MintRedeemController deposits() calls usdc_sac.transfer(user → controller)
```

The SAC address is fixed at network level; the `hub_cctp_domain` and SAC address are
stored in `GlobalState` — **never hardcoded** in contract logic.

### 4.4 CCTP v2 on Stellar

Circle's Cross-Chain Transfer Protocol (CCTP) lets native USDC burn on one chain and
mint on another. Stellar is CCTP **domain 27**.

Stellar's CCTP stack (deployed by Circle):

```
┌────────────────────────────────────────────────────────────────┐
│  Circle's Stellar CCTP Contracts                               │
│                                                                │
│  TokenMessengerMinter   ─── burns/mints native Stellar USDC   │
│  MessageTransmitter     ─── packs/unpacks CCTP messages        │
│  CctpForwarder          ─── routes minted USDC to a recipient  │
│                              when mintRecipient is a contract  │
└────────────────────────────────────────────────────────────────┘
```

**Receiving USDC on Stellar from an EVM chain:**

```
EVM chain (e.g., Base)
  1. User calls CCTP burn: burn 100 USDC, mintRecipient = Stellar hub address
  2. Circle Iris attests the burn (off-chain API, ~20 s)

Stellar
  3. Relayer submits attestation to MessageTransmitter
  4. TokenMessengerMinter mints 100 USDC to hub (or CctpForwarder if needed)
  5. MintRedeemController calls record_inbound_settlement()
     → amount is computed as (balance_after - balance_before) ← balance delta, NOT a param
     → VaultAccounting.mint_allowance_6 += net_received
```

**Critical**: the hub never trusts a relayer-supplied amount. It reads its own USDC
balance before and after the CCTP `receiveMessage` call.

**Sending USDC from Stellar to an EVM chain (redemption):**

```
Stellar
  1. MintRedeemController computes net_out = burned_fusd - fee
  2. Calls CCTP burn: burn net_out USDC from hub address, mintRecipient = EVM user address
  3. Circle Iris attests the Stellar burn
  4. Relayer submits attestation to EVM MessageTransmitter
  5. User receives native USDC on EVM
```

**CCTP address fields on Stellar are 32 bytes.** An EVM address (20 bytes) is
left-padded to 32 bytes. A Stellar account (32 bytes) maps directly. When the
recipient is a Stellar contract, `CctpForwarder` is used to route the mint correctly.

### 4.5 Axelar GMP on Stellar

Axelar GMP (General Message Passing) delivers **authenticated instructions** between chains.
It does **not** move USDC — CCTP does that.

```
Stellar hub
  │
  │  Axelar GMP — carries instruction payloads
  │
  ├──► EVM spoke: "authorize remote mint for Alice, 100 fUSD, expiry 1000 ledgers"
  │    EVM remote router calls axelarGateway.validateContractCall() before executing
  │
  ◄──── EVM spoke: "remote mint executed, mintAuthId=0xABC"
  │    hub calls confirm_remote_mint_executed()
  │
  ├──── EVM spoke: "spoke collateral locked, vault=X, amount=500 USDC"
       hub calls record_spoke_collateral_locked()
```

Axelar security model on each chain:
- The **Axelar Gateway contract** (on EVM) or **Axelar Gateway program** (Solana) is the
  trust anchor. Messages are valid only if `validateContractCall()` returns true.
- The hub allowlists source chain names and source contract addresses per spoke. Messages
  from unknown sources are rejected.
- GMP carries no financial value by itself. It only changes state on the receiving contract.

### 4.6 Decimal Handling

Stellar USDC has **7 decimals**. CCTP and all hub accounting use **6 decimals**.

```
User deposits 100.1234567 USDC (Stellar, 7-dec = 1_001_234_567 raw)
                │
                │ MintRedeemController floors the 7th decimal digit
                │
                ▼
     1_001_234_560  (still 7-dec) → floor to 6-dec →  1_001_234_5 = 100.123456 USDC_6
                │
                │ 0.000001 USDC dust stays in protocol_dust_usdc_7
                │
                ▼
     mint_allowance_6 += 100_123_456  (100.123456 fUSD minted at 6 decimals)
```

fUSD is **6 decimals on all chains** — Stellar, EVM, and Solana. This keeps
`1 fUSD unit == 1 USDC_6 unit` everywhere.

---

## 5. Smart Contract Architecture

### 5.1 Soroban Contracts (Hub)

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                        STELLAR HUB — SOROBAN CONTRACTS                       │
│                                                                              │
│  ┌──────────────────────────────────────────────────────────────────────┐   │
│  │  FusdToken  (fusd-token/src/lib.rs)                                  │   │
│  │                                                                      │   │
│  │  Storage: Balance(Address) · TotalSupply · Admin · Controller        │   │
│  │           Paused · Allowance(Address,Address)                        │   │
│  │                                                                      │   │
│  │  Auth model:                                                         │   │
│  │    transfer/burn   ← requires from.require_auth()                    │   │
│  │    mint            ← requires controller.require_auth()              │   │
│  │    controller_burn ← requires controller.require_auth()              │   │
│  │    pause/unpause   ← requires admin.require_auth()                   │   │
│  └───────────────────────────────┬──────────────────────────────────────┘   │
│                                  │ mint() / controller_burn()               │
│  ┌───────────────────────────────▼──────────────────────────────────────┐   │
│  │  MintRedeemController  (mint-redeem-controller/src/lib.rs)           │   │
│  │                                                                      │   │
│  │  User entry points:                                                  │   │
│  │    deposit_usdc(user, amount_7, min_fee_version)                     │   │
│  │      → floors dust → record_local_deposit() → mint_liability()       │   │
│  │        → fusd.mint(user)                                             │   │
│  │                                                                      │   │
│  │    redeem_local(user, fusd_amount, min_fee_version)                  │   │
│  │      → fusd.controller_burn() → burn_liability_for_redemption()      │   │
│  │        → CCTP burn (usdc → user) → mark_outbound_sent()             │   │
│  │                                                                      │   │
│  │    receive_cctp_settlement(msg_hash, min_fee_version)                │   │
│  │      → reads balance delta → record_inbound_settlement()             │   │
│  │        → authorize_remote_mint() [if remote dest]                    │   │
│  │                                                                      │   │
│  │  Fee config:                                                         │   │
│  │    FeeConfig { mint_fee_bps, redeem_fee_bps, recipient, version }   │   │
│  │    set by admin (recipient) or manager (rates only)                  │   │
│  └───────────────────────────────┬──────────────────────────────────────┘   │
│                                  │ all accounting mutations                  │
│  ┌───────────────────────────────▼──────────────────────────────────────┐   │
│  │  VaultAccounting  (vault-accounting/src/lib.rs)                      │   │
│  │                                                                      │   │
│  │  GlobalState (one instance)                                          │   │
│  │    total_liabilities_6         all fUSD minted across all chains     │   │
│  │    settled_idle_usdc_6         USDC in Stellar hub                   │   │
│  │    settled_spoke_escrow_usdc_6 USDC locked in spoke vaults           │   │
│  │    total_strategy_value_6      conservative strategy NAV             │   │
│  │    mint_allowance_6            unspent deposit credit                │   │
│  │    pending_outbound_usdc_6     reserved for in-flight redemptions    │   │
│  │    pending_fast_credit_6       fast-credit backed by insurance reserve│  │
│  │    required_reserve_bps        10% default liquidity floor           │   │
│  │                                                                      │   │
│  │  ChainState (one per connected chain)                                │   │
│  │    cctp_domain · axelar_chain_name · remote_router (32B)             │   │
│  │    max_mint_6 · outstanding_supply_6 · pending_mint_auth_6           │   │
│  │    daily_redeem_limit_6 · redeemed_today_6                           │   │
│  │                                                                      │   │
│  │  Solvency invariant checked after EVERY mutation:                    │   │
│  │    assets = idle + spoke_escrow + strategy_value                     │   │
│  │    assets >= liabilities  AND  idle >= liabilities * reserve_bps     │   │
│  └──────────────────────────────────────────────────────────────────────┘   │
│                                                                              │
│  AllocationManager · XycloansAdapter · DefindexAdapter (implemented)                       │
│  BlendAdapter (retained, not active) · (Future) GovernanceController · SfUsdVault           │
└──────────────────────────────────────────────────────────────────────────────┘
```

**Storage TTLs** — Soroban storage expires unless extended. The protocol uses:

| Data | Storage class | TTL strategy |
|------|--------------|--------------|
| GlobalState | Persistent | bumped on every write |
| ChainState | Persistent | bumped on every write |
| consumed CCTP hashes | Persistent | 5-year minimum (replay protection) |
| MintAuth records | Persistent | 5-year minimum |
| Instance (contract metadata) | Instance | bumped at deploy time |

### 5.2 EVM Contracts (Spoke)

```
┌────────────────────────────────────────────────────────────────────────┐
│  EVM SPOKE  (contracts/evm/src/)                                       │
│                                                                        │
│  ┌─────────────────────────────────────────────────────────────────┐  │
│  │  RemoteFusd.sol  (ERC-20)                                       │  │
│  │                                                                 │  │
│  │  Immutable router reference set at deploy                       │  │
│  │  mint()  ← only router                                          │  │
│  │  burn()  ← only router                                          │  │
│  │  transfer/approve ← standard ERC-20, any holder                │  │
│  └────────────────────────────┬────────────────────────────────────┘  │
│                               │ mint / burn                            │
│  ┌────────────────────────────▼────────────────────────────────────┐  │
│  │  RemoteRouter.sol                                               │  │
│  │                                                                 │  │
│  │  depositAndBridge(amount, destChain, destRecipient)             │  │
│  │    1. transfer USDC from user                                   │  │
│  │    2. CCTP burn (USDC → Stellar domain 27)                      │  │
│  │                                                                 │  │
│  │  execute(srcChain, srcAddr, payload)   ← Axelar GMP callback    │  │
│  │    decode payload selector:                                     │  │
│  │      MSG_REMOTE_MINT_AUTH   → validate + mint fUSD to recipient  │  │
│  │      MSG_REMOTE_MINT_EXEC   → ack (no-op for v1)                │  │
│  │    security: validateContractCall() FIRST (CEI order)           │  │
│  │    replay guard: usedMintAuths[mintAuthId] = true               │  │
│  │    expiry guard: block.timestamp < mintAuth.expiryTimestamp      │  │
│  │    source guard: srcChain == hubChain && srcAddr == hubAddress   │  │
│  │                                                                 │  │
│  │  burnRemoteFusdAndRedeem(amount, destDomain, usdcRecipient)     │  │
│  │    1. burn fUSD from user                                       │  │
│  │    2. CCTP burn USDC from router reserve → Stellar             │  │
│  └─────────────────────────────────────────────────────────────────┘  │
│                                                                        │
│  Interfaces: ICCTP.sol · IAxelarGateway.sol                           │
│  Mocks (test only): MockCCTP.sol · MockAxelarGateway.sol              │
└────────────────────────────────────────────────────────────────────────┘
```

### 5.3 Solana Programs (Spoke)

Planned for phase 2. Same hub-authorized pattern as EVM:

```
Solana remote_router (Anchor/Rust)
  ├── DepositAndBurnUsdc    → CCTP burn to domain 27
  ├── ConsumeGmpMessage     → Axelar-authenticated execute
  │     └── AuthorizeRemoteMint → SPL fUSD mint to recipient
  └── BurnRemoteFusdAndRedeem → SPL fUSD burn + CCTP USDC burn
```

SPL fUSD mint authority is the router PDA, locked to hub-only authorization.

### 5.4 Contract Interaction Map

```
User (Stellar)
    │
    │ deposit_usdc / redeem_local
    ▼
MintRedeemController ──────► VaultAccounting  ────► check_invariant_gs (every call)
    │                               │
    │ mint / burn                   │ GlobalState · ChainState
    ▼                               │ updated atomically
FusdToken (SEP-41)                  │
    │                               │
    │ CCTP burn / receive           │
    ▼                               │
Circle CCTP v2 ◄────────────────────┘
    │
    │ Axelar GMP (RemoteMintAuth)
    ▼
RemoteRouter (EVM/Solana)
    │
    │ mint
    ▼
RemoteFusd (ERC-20 / SPL)
    │
    │ Axelar GMP (RemoteMintExecuted ack)
    ▼
MintRedeemController.confirm_remote_mint_executed()
    │
    ▼
VaultAccounting.confirm_remote_mint_executed()
  (clears pending_mint_auth_6 on ChainState)
```

---

## 6. Cross-Chain Flows

### 6.1 Deposit → Mint fUSD

**Case A: User is on an EVM chain, wants fUSD on EVM**

```
Alice (Base)                 RemoteRouter (Base)          Stellar Hub
    │                               │                          │
    ├─ approve USDC ───────────────►│                          │
    ├─ depositAndBridge(100 USDC) ──►│                          │
    │                               ├─ USDC.transferFrom(Alice)│
    │                               ├─ CCTP burn ─────────────►│ (domain 27)
    │                               │   mintRecipient=hub       │
    │                               │                          │ MessageTransmitter.receiveMessage()
    │                               │                          │ balance_after - balance_before = 100M
    │                               │                          │ VaultAccounting.record_inbound_settlement()
    │                               │                          │ VaultAccounting.authorize_remote_mint()
    │                               │◄──── Axelar GMP ─────────┤
    │                               │  RemoteMintAuth {         │
    │                               │    mintAuthId: 0xABC,     │
    │                               │    to: Alice,             │
    │                               │    amount: 100_000_000,   │
    │                               │    expiry: T+10min        │
    │                               │  }                        │
    │                               │                          │
    │                               ├─ gateway.validateContractCall() ✓
    │                               ├─ usedMintAuths[0xABC] = true
    │                               ├─ fusd.mint(Alice, 100_000_000)
    │◄─ 100 fUSD ───────────────────┤                          │
    │                               ├─ Axelar GMP (ack) ──────►│
    │                               │  RemoteMintExecuted       │
    │                               │                          │ confirm_remote_mint_executed()
    │                               │                          │ pending_mint_auth_6 -= 100M
    │                               │                          │ outstanding_supply_6 += 100M
```

**Case B: User deposits on Stellar, wants fUSD on Stellar**

```
Alice (Stellar)              MintRedeemController         VaultAccounting
    │                               │                          │
    ├─ USDC.approve(controller) ───►│                          │
    ├─ deposit_usdc(100 USDC) ──────►│                          │
    │                               ├─ usdc.transfer(Alice→hub)│
    │                               ├─ record_local_deposit() ─►│
    │                               │                          │ settled_idle += 100M
    │                               │                          │ mint_allowance += 100M
    │                               ├─ mint_liability() ───────►│
    │                               │                          │ total_liabilities += 100M
    │                               │                          │ mint_allowance -= 100M
    │                               │                          │ invariant check ✓
    │                               ├─ fusd.mint(Alice, 100M)  │
    │◄─ 100 fUSD ───────────────────┤                          │
```

### 6.2 Redeem fUSD → USDC

```
Alice (Stellar)              MintRedeemController         VaultAccounting
    │                               │                          │
    ├─ redeem_local(500 fUSD) ──────►│                          │
    │                               ├─ fusd.controller_burn()  │
    │                               ├─ burn_liability() ───────►│
    │                               │                          │ total_liabilities -= 500M
    │                               │                          │ net_out = 500M - fee (1.5M at 0.3%)
    │                               │                          │ settled_idle -= net_out (498.5M)
    │                               │                          │ pending_outbound += net_out
    │                               │                          │ invariant check ✓
    │                               ├─ CCTP burn (498.5M USDC → Alice on Stellar or EVM)
    │                               ├─ mark_outbound_sent() ───►│
    │                               │                          │ pending_outbound -= 498.5M
    │◄─ 498.5 USDC ─────────────────┤                          │ (1.5M fee stays in settled_idle)
```

### 6.3 Remote Mint (Stellar deposit → EVM fUSD)

```
Alice (Stellar)              MintRedeemController    VaultAccounting     RemoteRouter (Base)
    │                               │                     │                     │
    ├─ deposit_usdc(100 USDC) ──────►│                     │                     │
    │                               ├─ record_local_dep.─►│                     │
    │                               │                     │ mint_allowance += 100M
    │                               ├─ authorize_remote_mint(chain=Base, to=AliceEVM, 100M)
    │                               │                     │                     │
    │                               │                     │ pending_mint_auth_6 += 100M
    │                               │                     │ mint_allowance -= 100M
    │                               │──── Axelar GMP ─────────────────────────►│
    │                               │     RemoteMintAuth                        │
    │                               │                     │                     ├─ validate ✓
    │                               │                     │                     ├─ mint(AliceEVM, 100M)
    │                               │◄───────────────────────── Axelar GMP ────┤
    │                               │     RemoteMintExecuted                    │
    │                               │ confirm() ──────────►│                    │
    │                               │                     │ pending_mint -= 100M
    │                               │                     │ outstanding_supply_6 += 100M
```

### 6.4 Strategy Allocation

```
AllocationManager (Stellar)        CCTP          xycLoans / deFindex (Stellar)
        │                            │                       │
        ├─ approve route via governance
        ├─ execute_bridge_route(route_id, route_version, 500M USDC)
        │     checks: route exists · version matches · not expired · cap not exceeded
        │                            │                       │
        │                            │ if same-chain:        │
        │                            │─── USDC transfer ─────►
        │                            │                       │ pool/vault deposit
        │                            │                       │ → yield accumulates
        │                            │                       │
        │◄─ report_strategy_value() ─┤──────────────────────┤
        │   strategy_id, value_6     │                       │
        │   (live, never inflates mint_allowance_6)          │
        ├─ VaultAccounting.report_strategy_value()           │
        │   total_strategy_value_6 = reported value          │
        │   invariant re-checked                             │
```

Blend V1/V2 is not used here — see [`CROSS_CHAIN_FUSD_TECHNICAL_SPEC.md` §8](CROSS_CHAIN_FUSD_TECHNICAL_SPEC.md#8-stellar-strategy-adapters)
for why (Blend V2's backstop was drained in the August 2026 Comet AMM exploit and cannot
be repaired) and for the xycLoans/deFindex adapter design.

---

## 7. Solvency Invariant

`VaultAccounting.check_invariant_gs()` is called at the end of every state mutation.
If it fails, the entire transaction panics and no state is committed.

```
                    ┌─────────────────────────────────────────────┐
                    │          ASSETS (6-decimal USDC)            │
                    │                                             │
                    │  settled_idle_usdc_6                        │
                    │  + settled_spoke_escrow_usdc_6              │
                    │  + total_strategy_value_6                   │
                    │                                             │
                    │  ≥  total_liabilities_6   (basic solvency)  │
                    │                                             │
                    │  AND                                        │
                    │                                             │
                    │  settled_idle_usdc_6                        │
                    │  ≥  total_liabilities_6 * reserve_bps       │
                    │     ─────────────────────────────           │
                    │              10_000          (liquidity floor)│
                    └─────────────────────────────────────────────┘
```

In Rust:

```rust
fn check_invariant_gs(gs: &GlobalState) -> bool {
    // settled_idle has already been reduced by pending_outbound in
    // burn_liability_for_redemption — no double-subtraction.
    let total_assets = gs.settled_idle_usdc_6
        .saturating_add(gs.settled_spoke_escrow_usdc_6)
        .saturating_add(gs.total_strategy_value_6);

    let basic_solvency = total_assets >= gs.total_liabilities_6;

    let required_idle = gs.total_liabilities_6
        .saturating_mul(gs.required_reserve_bps as i128) / 10_000;
    let liquidity_ok = gs.settled_idle_usdc_6 >= required_idle;

    basic_solvency && liquidity_ok
}
```

---

## 8. Governance and Roles

```
                  ┌──────────────────────────────────────────────────────┐
                  │  GovernanceController  (Soroban)                     │
                  │                                                      │
                  │  Timelock delays:                                    │
                  │    Standard      48 h  (~138,240 ledgers)            │
                  │    RiskReducing  0-6 h (including emergency unpause) │
                  │    Immediate     0     (guardian pause / exit)       │
                  └────────────────────────────┬─────────────────────────┘
                                               │
                 ┌─────────────────────────────┼──────────────────────────┐
                 │                             │                          │
         ┌───────▼────────┐          ┌─────────▼────────┐      ┌─────────▼──────┐
         │   Timelock     │          │    Guardian       │      │    Manager     │
         │                │          │                   │      │                │
         │ add chains     │          │ emergency pause   │      │ set fee rates  │
         │ upgrade code   │          │ emergency exit    │      │ pause strategy │
         │ raise caps     │          │ self-revoke       │      │ select whitel. │
         │ register strat │          │ (within ~1h)      │      │ strategies     │
         └───────┬────────┘          └───────────────────┘      └────────────────┘
                 │
         ┌───────▼────────┐
         │    Trader /    │
         │    Alloc Op    │
         │                │
         │ execute bridge │
         │ routes (within │
         │ approved caps) │
         │ run strategies │
         └────────────────┘
```

**What each role can never do:**

| Role | Hard limits |
|------|------------|
| Guardian | Cannot mint, raise caps, upgrade, or add chains — even during incident |
| Manager | Cannot set fee_recipient, add strategies, mint, or alter liabilities |
| Trader | Cannot register routes/guards, change caps/reserves, or create mint allowance |
| CCTP Relayer | Cannot alter amount or recipient of the CCTP message. Implemented today as a single admin-appointed address (`MintRedeemController.set_relayer`/`receive_cctp_settlement`) — this bounds *who* can submit a settlement, but the credited amount is still a caller-supplied PoC mock (`mock_net_received_6`) pending real Stellar CCTP `receive_message` integration and balance-delta computation; do not treat the amount as trustworthy in production until that lands |
| Axelar Relayer | Gateway verification is the security boundary; relayer is operational only |

---

## 9. dApp Architecture

### 9.1 Frontend Stack

```
┌────────────────────────────────────────────────────────────────────────────┐
│  apps/web/  (Next.js · TypeScript · TanStack Query)                        │
│                                                                            │
│  ┌─────────────────────────────────────────────────────────────────────┐  │
│  │  ROUTES                                                             │  │
│  │  /mint       /redeem      /stake      /portfolio                    │  │
│  │  /proof      /governance                                            │  │
│  └────────────────────────────────┬────────────────────────────────────┘  │
│                                   │                                        │
│  ┌────────────────────────────────▼────────────────────────────────────┐  │
│  │  COMPONENTS                                                         │  │
│  │  ChainSelector        which chain to deposit from / receive to      │  │
│  │  RouteQuote           shows fee, slippage, estimated CCTP time      │  │
│  │  TransactionTimeline  live status: CCTP pending → GMP → minted      │  │
│  │  WalletSwitcher       manages EVM + Stellar + Solana sessions       │  │
│  │  LiquidityWarning     shows if redemption will exceed daily limit   │  │
│  │  ProofOfBacking       on-chain backed supply attestation            │  │
│  └────────────────────────────────┬────────────────────────────────────┘  │
│                                   │                                        │
│  ┌────────────────────────────────▼────────────────────────────────────┐  │
│  │  CLIENT LIBS                                                        │  │
│  │  evmClient       wagmi + viem, RainbowKit                           │  │
│  │  stellarClient   Stellar SDK, Freighter/Lobstr wallet              │  │
│  │  solanaClient    @solana/web3.js, Wallet Adapter                    │  │
│  │  cctpStatus      polls Circle Iris API for attestation status       │  │
│  │  axelarStatus    polls Axelar API for GMP delivery status           │  │
│  │  amountConversion 6-dec / 7-dec conversion, fee math               │  │
│  └─────────────────────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────────────────────┘
```

**Multi-wallet session model:**

The dApp can hold sessions for up to three runtimes simultaneously. This is required
when a user deposits USDC from Base (EVM session) but wants fUSD on Stellar (Stellar session).

```ts
type WalletSession =
  | { runtime: "evm";     chainId: number;  address: `0x${string}` }
  | { runtime: "stellar"; publicKey: string; contractAddress?: string }
  | { runtime: "solana";  publicKey: string }

interface AppSession {
  source: WalletSession
  destination: WalletSession
}
```

### 9.2 Backend and Indexer

```
┌────────────────────────────────────────────────────────────────────────────┐
│  Backend / Indexer  (Node.js · TypeScript · PostgreSQL)                    │
│                                                                            │
│  ┌──────────────────────────────────────────────────────────────────────┐ │
│  │  CCTP Relayer Service                                                │ │
│  │    polls Circle Iris API for pending attestations                    │ │
│  │    submits receiveMessage() to destination chain                     │ │
│  │    writes tx hash + status to DB                                     │ │
│  └──────────────────────────┬───────────────────────────────────────────┘ │
│                             │                                              │
│  ┌──────────────────────────▼───────────────────────────────────────────┐ │
│  │  Axelar GMP Monitor                                                  │ │
│  │    tracks RemoteMintAuth delivery status via Axelar Scan API         │ │
│  │    tracks RemoteMintExecuted ack delivery                            │ │
│  │    triggers force_reconcile_mint_auth if ack never arrives           │ │
│  └──────────────────────────┬───────────────────────────────────────────┘ │
│                             │                                              │
│  ┌──────────────────────────▼───────────────────────────────────────────┐ │
│  │  Stellar Event Indexer                                               │ │
│  │    subscribes to Stellar contract events via Horizon or Soroban RPC  │ │
│  │    indexes: DepositLocal · DepositRemoteSettled · MintLiab           │ │
│  │             RedeemLocal · CctpMessageConsumed · GmpMessageConsumed   │ │
│  │             StrategyValueReported · InvariantChecked                 │ │
│  └──────────────────────────┬───────────────────────────────────────────┘ │
│                             │                                              │
│  ┌──────────────────────────▼───────────────────────────────────────────┐ │
│  │  Proof-of-Backing API                                                │ │
│  │    reads VaultAccounting.global_state() on-chain                     │ │
│  │    anchors snapshot to Stellar ledger hash                           │ │
│  │    serves /api/v1/proof endpoint to frontend                         │ │
│  └──────────────────────────────────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────────────────────────┘
```

### 9.3 Transaction State Machine

Every cross-chain operation produces a status record that the dApp polls and displays.

```
quote_created
      │
      ▼
source_approval_required ────(auto-approve)───►
      │                                         │
      ▼                                         ▼
source_tx_pending_signature ──────────► source_tx_submitted
                                               │
                                               ▼
                                       source_tx_confirmed
                                               │
                              ┌────────────────┴──────────────────┐
                              │ EVM/Solana deposit                 │ Stellar deposit
                              ▼                                    ▼
                    cctp_attestation_pending             stellar_accounting_confirmed
                              │                                    │
                              ▼                                    │
                    cctp_attestation_ready                         │
                              │                                    │
                              ▼                                    │
                    destination_receive_submitted                  │
                              │                                    │
                              ▼                                    │
                    stellar_accounting_confirmed ◄─────────────────┘
                              │
                     ┌────────┴─────────┐
                     │ local fUSD       │ remote fUSD
                     ▼                  ▼
                 completed         gmp_ack_pending
                                        │
                                        ▼
                                 remote_mint_pending
                                        │
                                        ▼
                                    completed
                              (or failed_recoverable
                               → user claim required)
```

---

## 10. Security Properties

| Property | Mechanism | Where enforced |
|----------|-----------|---------------|
| No relayer-injectable amount | Balance delta, not param | `MintRedeemController.receive_cctp_settlement()` |
| Mint auth replay protection | `consumed_hashes` set (Persistent, 5yr TTL) | `VaultAccounting` + EVM `usedMintAuths` map |
| Expired mint auth rejection | `block.timestamp < expiryTimestamp` | EVM `RemoteRouter.execute()` |
| Unknown hub rejection | `srcChain == hubChain && srcAddr == hubAddr` | EVM `RemoteRouter.execute()` |
| Unvalidated GMP rejection | `validateContractCall()` must return true FIRST | EVM `RemoteRouter.execute()` (CEI order) |
| Collateral release race guard | `panic if pending_mint_auth_6 > 0` | `VaultAccounting.record_spoke_collateral_released()` |
| No double-mint on fast credit | `pending_fast_credit -= amount` (no allowance increase) | `VaultAccounting.finalize_fast_credit()` |
| Stuck ack recovery | `force_reconcile_mint_auth` after timeout | `VaultAccounting` (governance callable after `mint_auth_ack_timeout_ledgers`) |
| Fee recipient separation | `manager_set_fees` only accepts rate fields | `MintRedeemController` |
| Fee version guard | caller passes `min_fee_version`; reverts if stale | `MintRedeemController` |
| Solvency invariant | `check_invariant_gs` panics on breach | Every `VaultAccounting` mutation |
| Route version binding | `route.version == caller_version` | `AllocationManager.execute_bridge_route()` |

---

## Repository Layout

```
frgmnt_stellar/
├── contracts/
│   ├── soroban/
│   │   ├── fusd-token/              SEP-41 fUSD token on Stellar
│   │   │   └── src/lib.rs
│   │   ├── vault-accounting/        Canonical hub state machine
│   │   │   └── src/lib.rs
│   │   ├── mint-redeem-controller/  User entry point
│   │   │   └── src/lib.rs
│   │   ├── allocation-manager/      Strategy orchestration (idle USDC <-> adapters)
│   │   │   └── src/lib.rs
│   │   ├── xycloans-adapter/        Active strategy adapter — xycLoans flash-loan pool
│   │   │   └── src/lib.rs
│   │   ├── defindex-adapter/        Active strategy adapter — deFindex vault
│   │   │   └── src/lib.rs
│   │   └── blend-adapter/           Retained, not active — see spec §8 status note
│   │       └── src/lib.rs
│   └── evm/
│       ├── src/
│       │   ├── RemoteFusd.sol       ERC-20 fUSD
│       │   ├── RemoteRouter.sol     EVM spoke router
│       │   ├── interfaces/          ICCTP.sol · IAxelarGateway.sol
│       │   └── mocks/               MockCCTP.sol · MockAxelarGateway.sol
│       └── test/
│           └── CrossChainFlow.t.sol 23 Foundry integration tests
├── docs/
│   ├── ARCHITECTURE.md              ← this document
│   ├── CROSS_CHAIN_FUSD_TECHNICAL_SPEC.md   full contract spec (3900 lines)
│   └── POC_GUIDE.md                 testnet deployment + demo txs
└── Cargo.toml                       Rust workspace
```
