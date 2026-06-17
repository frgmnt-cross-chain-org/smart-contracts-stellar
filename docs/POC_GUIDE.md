# fUSD Cross-Chain PoC — Developer Guide

> **Scope**: This document covers only the soft PoC under `contracts/`.
> For the full production architecture, security model, and protocol invariants see
> [`CROSS_CHAIN_FUSD_TECHNICAL_SPEC.md`](CROSS_CHAIN_FUSD_TECHNICAL_SPEC.md).

---

## What the PoC covers

The PoC is a runnable demonstration of the three core cross-chain flows and the
critical security invariants that the production system must enforce. It is **not**
a production-ready deployment — mocks replace Circle CCTP and Axelar, and the
Soroban contracts omit governance, strategy allocation, and the full fee engine.

| Layer | What is implemented |
|-------|---------------------|
| Soroban (Stellar hub) | SEP-41 fUSD token, canonical vault accounting, mint/redeem controller |
| EVM spoke | ERC-20 fUSD, `RemoteRouter` (deposit/GMP receive/redeem) |
| Cross-chain transport | Mock CCTP v2, Mock Axelar Gateway |
| Tests | 30 Soroban unit tests, 23 Foundry integration tests |

---

## Contract map

```
contracts/
├── soroban/
│   ├── fusd-token/               SEP-41 fUSD on Stellar
│   │   └── src/lib.rs
│   ├── vault-accounting/         Hub state machine — the accounting authority
│   │   └── src/lib.rs
│   └── mint-redeem-controller/   User-facing entry point
│       └── src/lib.rs
└── evm/
    ├── foundry.toml
    ├── src/
    │   ├── RemoteFusd.sol         ERC-20 fUSD on EVM spokes
    │   ├── RemoteRouter.sol       EVM spoke router
    │   ├── interfaces/
    │   │   ├── ICCTP.sol          Circle CCTP v2 minimal interface
    │   │   └── IAxelarGateway.sol Axelar GMP minimal interface
    │   └── mocks/
    │       ├── MockCCTP.sol       Simulates Circle burn + USDC delivery
    │       └── MockAxelarGateway.sol  Simulates Axelar consensus + GMP relay
    └── test/
        └── CrossChainFlow.t.sol   Foundry integration tests
```

---

## Prerequisites

| Tool | Minimum version | Install |
|------|----------------|---------|
| Rust + cargo | stable ≥ 1.78 | https://rustup.rs |
| soroban-cli | 21.x | `cargo install --locked soroban-cli` |
| Foundry (forge) | latest | https://getfoundry.sh |

---

## Running Soroban tests

```bash
# From the repo root
cargo test --workspace
```

Expected output — all 30 tests passing across fusd-token and vault-accounting:

```
test result: ok. 12 passed; 0 failed  ← fusd-token
test result: ok. 18 passed; 0 failed  ← vault-accounting
```

---

## Running EVM tests

```bash
cd contracts/evm

# Install Foundry dependencies (first time only)
forge install foundry-rs/forge-std --no-git
forge install OpenZeppelin/openzeppelin-contracts --no-git

# Run all tests with full trace
forge test -vvv
```

Expected output:

```
Suite result: ok. 23 passed; 0 failed
```

### Test coverage

