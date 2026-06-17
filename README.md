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

All three Soroban crates include unit tests covering:
- `fusd-token`: mint/burn, non-controller rejection, pause guard
- `vault-accounting`: solvency invariant, CCTP replay protection, collateral-release guard,
  fast-credit finalization (no double-mint), stuck-ack recovery
- `mint-redeem-controller`: fee CRIT-1 (no fee_recipient in manager call), decimal dust,
  daily redeem limit rollover

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

### 1. Balance-delta CCTP settlement (CC-CRIT-1)
`receive_cctp_settlement` in `mint-redeem-controller` does **not** accept `amount_6` as a
relayer-supplied parameter. The credited amount is computed as `usdc_balance_after -
usdc_balance_before` inside the same transaction. (PoC uses `mock_net_received_6` for
test expressibility, with a comment noting this must be the delta in production.)

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

---

## Architecture

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for:
- full technical stack diagram
- Stellar integration deep-dive (Soroban, SEP-41, CCTP v2, Axelar GMP, decimal handling)
- smart contract architecture with interaction maps
- all three cross-chain flows with sequence diagrams
- dApp and indexer architecture
- governance model and role limits

