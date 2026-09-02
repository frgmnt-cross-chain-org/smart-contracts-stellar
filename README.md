# fUSD Cross-Chain Stablecoin — Soft PoC

Demonstrates the hub-and-spoke architecture described in
[`docs/CROSS_CHAIN_FUSD_TECHNICAL_SPEC.md`](docs/CROSS_CHAIN_FUSD_TECHNICAL_SPEC.md).

Stellar is the canonical accounting hub. EVM chains (Base, Ethereum, Arbitrum, OP Mainnet)
are execution spokes. Value moves via **CCTP v2**; authorization signals move via **Axelar GMP**.

---

## Repository layout

```
contracts/
  soroban/
    fusd-token/           SEP-41 fUSD token (Soroban)
    vault-accounting/     Canonical accounting hub (Soroban)
    mint-redeem-controller/  User entry point — deposit, redeem, remote mint auth
    allocation-manager/   Governance-gated strategy orchestration (idle USDC <-> adapters)
    xycloans-adapter/     Active strategy adapter — xycLoans flash-loan liquidity pool
    defindex-adapter/     Active strategy adapter — deFindex vault
    blend-adapter/        Retained, not an active integration — see below
  evm/
    src/
      RemoteFusd.sol      ERC-20 fUSD token on EVM spokes
      RemoteRouter.sol    EVM spoke — depositAndBridge, execute (GMP), burnRemoteFusdAndRedeem
      interfaces/         ICCTP.sol, IAxelarGateway.sol
      mocks/              MockCCTP.sol, MockAxelarGateway.sol
    test/
      CrossChainFlow.t.sol  Foundry integration tests (3 flows + security checks)
docs/
  CROSS_CHAIN_FUSD_TECHNICAL_SPEC.md   Architecture and cross-chain flow specification
  ARCHITECTURE.md                       Full technical stack, contracts, and governance model
  POC_GUIDE.md                          PoC setup and deployment guide
```

---

## Prerequisites

| Tool | Version |
|------|---------|
| Rust + `cargo` | stable ≥ 1.78 |
| `soroban-cli` | ≥ 21.x |
| Foundry (`forge`) | latest |
| Node.js (optional, for scripts) | ≥ 18 |

---

## Running Soroban tests

```bash
# From repo root
cargo test --workspace
```

All seven Soroban crates include unit tests (**129 total**) covering:
- `fusd-token`: mint/burn, non-controller rejection, pause guard
- `vault-accounting`: solvency invariant, CCTP replay protection, collateral-release guard,
  fast-credit finalization (no double-mint), stuck-ack recovery, strategy allocation
  accounting (debt ceilings, yield/loss reporting never touching mint allowance, a
  per-epoch cap on reported gains, and aggregate-vs-per-strategy value consistency across
  multiple strategies)
- `mint-redeem-controller`: fee CRIT-1 (no fee_recipient in manager call), decimal dust,
  daily redeem limit rollover, gross-vs-net deposit fee accounting, real SEP-41 token
  transfers via a live Stellar Asset Contract test double, the Relayer gate on CCTP
  settlement submission, and pause/idle-bound enforcement on Allocator fund egress
