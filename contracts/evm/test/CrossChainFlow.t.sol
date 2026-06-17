// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "@openzeppelin/contracts/token/ERC20/ERC20.sol";

import "../src/RemoteFusd.sol";
import "../src/RemoteRouter.sol";
import "../src/mocks/MockCCTP.sol";
import "../src/mocks/MockAxelarGateway.sol";

/// @notice Minimal ERC-20 USDC mock for local testing.
contract MockUsdc is ERC20 {
    constructor() ERC20("USD Coin", "USDC") {}

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }

    function decimals() public pure override returns (uint8) {
        return 6;
    }
}

/// @title CrossChainFlow
/// @notice Integration tests for the EVM spoke of the fUSD cross-chain system.
///
/// Three end-to-end scenarios demonstrated:
///
///   Flow A — Deposit & Bridge:
///     User deposits USDC on EVM → CCTP burns it → Stellar hub settles →
///     Hub issues RemoteMintAuth via Axelar → Router mints fUSD to user.
///
///   Flow B — Redeem:
///     User burns fUSD on EVM → Router burns USDC reserve via CCTP →
///     Stellar hub receives USDC, marks liability settled.
///
///   Flow C — Security checks:
///     Replay of mint auth reverts. Expired auth reverts. Unknown hub reverts.
contract CrossChainFlow is Test {
    // ── Contracts ────────────────────────────────────────────────────────────
    MockUsdc             usdc;
    RemoteRouter         router;
    RemoteFusd           fusd;
    MockCCTP             mockCctp;
    MockMessageTransmitter mockTransmitter;
    MockAxelarGateway    axelarGw;
    MockAxelarGasService axelarGs;

    // ── Constants ─────────────────────────────────────────────────────────
    uint32 constant HUB_CCTP_DOMAIN = 27;
    string constant HUB_AXELAR_CHAIN   = "stellar";
    string constant HUB_AXELAR_ADDRESS = "GSTELLARHUBADDRESS000000000000000000000000000000000000";

    // Mirror the router's payload selectors so tests compile without contract-type access.
    bytes4 constant MSG_REMOTE_MINT_AUTH     = bytes4(keccak256("RemoteMintAuthorize"));
    bytes4 constant MSG_REMOTE_MINT_EXECUTED = bytes4(keccak256("RemoteMintExecuted"));

    address alice = makeAddr("alice");
    address bob   = makeAddr("bob");

    // Simulate Stellar hub vault address as 32-byte value.
    bytes32 stellarHubVault = bytes32(uint256(0xCAFE));

    // ── Setup ────────────────────────────────────────────────────────────────
    function setUp() public {
        usdc            = new MockUsdc();
        mockCctp        = new MockCCTP();
        mockTransmitter = new MockMessageTransmitter(address(usdc));
        axelarGw        = new MockAxelarGateway();
        axelarGs        = new MockAxelarGasService();

        // Deploy fUSD with a placeholder router address; update after router deploy.
        // Use a two-step approach: deploy router first with temp address, then set fusd.
        // For simplicity: deploy fusd with this test contract as router, then override.
        // Actual approach: deploy router, construct fusd with router address.

        // 1. Predict router address — use CREATE with nonce offset.
        //    Simpler: deploy fusd with address(this), then deploy router and use it directly.
        //    Since RemoteFusd.router is immutable, we must deploy in the right order:
        //    Deploy router bytecode with a known address for fusd. Use vm.computeCreateAddress.

        address predictedRouter = vm.computeCreateAddress(address(this), vm.getNonce(address(this)) + 1);
        fusd = new RemoteFusd(predictedRouter);

        router = new RemoteRouter(
            address(usdc),
            address(fusd),
            address(mockCctp),
            address(mockTransmitter),
            address(axelarGw),
            address(axelarGs),
            HUB_CCTP_DOMAIN,
            HUB_AXELAR_CHAIN,
            HUB_AXELAR_ADDRESS
        );

        assertEq(address(router), predictedRouter, "router address mismatch");

        // Seed alice with USDC.
        usdc.mint(alice, 1_000e6);
        // Pre-fund the transmitter with USDC so it can "deliver" USDC on EVM receive.
        usdc.mint(address(this), 500e6);
        usdc.approve(address(mockTransmitter), 500e6);
        mockTransmitter.fund(500e6);
        // Pre-fund router with USDC reserve for redeem flow (simulating hub rebalance).
        usdc.mint(address(router), 500e6);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Flow A: Deposit → CCTP burn → (Stellar settles) → Axelar mint auth → fUSD minted
    // ─────────────────────────────────────────────────────────────────────────

    function test_flowA_depositAndBridge() public {
        uint256 depositAmount = 100e6; // 100 USDC

        vm.startPrank(alice);
        usdc.approve(address(router), depositAmount);
        uint64 nonce = router.depositAndBridge(depositAmount, stellarHubVault);
        vm.stopPrank();

        // Verify USDC was pulled from alice and burned via CCTP.
        assertEq(usdc.balanceOf(alice), 900e6, "alice USDC should be 900 after deposit");
        MockCCTP.BurnRecord memory burn = mockCctp.getBurn(nonce);
        assertEq(burn.amount, depositAmount, "CCTP burn amount mismatch");
        assertEq(burn.destinationDomain, HUB_CCTP_DOMAIN, "Wrong destination domain");
        assertEq(burn.mintRecipient, stellarHubVault, "Wrong mint recipient");

        // Simulate: Stellar hub settles the CCTP message, issues RemoteMintAuth via Axelar.
        bytes32 mintAuthId = keccak256(abi.encodePacked("MINT_AUTH_1", nonce));
        uint256 expiry = block.timestamp + 3600;

        bytes memory gmpPayload = abi.encode(
            MSG_REMOTE_MINT_AUTH,
            mintAuthId,
            alice,          // recipient on EVM
            depositAmount,  // fUSD amount (same as USDC, 6 dec)
            expiry
        );

        bytes32 commandId = keccak256("cmd-1");
        // Axelar approves the call.
        axelarGw.approveContractCall(
            commandId,
            HUB_AXELAR_CHAIN,
            HUB_AXELAR_ADDRESS,
            keccak256(gmpPayload)
        );

        // Axelar relayer calls execute on the router.
        router.execute(commandId, HUB_AXELAR_CHAIN, HUB_AXELAR_ADDRESS, gmpPayload);

        // fUSD should be minted to alice.
        assertEq(fusd.balanceOf(alice), depositAmount, "alice fUSD balance mismatch");
        // Mint auth marked used.
        assertTrue(router.usedMintAuths(mintAuthId), "mintAuthId should be consumed");
        // Axelar ack sent back to hub.
        assertEq(axelarGw.sentMessageCount(), 1, "Should have sent 1 Axelar ack");
        MockAxelarGateway.SentMessage memory ack = axelarGw.lastSentMessage();
        assertEq(keccak256(bytes(ack.destinationChain)), keccak256(bytes(HUB_AXELAR_CHAIN)));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Flow B: Redeem — user burns fUSD → CCTP burns USDC → hub credits user
    // ─────────────────────────────────────────────────────────────────────────

    function test_flowB_redeemLocalFusd() public {
        // First give alice some fUSD (simulate she already received it from Flow A).
        _mintFusdToAlice(200e6);

        bytes32 aliceStellarAddr = bytes32(uint256(uint160(alice)));
        uint256 redeemAmount = 150e6;

        vm.startPrank(alice);
        // No approve needed — RemoteFusd.burn is onlyRouter and bypasses allowance.
        uint64 cctpNonce = router.burnRemoteFusdAndRedeem(redeemAmount, aliceStellarAddr);
        vm.stopPrank();

        // fUSD burned from alice.
        assertEq(fusd.balanceOf(alice), 50e6, "alice fUSD should be 50 after redeem");

        // CCTP burn recorded.
        MockCCTP.BurnRecord memory burn = mockCctp.getBurn(cctpNonce);
        assertEq(burn.amount, redeemAmount, "CCTP burn amount for redeem mismatch");
        assertEq(burn.destinationDomain, HUB_CCTP_DOMAIN, "Wrong dest domain for redeem");
        assertEq(burn.mintRecipient, aliceStellarAddr, "Wrong USDC recipient for redeem");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Flow C: Security — replay, expiry, unknown hub
    // ─────────────────────────────────────────────────────────────────────────

    function test_flowC_mintAuthReplayReverts() public {
        bytes32 mintAuthId = keccak256("REPLAY_TEST");
        uint256 expiry = block.timestamp + 3600;

        bytes memory gmpPayload = abi.encode(
            MSG_REMOTE_MINT_AUTH,
            mintAuthId,
            alice,
            50e6,
            expiry
        );

        bytes32 commandId1 = keccak256("cmd-replay-1");
        axelarGw.approveContractCall(commandId1, HUB_AXELAR_CHAIN, HUB_AXELAR_ADDRESS, keccak256(gmpPayload));
        router.execute(commandId1, HUB_AXELAR_CHAIN, HUB_AXELAR_ADDRESS, gmpPayload);

        // Attempt replay with a new commandId but same mintAuthId.
        bytes32 commandId2 = keccak256("cmd-replay-2");
        axelarGw.approveContractCall(commandId2, HUB_AXELAR_CHAIN, HUB_AXELAR_ADDRESS, keccak256(gmpPayload));

        vm.expectRevert(abi.encodeWithSelector(RemoteRouter.MintAuthAlreadyUsed.selector, mintAuthId));
        router.execute(commandId2, HUB_AXELAR_CHAIN, HUB_AXELAR_ADDRESS, gmpPayload);
    }

    function test_flowC_expiredMintAuthReverts() public {
        bytes32 mintAuthId = keccak256("EXPIRED_AUTH");
        uint256 expiry = block.timestamp - 1; // already expired

        bytes memory gmpPayload = abi.encode(
            MSG_REMOTE_MINT_AUTH,
            mintAuthId,
            alice,
            50e6,
            expiry
        );

        bytes32 commandId = keccak256("cmd-expired");
        axelarGw.approveContractCall(commandId, HUB_AXELAR_CHAIN, HUB_AXELAR_ADDRESS, keccak256(gmpPayload));

        vm.expectRevert(abi.encodeWithSelector(RemoteRouter.MintAuthExpired.selector, mintAuthId, expiry));
        router.execute(commandId, HUB_AXELAR_CHAIN, HUB_AXELAR_ADDRESS, gmpPayload);
    }

    function test_flowC_unknownHubReverts() public {
        bytes32 mintAuthId = keccak256("UNKNOWN_HUB");
        uint256 expiry = block.timestamp + 3600;
        string memory fakeChain = "ethereum";
        string memory fakeAddr  = "0xDEADBEEF";

        bytes memory gmpPayload = abi.encode(
            MSG_REMOTE_MINT_AUTH,
            mintAuthId,
            alice,
            50e6,
            expiry
        );

        bytes32 commandId = keccak256("cmd-unknown-hub");
        axelarGw.approveContractCall(commandId, fakeChain, fakeAddr, keccak256(gmpPayload));

        vm.expectRevert(
            abi.encodeWithSelector(RemoteRouter.UnknownHub.selector, fakeChain, fakeAddr)
        );
        router.execute(commandId, fakeChain, fakeAddr, gmpPayload);
    }

    function test_flowC_unapprovedGmpReverts() public {
        bytes32 mintAuthId = keccak256("NO_APPROVAL");
        bytes memory gmpPayload = abi.encode(
            MSG_REMOTE_MINT_AUTH,
            mintAuthId,
            alice,
            50e6,
            block.timestamp + 3600
        );

        // Do NOT call axelarGw.approveContractCall — simulate a forged message.
        bytes32 commandId = keccak256("cmd-no-approval");
        vm.expectRevert(RemoteRouter.OnlyAxelarGateway.selector);
        router.execute(commandId, HUB_AXELAR_CHAIN, HUB_AXELAR_ADDRESS, gmpPayload);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Full round-trip: deposit → bridge → mint → redeem
    // ─────────────────────────────────────────────────────────────────────────

    function test_fullRoundTrip() public {
        uint256 amount = 200e6;

        // Step 1: Alice deposits USDC.
        vm.startPrank(alice);
        usdc.approve(address(router), amount);
        uint64 depositNonce = router.depositAndBridge(amount, stellarHubVault);
        vm.stopPrank();

        assertEq(mockCctp.getBurn(depositNonce).amount, amount);

        // Step 2: Hub issues mint auth (simulated via Axelar GMP).
        bytes32 mintAuthId = keccak256(abi.encodePacked("ROUND_TRIP", depositNonce));
        bytes memory mintPayload = abi.encode(
            MSG_REMOTE_MINT_AUTH,
            mintAuthId,
            alice,
            amount,
            block.timestamp + 7200
        );
        bytes32 cmdId = keccak256("cmd-round-trip");
        axelarGw.approveContractCall(cmdId, HUB_AXELAR_CHAIN, HUB_AXELAR_ADDRESS, keccak256(mintPayload));
        router.execute(cmdId, HUB_AXELAR_CHAIN, HUB_AXELAR_ADDRESS, mintPayload);

        assertEq(fusd.balanceOf(alice), amount, "fUSD not minted");

        // Step 3: Alice redeems all fUSD back.
        bytes32 aliceStellarAddr = bytes32(uint256(uint160(alice)));
        vm.startPrank(alice);
        fusd.approve(address(router), amount);
        uint64 redeemNonce = router.burnRemoteFusdAndRedeem(amount, aliceStellarAddr);
        vm.stopPrank();

        assertEq(fusd.balanceOf(alice), 0, "fUSD should be fully burned");
        assertEq(mockCctp.getBurn(redeemNonce).amount, amount, "Redeem CCTP burn amount wrong");
        // Alice's USDC back on Stellar (simulated by hub receiving CCTP message with redeemNonce).
    }

    // ─────────────────────────────────────────────────────────────────────────
    // RemoteRouter — input validation
    // ─────────────────────────────────────────────────────────────────────────

    function test_depositZeroReverts() public {
        vm.startPrank(alice);
        vm.expectRevert(RemoteRouter.ZeroAmount.selector);
        router.depositAndBridge(0, stellarHubVault);
        vm.stopPrank();
    }

    function test_redeemZeroReverts() public {
        vm.startPrank(alice);
        vm.expectRevert(RemoteRouter.ZeroAmount.selector);
        router.burnRemoteFusdAndRedeem(0, bytes32(uint256(uint160(alice))));
        vm.stopPrank();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // RemoteRouter — admin management
    // ─────────────────────────────────────────────────────────────────────────

    function test_setAdmin_nonAdminReverts() public {
        vm.startPrank(alice);
        vm.expectRevert(RemoteRouter.OnlyAdmin.selector);
        router.setAdmin(alice);
        vm.stopPrank();
    }

    function test_setAdmin_zeroAddressReverts() public {
        vm.expectRevert(RemoteRouter.ZeroAddress.selector);
        router.setAdmin(address(0));
    }

    function test_setAdmin_works() public {
        address newAdmin = makeAddr("newAdmin");
        router.setAdmin(newAdmin);
        assertEq(router.admin(), newAdmin, "admin should be updated");

        // Old admin (this contract = test contract = original admin) can no longer call.
        vm.expectRevert(RemoteRouter.OnlyAdmin.selector);
        router.setAdmin(address(this));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // RemoteRouter — rescueUsdc
    // ─────────────────────────────────────────────────────────────────────────

    function test_rescueUsdc_worksForAdmin() public {
        // Router has 500e6 USDC pre-funded in setUp.
        uint256 routerBefore = usdc.balanceOf(address(router));
        address treasury = makeAddr("treasury");
        router.rescueUsdc(treasury, 100e6);
        assertEq(usdc.balanceOf(treasury), 100e6, "treasury received USDC");
        assertEq(usdc.balanceOf(address(router)), routerBefore - 100e6);
    }

    function test_rescueUsdc_nonAdminReverts() public {
        vm.startPrank(alice);
        vm.expectRevert(RemoteRouter.OnlyAdmin.selector);
        router.rescueUsdc(alice, 1e6);
        vm.stopPrank();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // RemoteFusd — access control
    // ─────────────────────────────────────────────────────────────────────────

    function test_fusd_nonRouterCannotMint() public {
        vm.startPrank(alice);
        vm.expectRevert(RemoteFusd.OnlyRouter.selector);
        fusd.mint(alice, 1e6);
        vm.stopPrank();
    }

    function test_fusd_nonRouterCannotBurn() public {
        _mintFusdToAlice(100e6);
        vm.startPrank(alice);
        vm.expectRevert(RemoteFusd.OnlyRouter.selector);
        fusd.burn(alice, 1e6);
        vm.stopPrank();
    }

    function test_fusd_mintZeroReverts() public {
        // Deploy a test-controlled wrapper: call burn via router path.
        // Easier: check via a direct call from the router's address.
        vm.startPrank(address(router));
        vm.expectRevert(RemoteFusd.ZeroAmount.selector);
        fusd.mint(alice, 0);
        vm.stopPrank();
    }

    function test_fusd_burnZeroReverts() public {
        vm.startPrank(address(router));
        vm.expectRevert(RemoteFusd.ZeroAmount.selector);
        fusd.burn(alice, 0);
        vm.stopPrank();
    }

    function test_fusd_burnExceedsBalance() public {
        _mintFusdToAlice(50e6);
        // Router can burn beyond balance — ERC-20 _burn will revert with ERC20InsufficientBalance.
        vm.startPrank(address(router));
        vm.expectRevert(); // OpenZeppelin ERC20InsufficientBalance
        fusd.burn(alice, 51e6);
        vm.stopPrank();
    }

    function test_fusd_decimalsAre6() public view {
        assertEq(fusd.decimals(), 6);
    }

    function test_fusd_routerIsImmutable() public view {
        assertEq(fusd.router(), address(router));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // RemoteRouter — ack payload correctness
    // ─────────────────────────────────────────────────────────────────────────

    function test_ackPayloadContainsMintAuthId() public {
        bytes32 mintAuthId = keccak256("ACK_PAYLOAD_TEST");
        bytes memory gmpPayload = abi.encode(
            MSG_REMOTE_MINT_AUTH,
            mintAuthId,
            alice,
            10e6,
            block.timestamp + 3600
        );
        bytes32 commandId = keccak256("cmd-ack-test");
        axelarGw.approveContractCall(commandId, HUB_AXELAR_CHAIN, HUB_AXELAR_ADDRESS, keccak256(gmpPayload));
        router.execute(commandId, HUB_AXELAR_CHAIN, HUB_AXELAR_ADDRESS, gmpPayload);

        // Check the ack sent to hub contains the correct mintAuthId.
        MockAxelarGateway.SentMessage memory ack = axelarGw.lastSentMessage();
        (bytes4 msgType, bytes32 ackMintAuthId) = abi.decode(ack.payload, (bytes4, bytes32));
        assertEq(msgType, MSG_REMOTE_MINT_EXECUTED, "ack message type");
        assertEq(ackMintAuthId, mintAuthId, "ack should contain the original mintAuthId");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Multiple users
    // ─────────────────────────────────────────────────────────────────────────

    function test_multipleUsersIndependent() public {
        // Alice and Bob both deposit, each receive independent fUSD.
        usdc.mint(bob, 500e6);

        // Alice deposits 100 USDC.
        vm.prank(alice);
        usdc.approve(address(router), 100e6);
        vm.prank(alice);
        router.depositAndBridge(100e6, stellarHubVault);

        // Bob deposits 200 USDC.
        vm.prank(bob);
        usdc.approve(address(router), 200e6);
        vm.prank(bob);
        router.depositAndBridge(200e6, stellarHubVault);

        // Hub issues separate mint auths for each.
        _mintFusdToUser(alice, 100e6);
        _mintFusdToUser(bob,   200e6);

        assertEq(fusd.balanceOf(alice), 100e6);
        assertEq(fusd.balanceOf(bob),   200e6);
        assertEq(fusd.totalSupply(),    300e6);
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    function _mintFusdToAlice(uint256 amount) internal {
        _mintFusdToUser(alice, amount);
    }

    function _mintFusdToUser(address user, uint256 amount) internal {
        bytes32 mintAuthId = keccak256(abi.encodePacked("SETUP_MINT", user, amount));
        bytes memory payload = abi.encode(
            MSG_REMOTE_MINT_AUTH,
            mintAuthId,
            user,
            amount,
            block.timestamp + 7200
        );
        bytes32 cmdId = keccak256(abi.encodePacked("cmd-setup", mintAuthId));
        axelarGw.approveContractCall(cmdId, HUB_AXELAR_CHAIN, HUB_AXELAR_ADDRESS, keccak256(payload));
        router.execute(cmdId, HUB_AXELAR_CHAIN, HUB_AXELAR_ADDRESS, payload);
    }
}
