// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "./RemoteFusd.sol";
import "./interfaces/ICCTP.sol";
import "./interfaces/IAxelarGateway.sol";

/// @notice EVM spoke router for the fUSD cross-chain stablecoin system.
///
/// Three primary flows:
///
/// 1. DEPOSIT: User deposits USDC → CCTP burns → Stellar hub settles → hub issues
///    RemoteMintAuth via Axelar GMP → execute() mints fUSD to user.
///
/// 2. REDEEM: User calls burnRemoteFusdAndRedeem → fUSD burned → CCTP burns USDC
///    from router reserve → Stellar hub receives CCTP message, credits user USDC.
///    (Router must hold USDC reserve pre-funded via CCTP or rebalance.)
///
/// 3. REMOTE MINT (receive-side): Stellar hub sends RemoteMintAuthorize via Axelar.
///    execute() validates the GMP message and mints fUSD to the designated recipient.
///
/// Security invariants:
/// - execute() validates every inbound GMP via IAxelarGateway.validateContractCall.
/// - Only known hub chain + address accepted in execute().
/// - Mint auth IDs are single-use (tracked in usedMintAuths).
/// - CCTP nonce provided so Stellar hub can correlate the burn.
///
/// Decimal note: USDC on EVM = 6 decimals. fUSD = 6 decimals. No conversion needed here.
contract RemoteRouter is IAxelarExecutable {
    using SafeERC20 for IERC20;

    // ── Immutables ──────────────────────────────────────────────────────────
    IERC20  public immutable usdc;
    RemoteFusd public immutable fusd;
    ITokenMessengerV2 public immutable cctpMessenger;
    IMessageTransmitterV2 public immutable cctpTransmitter;
    IAxelarGateway public immutable axelarGateway;
    IAxelarGasService public immutable axelarGasService;

    /// @dev Circle CCTP domain of the Stellar hub (27).
    uint32 public immutable hubCctpDomain;
    /// @dev Axelar chain name of the Stellar hub (e.g. "stellar").
    string public hubAxelarChain;
    /// @dev Expected Axelar source address — the hub's Axelar gateway string address.
    string public hubAxelarAddress;

    address public admin;

    // ── State ────────────────────────────────────────────────────────────────
    /// @dev Tracks consumed mint auth IDs to prevent replay.
    mapping(bytes32 => bool) public usedMintAuths;

    // ── GMP message types ───────────────────────────────────────────────────
    /// @dev Payload selector: hub → spoke mint authorization.
    bytes4 public constant MSG_REMOTE_MINT_AUTH = bytes4(keccak256("RemoteMintAuthorize"));
    /// @dev Payload selector: spoke → hub mint executed acknowledgement.
    bytes4 public constant MSG_REMOTE_MINT_EXECUTED = bytes4(keccak256("RemoteMintExecuted"));

    // CCTP finality threshold: 2000 = finalized (fast-finality = 1000 in production).
    uint32 public constant FINALITY_FINALIZED = 2000;

    // ── Events ───────────────────────────────────────────────────────────────
    event DepositAndBridgeInitiated(
        address indexed user,
        uint256 amount,
        uint64 cctpNonce
    );
    event RemoteMintExecuted(
        bytes32 indexed mintAuthId,
        address indexed recipient,
        uint256 amount
    );
    event RedeemBridgeInitiated(
        address indexed user,
        uint256 fusdBurned,
        uint64 cctpNonce
    );
    event AckSent(bytes32 indexed mintAuthId, string hubChain);

    // ── Errors ───────────────────────────────────────────────────────────────
    error OnlyAdmin();
    error OnlyAxelarGateway();
    error UnknownHub(string sourceChain, string sourceAddress);
    error InvalidMsgType(bytes4 got);
    error MintAuthAlreadyUsed(bytes32 mintAuthId);
    error MintAuthExpired(bytes32 mintAuthId, uint256 expiry);
    error ZeroAmount();

    // ── Constructor ──────────────────────────────────────────────────────────
    constructor(
        address _usdc,
        address _fusd,
        address _cctpMessenger,
        address _cctpTransmitter,
        address _axelarGateway,
        address _axelarGasService,
        uint32 _hubCctpDomain,
        string memory _hubAxelarChain,
        string memory _hubAxelarAddress
    ) {
        usdc            = IERC20(_usdc);
        fusd            = RemoteFusd(_fusd);
        cctpMessenger   = ITokenMessengerV2(_cctpMessenger);
        cctpTransmitter = IMessageTransmitterV2(_cctpTransmitter);
        axelarGateway   = IAxelarGateway(_axelarGateway);
        axelarGasService= IAxelarGasService(_axelarGasService);
        hubCctpDomain   = _hubCctpDomain;
        hubAxelarChain  = _hubAxelarChain;
        hubAxelarAddress= _hubAxelarAddress;
        admin           = msg.sender;
    }

    modifier onlyAdmin() {
        if (msg.sender != admin) revert OnlyAdmin();
        _;
    }

    // ── Flow 1: Deposit & Bridge ─────────────────────────────────────────────

    /// @notice User deposits USDC. This initiates a CCTP burn to Stellar hub.
    /// Hub will settle, issue a RemoteMintAuth via Axelar, then we mint fUSD.
    ///
    /// @param amount        USDC amount (6 decimals).
    /// @param stellarRecipient  Stellar-encoded hub vault address as 32-byte key,
    ///                          padded right if shorter. Hub maps to user's deposit.
    function depositAndBridge(
        uint256 amount,
        bytes32 stellarRecipient
    ) external returns (uint64 cctpNonce) {
        if (amount == 0) revert ZeroAmount();

        // Pull USDC from user → approve messenger → burn via CCTP.
        usdc.safeTransferFrom(msg.sender, address(this), amount);
        usdc.forceApprove(address(cctpMessenger), amount);

        cctpNonce = cctpMessenger.depositForBurnWithHook(
            amount,
            hubCctpDomain,
            stellarRecipient,
            address(usdc),
            bytes32(0),         // destinationCaller: permissionless relay
            0,                  // maxFee: 0 for PoC
            FINALITY_FINALIZED,
            abi.encode(msg.sender) // hookData: original depositor for hub correlation
        );

        emit DepositAndBridgeInitiated(msg.sender, amount, cctpNonce);
    }

    // ── Flow 2: Remote Mint (Axelar GMP receive) ─────────────────────────────

    /// @notice Called by Axelar relayer when the hub sends a RemoteMintAuthorize GMP.
    /// Validates the message, checks expiry, marks auth used, mints fUSD.
    /// Sends RemoteMintExecuted ack back to hub.
    ///
    /// Payload ABI encoding (hub produces this):
    ///   bytes4  msgType   = MSG_REMOTE_MINT_AUTH
    ///   bytes32 mintAuthId
    ///   address recipient
    ///   uint256 amount
    ///   uint256 expiry    (unix timestamp, seconds)
    function execute(
        bytes32 commandId,
        string calldata sourceChain,
        string calldata sourceAddress,
        bytes calldata payload
    ) external override {
        // Authenticate: only Axelar gateway can call this.
        if (!axelarGateway.validateContractCall(commandId, sourceChain, sourceAddress, keccak256(payload))) {
            revert OnlyAxelarGateway();
        }

        // Authenticate hub identity.
        if (
            keccak256(bytes(sourceChain))   != keccak256(bytes(hubAxelarChain)) ||
            keccak256(bytes(sourceAddress)) != keccak256(bytes(hubAxelarAddress))
        ) {
            revert UnknownHub(sourceChain, sourceAddress);
        }

        // Decode payload.
        bytes4 msgType = bytes4(payload[:4]);
        if (msgType != MSG_REMOTE_MINT_AUTH) revert InvalidMsgType(msgType);

        (
            ,   // msgType already read
            bytes32 mintAuthId,
            address recipient,
            uint256 amount,
            uint256 expiry
        ) = abi.decode(payload, (bytes4, bytes32, address, uint256, uint256));

        // Check replay.
        if (usedMintAuths[mintAuthId]) revert MintAuthAlreadyUsed(mintAuthId);
        // Check expiry.
        if (block.timestamp > expiry) revert MintAuthExpired(mintAuthId, expiry);

        // Mark consumed before any external call (CEI pattern).
        usedMintAuths[mintAuthId] = true;

        // Mint fUSD to recipient.
        fusd.mint(recipient, amount);
        emit RemoteMintExecuted(mintAuthId, recipient, amount);

        // Send RemoteMintExecuted ack back to hub via Axelar.
        _sendAck(mintAuthId);
    }

    // ── Flow 3: Burn fUSD & Redeem USDC ─────────────────────────────────────

    /// @notice User burns fUSD; router initiates CCTP transfer back to Stellar hub
    /// which will then route USDC to the user's specified destination.
    ///
    /// @param fusdAmount     Amount of fUSD to burn (6 decimals).
    /// @param usdcRecipient  Stellar address (32-byte) to receive USDC on hub side.
    function burnRemoteFusdAndRedeem(
        uint256 fusdAmount,
        bytes32 usdcRecipient
    ) external returns (uint64 cctpNonce) {
        if (fusdAmount == 0) revert ZeroAmount();

        // Burn fUSD from msg.sender directly — no approval needed because RemoteFusd.burn
        // is onlyRouter and always burns from the address the router specifies.
        fusd.burn(msg.sender, fusdAmount);

        // Router must hold matching USDC reserve. In production the hub funds this
        // via rebalance; in PoC the test pre-funds the router.
        uint256 usdcAmount = fusdAmount; // 1:1, both 6 decimals

        usdc.forceApprove(address(cctpMessenger), usdcAmount);
        cctpNonce = cctpMessenger.depositForBurnWithHook(
            usdcAmount,
            hubCctpDomain,
            usdcRecipient,
            address(usdc),
            bytes32(0),
            0,
            FINALITY_FINALIZED,
            abi.encode(msg.sender) // hookData: original redeemer
        );

        emit RedeemBridgeInitiated(msg.sender, fusdAmount, cctpNonce);
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    function _sendAck(bytes32 mintAuthId) internal {
        bytes memory payload = abi.encode(MSG_REMOTE_MINT_EXECUTED, mintAuthId);

        // Pay gas with any ETH sent to this call; for PoC tests omit gas prepayment.
        if (address(this).balance > 0) {
            axelarGasService.payNativeGasForContractCall{value: address(this).balance}(
                address(this),
                hubAxelarChain,
                hubAxelarAddress,
                payload,
                admin
            );
        }

        axelarGateway.callContract(hubAxelarChain, hubAxelarAddress, payload);
        emit AckSent(mintAuthId, hubAxelarChain);
    }

    // ── Admin ────────────────────────────────────────────────────────────────

    error ZeroAddress();

    function setAdmin(address _admin) external onlyAdmin {
        if (_admin == address(0)) revert ZeroAddress();
        admin = _admin;
    }

    /// @notice Recover USDC accidentally sent to this contract.
    function rescueUsdc(address to, uint256 amount) external onlyAdmin {
        usdc.safeTransfer(to, amount);
    }

    receive() external payable {}
}