- `allocation-manager`: role-gated allocate/deallocate/emergency-exit orchestration across
  VaultAccounting, MintRedeemController, and a strategy adapter, end-to-end with real
  token movement, including that `emergency_exit` relays the real admin caller (not this
  contract's own address) into the adapter's own Admin-gated check
- `xycloans-adapter`: deposit/withdraw against a mock xycLoans pool with real SAC token
  transfers, matured-fee harvesting semantics, balance-delta-based slippage protection,
  exposure caps, pause semantics
- `defindex-adapter`: deposit/withdraw against a mock deFindex vault with real SAC token
  transfers, share-price-based valuation under simulated yield, proportional-withdraw
  rounding direction (never favors the withdrawer), exposure caps, pause semantics
- `blend-adapter`: kept passing (not actively used — see below) — deposit/withdraw
  against a mock Blend pool, V1-vs-V2 valuation behavior, balance-delta-based slippage
  protection, exposure caps, pause semantics, and a required explicit
  `deprecation_acknowledged` flag at initialization

### Strategy allocation layer

`allocation-manager` implements the "(Future)" `AllocationManager` piece from
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md#51-soroban-contracts-hub): idle Stellar
USDC can be moved into a registered `StrategyAdapter`-shaped contract and back, with a
strategy's own reported value reflected in `VaultAccounting` — never inflating mint
allowance.

**Active adapters: `xycloans-adapter` and `defindex-adapter`.** Blend V2's backstop (the
Comet AMM BLND-USDC pool) was exploited on 2026-08-22 and cannot be repaired; Blend V2 is
being wound down, and the Stellar Community Fund has withdrawn Blend from its Integration
Track. `xycloans-adapter` integrates
[xycLoans](https://github.com/xycloo/xycloans), a flash-loan-only liquidity pool with no
price oracle and no undercollateralized-borrow surface at all — yield is flash-loan fee
income rather than term-loan interest, but the protocol structurally cannot accrue bad
debt. `defindex-adapter` integrates a single-asset
[deFindex](https://github.com/defindex-io/stellar-contracts) vault (audited by OtterSec,
March 2025); deFindex is itself a multi-strategy router, so governance must confirm
out-of-band which strategies a given vault routes to before registering it — this
protocol's own use of deFindex never routes through deFindex's own Blend strategy.

**`blend-adapter` is retained but not an active integration.** Its interface is generic
across Blend pool versions (`pool_version: 1 | 2`), so it is kept, tested, and unused
purely so a future, independently audited Blend V3 can be evaluated later without a
rewrite — it must not be registered against a live Blend V1/V2 pool.
`blend-adapter/src/blend_pool.rs` documents exactly which parts of the V1/V2 interface
were verified.

See [`docs/CROSS_CHAIN_FUSD_TECHNICAL_SPEC.md` §8](docs/CROSS_CHAIN_FUSD_TECHNICAL_SPEC.md#8-stellar-strategy-adapters)
for the full design rationale and per-adapter risk notes.

---

## Running EVM (Foundry) tests

```bash
cd contracts/evm

# Install dependencies (first time only)
forge install foundry-rs/forge-std
forge install OpenZeppelin/openzeppelin-contracts

# Run all tests
forge test -vvv
```

`CrossChainFlow.t.sol` covers:

| Test | Flow |
|------|------|
| `test_flowA_depositAndBridge` | USDC deposit → CCTP burn → GMP mint auth → fUSD minted |
| `test_flowB_redeemLocalFusd` | fUSD burn → CCTP USDC burn → hub credited |
| `test_flowC_mintAuthReplayReverts` | Replay of same `mintAuthId` is rejected |
| `test_flowC_expiredMintAuthReverts` | Expired auth is rejected |
| `test_flowC_unknownHubReverts` | Wrong Axelar source chain/address is rejected |
| `test_flowC_unapprovedGmpReverts` | Unvalidated GMP call is rejected |
| `test_fullRoundTrip` | Deposit → receive fUSD → redeem fUSD → USDC back |

---

## Key security properties demonstrated

### 1. CCTP settlement submission is Relayer-gated (CC-CRIT-1, partial)
`receive_cctp_settlement` in `mint-redeem-controller` still accepts `mock_net_received_6`
as a caller-supplied parameter in this PoC — real balance-delta computation
(`usdc_balance_after - usdc_balance_before` inside the same transaction, replacing the
Circle CCTP `receive_message` call this repo does not vendor) is not yet implemented, and
the function's own comments say so. What **is** enforced now: the function is gated to a
single admin-appointed `Relayer` address (`set_relayer`), not fully permissionless —
closing the "anyone can call this and mint themselves fUSD" path that existed once
`local_recipient` let a caller direct the mint to an address of their choosing. The mock
amount itself remains untrustworthy until real CCTP verification lands; do not treat this
function as production-ready before that work is done.

### 2. Mint auth replay protection
`vault-accounting` stores every `mint_auth_id` in Soroban Persistent storage with a
5-year TTL. `RemoteRouter` maps `usedMintAuths[mintAuthId] => true` before minting,
following CEI (Check-Effects-Interactions) order.

### 3. Collateral-release race guard (CC-HIGH-3)
Hub rejects `SpokeCollateralReleased` when `chain.pending_mint_auth_6 > 0`. Prevents
a spoke from releasing collateral while a mint authorization is still in flight.

### 4. Fast-credit finalization without double-mint (CC-MED-1)
When a final CCTP attestation arrives for a previously fast-credited transfer:
`pending_fast_credit_6` is decremented; `mint_allowance_6` is NOT incremented
(fUSD was already issued at fast-credit time).

### 5. Governance ack recovery (CC-MED-2)
`force_reconcile_mint_auth` lets governance unstick a `pending_mint_auth_6` balance
when Axelar permanently fails to deliver the `RemoteMintExecuted` ack. Requires
`mint_auth_ack_timeout_ledgers` to have elapsed since issuance.

### 6. Manager fee authority (CRIT-1)
`manager_set_fees` accepts only `mint_fee_bps / redeem_fee_bps` rate fields.
`fee_recipient` is a separate admin-only call. Manager cannot redirect protocol fees.

### 7. Solvency invariant
Every state mutation in `vault-accounting` calls `check_invariant_gs`. If
`total_liabilities_6 > settled_idle_usdc_6 + settled_spoke_escrow_usdc_6 + total_strategy_value_6
 - pending_outbound_usdc_6 - required_reserve_6`, the transaction panics.

### 8. Per-epoch yield rate limit (strategy value reporting)
`report_strategy_value` bounds every *gain* against `max_yield_per_epoch_6` (accumulated
in `yield_credited_this_epoch_6`, resetting when `epoch_length_ledgers` elapses) — a
circuit breaker against a compromised or buggy Allocator key instantly fabricating
backing in one call. Losses are never rate-limited: bad news must always be reflected
immediately and in full for the solvency invariant to mean anything.

### 9. `emergency_exit` caller relay (AllocationManager)
`AllocationManager.emergency_exit` relays the real admin `caller` into the adapter's own
`emergency_exit`, not this contract's own address. Every adapter gates `emergency_exit` on
its own separately-configured `Admin` (governance), distinct from `Allocator` (=
AllocationManager's address) — relaying the wrong identity would make the entire
governance emergency-exit path silently unreachable in production while still passing a
test whose mock happened to conflate the two roles. `min_out_6` is now a real
caller-supplied parameter here too, rather than hardcoded to `0`.

---

## Architecture

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for:
- full technical stack diagram
- Stellar integration deep-dive (Soroban, SEP-41, CCTP v2, Axelar GMP, decimal handling)
- smart contract architecture with interaction maps
- all three cross-chain flows with sequence diagrams
- dApp and indexer architecture
- governance model and role limits