| Test | Flow demonstrated |
|------|------------------|
| `test_flowA_depositAndBridge` | Alice deposits USDC → CCTP burn → hub issues GMP mint auth → router mints fUSD |
| `test_flowB_redeemLocalFusd` | Alice burns fUSD → router burns USDC via CCTP → hub credits USDC |
| `test_flowC_mintAuthReplayReverts` | Same `mintAuthId` on a second call reverts with `MintAuthAlreadyUsed` |
| `test_flowC_expiredMintAuthReverts` | Past-expiry auth reverts with `MintAuthExpired` |
| `test_flowC_unknownHubReverts` | Wrong Axelar source chain/address reverts with `UnknownHub` |
| `test_flowC_unapprovedGmpReverts` | GMP call not validated by gateway reverts with `OnlyAxelarGateway` |
| `test_fullRoundTrip` | Deposit → receive fUSD → redeem fUSD → USDC burn confirmed end-to-end |
| `test_depositZeroReverts` | Zero-amount deposit reverts |
| `test_redeemZeroReverts` | Zero-amount redeem reverts |
| `test_setAdmin_*` | Admin transfer with zero-address guard |
| `test_rescueUsdc_*` | Emergency USDC rescue (admin only) |
| `test_fusd_*` | RemoteFusd unit checks (non-router can't mint/burn, decimals, immutability) |
| `test_ackPayloadContainsMintAuthId` | MintExecuted ack encodes the correct mintAuthId |
| `test_multipleUsersIndependent` | Two users can deposit/redeem without interference |

---

## ✅ Stellar Testnet Deployment

All three Soroban contracts are live on Stellar testnet.

### Contract Addresses

| Contract | Address |
|----------|---------|
| **fUSD Token** (SEP-41) | `CABKKCFL6OSJP3GTNFTQB67LB4PH5DLV6OZE7OT7ZZBSVNZDNTXPRW6C` |
| **Vault Accounting** | `CCIJIPNDCOZDARVZ5SRVTRAWORS6WOU3TBT2BZW2VVPYK7JZQ2BLU7UW` |
| **Mint-Redeem Controller** | `CB2YGF2B63E4VAKNVAYPTG2PDDHZVLTEWZLGDM4XQMS4G5LHPIHBD2CT` |

Supporting addresses:

| Role | Address |
|------|---------|
| Deployer / Admin | `GB2HC2NLXR7LHKXGS2IZL4F5LZVQVKRBKCWONQQW4WIYUXDILHORWQPZ` |
| Demo User | `GCZZW2O23FN6IULHJF7R3JLZVQ2MCG2TYSQFYPG7WQGWUZFTT7X75RTI` |
| Testnet USDC SAC | `CBIELTK6YBZJU5UP2WWQEUCYKLPU6AUNZ2BQ4WWFEIE3USCIHMXQDAMA` |

### Initialization Transactions

| Step | Transaction Hash |
|------|-----------------|
| Initialize Vault Accounting | [`1ec76ba0...`](https://stellar.expert/explorer/testnet/tx/1ec76ba00a65812dfb1078002801a3b235fe81bdfddba4d82ae5ad0d7a98df04) |
| Initialize fUSD Token | [`f363e2bf...`](https://stellar.expert/explorer/testnet/tx/f363e2bf27bc5dabb9797176744daadb0a428265da92290c2855d40ac396d106) |
| Initialize Controller | [`4b34abe9...`](https://stellar.expert/explorer/testnet/tx/4b34abe933c9f87530aa4e79c941a55d176bfbd665de6e9a766b2e6ba9ae45fd) |

Configuration: `mint_fee_bps=0`, `redeem_fee_bps=30` (0.3%), `required_reserve_bps=1000` (10%), `hub_cctp_domain=27`.

---

### Demo Transactions

#### Tx 1 — Deposit 1 000 USDC → Mint 1 000 fUSD

Depositor calls `deposit_usdc` on the controller with 1 000 USDC (10 000 000 000 in 7-decimal Stellar units).

The controller:
1. Records the deposit in vault (`settled_idle_usdc_6 += 1 000 000 000`)
2. Creates a fUSD liability (`total_liabilities_6 += 1 000 000 000`)
3. Mints 1 000 fUSD to the depositor

```
Transaction: c32e52062597589bcce82690807e08abfed9eeb04114069a1be1e6bdf880b79c
Explorer:    https://stellar.expert/explorer/testnet/tx/c32e52062597589bcce82690807e08abfed9eeb04114069a1be1e6bdf880b79c

Events emitted:
  UsdcXfr  deployer → controller  10 000 000 000 (7 dec, mocked)
  LocDep   1 000 000 000
  MintLiab 1 000 000 000
  Mint     → deployer  1 000 000 000

Post-state:
  deployer fUSD balance:   1 000 000 000  (1 000.000000 fUSD)
  vault settled_idle:      1 000 000 000
  vault total_liabilities: 1 000 000 000
```

#### Tx 2 — Transfer 200 fUSD to Demo User

Deployer transfers 200 fUSD to a second wallet to demonstrate the SEP-41 transfer interface.

```
Transaction: 9e7419c33c6a08eeab40292ce6956bfbd09dfd4d24d72a4ce2ca3af3aa0766af
Explorer:    https://stellar.expert/explorer/testnet/tx/9e7419c33c6a08eeab40292ce6956bfbd09dfd4d24d72a4ce2ca3af3aa0766af

Post-state:
  deployer fUSD balance:   800 000 000  (800.000000 fUSD)
  demo_user fUSD balance:  200 000 000  (200.000000 fUSD)
  total_supply:          1 000 000 000  (unchanged — transfer ≠ mint)
```

#### Tx 3 — Redeem 500 fUSD → Receive USDC (0.3% fee)

Deployer burns 500 fUSD and receives 499.5 USDC (net of 0.3% = 1.5 USDC fee which stays in vault as protocol income).

The controller:
1. Burns 500 fUSD from deployer
2. Reduces liability by full 500M burned
3. Moves only 498.5M (net) from idle to pending_outbound
4. Sends USDC to user (mocked in PoC)
5. Clears pending_outbound via `mark_outbound_sent`

```
Transaction: 0d361e67b5e40d52a485c809d98dbd9a62cece09216693892e131f23c1651e26
Explorer:    https://stellar.expert/explorer/testnet/tx/0d361e67b5e40d52a485c809d98dbd9a62cece09216693892e131f23c1651e26

Post-state:
  deployer fUSD balance:   300 000 000  (300.000000 fUSD)
  vault settled_idle:      501 500 000  (1.5 USDC fee retained as protocol income)
  vault total_liabilities: 500 000 000
```

#### Tx 4 — Pause Protocol (Admin)

Demonstrates the emergency pause mechanism — all deposits and redemptions are blocked.

```
Transaction: 0fbcc18d2775e20ad6946e3502c11ed7aaa96653b5f9499065783be36da17068
Explorer:    https://stellar.expert/explorer/testnet/tx/0fbcc18d2775e20ad6946e3502c11ed7aaa96653b5f9499065783be36da17068
```

#### Tx 5 — Deposit BLOCKED (while paused)

Attempt to deposit while paused fails with `UnreachableCodeReached` (Soroban's panic encoding):

```
Error: HostError: Error(WasmVm, InvalidAction) — "token paused"
(transaction intentionally rejected — not submitted on-chain)
```

#### Tx 6 — Unpause Protocol (Admin)

```
Transaction: 95964d6b6e6376f83b36031ed34b7ff839f66163a3c32865e91ff871d2841ff2
Explorer:    https://stellar.expert/explorer/testnet/tx/95964d6b6e6376f83b36031ed34b7ff839f66163a3c32865e91ff871d2841ff2
```

#### Tx 7 — Deposit 500 USDC After Unpause

Confirms protocol resumed normally after unpause.

```
Transaction: 9dd46e3af5185003d2c3d79b02eaeed4cf95f88214596040ed358b2958f749df
Explorer:    https://stellar.expert/explorer/testnet/tx/9dd46e3af5185003d2c3d79b02eaeed4cf95f88214596040ed358b2958f749df

Final state:
  deployer fUSD balance:   800 000 000  (300 + 500 = 800.000000 fUSD)
  demo_user fUSD balance:  200 000 000  (200.000000 fUSD)
  total_supply:          1 000 000 000
  vault settled_idle:    1 001 500 000  (501.5 pre-unpause + 500 new deposit)
  vault total_liabilities: 1 000 000 000
```

---

## EVM Deployment (Base Sepolia)

The EVM contracts compile and all 23 tests pass locally. On-chain deployment
requires an EOA with Base Sepolia ETH.

### Deploy script

```bash
cd contracts/evm

# Configure wallet
export PRIVATE_KEY=<your-key>
export RPC_URL=https://sepolia.base.org

# Base Sepolia contract addresses
USDC=0x036CbD53842c5426634e7929541eC2318f3dCF7e
CCTP_MESSENGER=0x9f3B8679c73C2Fef8b59B4f3444d4e156fb70AA5
CCTP_TRANSMITTER=0x7865fAfC2db2093669d92c0197ea5b5f45a5eb5
AXELAR_GATEWAY=0xe432150cce91c13a887f7D836923d5597adD8E31
AXELAR_GAS_SERVICE=0xbE406F0189A0B4cf3A05C286473D23791Dd44Cc

# 1. Deploy RemoteFusd with a predicted router address
#    (use forge script or two-step deploy)

# 2. Deploy RemoteRouter
forge create src/RemoteRouter.sol:RemoteRouter \
  --private-key $PRIVATE_KEY \
  --rpc-url $RPC_URL \
  --constructor-args \
    $USDC \
    <fusd_address> \
    $CCTP_MESSENGER \
    $CCTP_TRANSMITTER \
    $AXELAR_GATEWAY \
    $AXELAR_GAS_SERVICE \
    27 \
    "stellar" \
    "GSTELLARHUBADDRESS000000000000000000000000000000000000"
```

> **Note**: The `hubAxelarAddress` should be the actual Axelar-registered address for the
> Stellar hub in production. The mock value above works with the local test suite.

---

## Flow walkthroughs

### Flow 1 — Deposit and bridge (EVM → Stellar → fUSD)

```
Alice (EVM)                   RemoteRouter              Stellar Hub
    │                              │                         │
    │── depositAndBridge(100 USDC)►│                         │
    │                              │── CCTP burn ───────────►│
    │                              │   (domain=27, hub vault)│
    │                              │                         │ record_inbound_settlement
    │                              │                         │ (balance-delta, not param)
    │                              │                         │ authorize_remote_mint
    │                              │◄── Axelar GMP ──────────│
    │                              │    RemoteMintAuth        │
    │                              │    {mintAuthId, alice,   │
    │                              │     100e6, expiry}       │
    │                              │ validateContractCall ✓   │
    │                              │ usedMintAuths[id]=true   │
    │                              │ fusd.mint(alice, 100e6)  │
    │◄── 100 fUSD ─────────────────│                         │
    │                              │── Axelar GMP ──────────►│
    │                              │   RemoteMintExecuted     │
    │                              │                         │ confirm_remote_mint_executed
```

**Key invariant**: `record_inbound_settlement` computes `net_received_6` as
`usdc_balance_after - usdc_balance_before` — the relayer cannot inflate the amount.

### Flow 2 — Redeem (EVM fUSD → Stellar USDC)

```
Alice (EVM)                   RemoteRouter              Stellar Hub
    │                              │                         │
    │── burnRemoteFusdAndRedeem ──►│                         │
    │   (150 fUSD, stellarAddr)    │                         │
    │                              │ fusd.burn(alice, 150)   │
    │                              │── CCTP burn ───────────►│
    │                              │   (USDC from reserve,   │
    │                              │    recipient=stellarAddr)│
    │                              │                         │ accept_spoke_burn
    │                              │                         │ burn_liability
    │                              │                         │ → USDC released to user
```

**Note**: The router must hold a USDC reserve matching circulating fUSD on the spoke.
In production, the hub funds this via rebalance. The PoC pre-funds the router in `setUp`.

### Flow 3 — Remote mint (Stellar-native deposit → EVM fUSD)

When a user deposits USDC directly on Stellar and wants fUSD on an EVM chain:

```
User (Stellar)           Stellar Hub               RemoteRouter (EVM)
    │                        │                           │
    │── deposit_usdc ────────►│                           │
    │                        │ record_local_deposit       │
    │                        │ authorize_remote_mint      │
    │                        │── Axelar GMP RemoteMintAuth►│
    │                        │                           │ execute()
    │                        │                           │ validates + mints fUSD
    │                        │◄── Axelar GMP MintExecuted─│
    │                        │ confirm_remote_mint_executed│
```

---

## Security properties demonstrated in the PoC

### CC-CRIT-1 — Balance-delta CCTP amount
`receive_cctp_settlement` / `record_inbound_settlement` does not accept `amount_6` from
the relayer. The credited amount is `usdc_balance_after - usdc_balance_before` computed
inside the same transaction as the `receiveMessage` call.

### CC-HIGH-1 — Route version binding
`execute_bridge_route`, `rebalance_to_strategy`, `rebalance_from_strategy` all require
a caller-supplied `route_version: u32`. The hub rejects calls where
`AllocationRoute.version != route_version`, preventing stale-route replays.

### CC-HIGH-2 — Fee version guard
`deposit_usdc` and `redeem_local` take `min_fee_version: u32`. If `active_fee_config.version
!= min_fee_version` the call reverts, so governance can safely raise fees knowing
in-flight transactions won't silently use stale rates.

### CC-HIGH-3 — Collateral release guard
`record_spoke_collateral_released` panics if `chain.pending_mint_auth_6 > 0`. A spoke
cannot release collateral while a mint authorization is still in flight (would break
the spoke-escrow accounting on the hub).

### CC-MED-1 — Fast-credit finalization without double-mint
When a final CCTP attestation arrives for a previously fast-credited transfer:
`pending_fast_credit_6` is decremented and `mint_allowance_6` is **not** incremented
(fUSD was already issued at fast-credit time; incrementing would allow a double-mint).

### CC-MED-2 — Stuck-ack governance recovery
`force_reconcile_mint_auth` lets governance mark a `MintAuth` as `Executed` without
the Axelar ack, provided `current_ledger > issued_ledger + mint_auth_ack_timeout_ledgers`.
Prevents permanent `pending_mint_auth_6` drift when Axelar permanently fails.

### CRIT-1 — Manager fee authority separation
`manager_set_fees` accepts only `mint_fee_bps` and `redeem_fee_bps`. The `fee_recipient`
field is set exclusively by the admin via a separate `set_fee_recipient` call. A
compromised manager key cannot redirect protocol fees.

### Solvency invariant
After every state mutation, `vault-accounting` calls `check_invariant_gs`:

```rust
// Basic solvency: total assets must cover all fUSD liabilities.
// Note: settled_idle has already been reduced by pending_outbound in
// burn_liability_for_redemption — no double-subtraction here.
let total_assets = settled_idle_usdc_6
    + settled_spoke_escrow_usdc_6
    + total_strategy_value_6;
let basic_solvency = total_assets >= total_liabilities_6;

// Liquidity floor: enough idle USDC to serve immediate redemptions.
let required_idle = total_liabilities_6 * required_reserve_bps / 10_000;
let liquidity_ok  = settled_idle_usdc_6 >= required_idle;

basic_solvency && liquidity_ok
```

If the invariant fails, the transaction panics — no state is committed.

---

## What the PoC intentionally omits

| Feature | Where to find the full spec |
|---------|----------------------------|
| GovernanceController (timelock, delay classes) | §7, CROSS_CHAIN_FUSD_TECHNICAL_SPEC.md |
| Strategy allocation (bridge routes, yield) | §8–9 |
| Daily redeem limits and rollover | §6.4 |
| Yield epoch and performance fees | §6.5 |
| Fast-credit insurance reserve | §10.4 |
| Solana spoke | §11 |
| Emergency pause / circuit breaker | §7.3 |
| Full CCTP v2 message parsing | Circle CCTP v2 docs |
| Axelar proof verification | Axelar GMP docs |

---

## Decimal reference

| Asset | Decimals | Notes |
|-------|----------|-------|
| Stellar USDC | 7 | Soroban native; floor to 6 at controller boundary |
| EVM USDC | 6 | Circle standard |
| Solana USDC | 6 | Circle standard |
| fUSD (all chains) | 6 | Hub accounting unit |

Conversion happens **only** at the `mint-redeem-controller` boundary when accepting
Stellar-native USDC deposits. All internal hub accounting and all cross-chain messages
use 6-decimal values.

---

## Build reproducibility

```bash
# From repo root — must build in this order (controller imports vault ABI)
RUSTFLAGS="-C target-feature=-reference-types" \
  cargo build --target wasm32-unknown-unknown --release -p fusd-token
RUSTFLAGS="-C target-feature=-reference-types" \
  cargo build --target wasm32-unknown-unknown --release -p vault-accounting
RUSTFLAGS="-C target-feature=-reference-types" \
  cargo build --target wasm32-unknown-unknown --release -p mint-redeem-controller

# Optimize for deployment
soroban contract optimize --wasm target/wasm32-unknown-unknown/release/fusd_token.wasm
soroban contract optimize --wasm target/wasm32-unknown-unknown/release/vault_accounting.wasm
soroban contract optimize --wasm target/wasm32-unknown-unknown/release/mint_redeem_controller.wasm
```

> The `.cargo/config.toml` at the repo root sets `target-feature=-reference-types`
> automatically, so you can also just run `cargo build ...` without the env var prefix.
