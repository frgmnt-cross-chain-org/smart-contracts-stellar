// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "../interfaces/ICCTP.sol";

/// @notice PoC mock of Circle CCTP v2 TokenMessenger.
/// Emits a burn event and tracks pending burns for test assertions.
/// Production: replaced by Circle's deployed TokenMessengerV2.
contract MockCCTP is ITokenMessengerV2 {
    struct BurnRecord {
        address sender;
        address burnToken;
        uint256 amount;
        uint32 destinationDomain;
        bytes32 mintRecipient;
        uint64 nonce;
        uint32 finalityThreshold;
        bytes hookData;
    }

    uint64 public nextNonce = 1;
    mapping(uint64 => BurnRecord) public burns;

    event BurnInitiated(
        uint64 indexed nonce,
        address indexed sender,
        uint32 destinationDomain,
        bytes32 mintRecipient,
        uint256 amount,
        uint32 finalityThreshold
    );

    function depositForBurnWithHook(
        uint256 amount,
        uint32 destinationDomain,
        bytes32 mintRecipient,
        address burnToken,
        bytes32 /* destinationCaller */,
        uint256 /* maxFee */,
        uint32 minFinalityThreshold,
        bytes calldata hookData
    ) external returns (uint64 nonce) {
        require(amount > 0, "MockCCTP: zero amount");

        // Pull USDC from sender.
        IERC20(burnToken).transferFrom(msg.sender, address(this), amount);

        nonce = nextNonce++;
        burns[nonce] = BurnRecord({
            sender: msg.sender,
            burnToken: burnToken,
            amount: amount,
            destinationDomain: destinationDomain,
            mintRecipient: mintRecipient,
            nonce: nonce,
            finalityThreshold: minFinalityThreshold,
            hookData: hookData
        });

        emit BurnInitiated(nonce, msg.sender, destinationDomain, mintRecipient, amount, minFinalityThreshold);
    }

    // In tests: simulate Stellar receiving the CCTP message by returning burn details.
    function getBurn(uint64 nonce) external view returns (BurnRecord memory) {
        return burns[nonce];
    }
}

/// @notice Mock MessageTransmitter — simulates receiving USDC on destination chain.
contract MockMessageTransmitter is IMessageTransmitterV2 {
    address public usdc;

    constructor(address _usdc) {
        usdc = _usdc;
    }

    mapping(bytes32 => bool) public consumed;

    event MessageReceived(bytes32 indexed messageHash, address recipient, uint256 amount);

    function receiveMessage(bytes calldata message, bytes calldata /* attestation */) external returns (bool) {
        // In production: parse Circle's ABI-encoded message.
        // Mock: decode (recipient, amount) directly from test-supplied bytes.
        (address recipient, uint256 amount) = abi.decode(message, (address, uint256));
        bytes32 msgHash = keccak256(message);
        require(!consumed[msgHash], "MockCCTP: message already consumed");
        consumed[msgHash] = true;

        IERC20(usdc).transfer(recipient, amount);
        emit MessageReceived(msgHash, recipient, amount);
        return true;
    }

    function usedNonces(bytes32) external pure returns (uint256) { return 0; }

    // Fund the transmitter for test simulation.
    function fund(uint256 amount) external {
        IERC20(usdc).transferFrom(msg.sender, address(this), amount);
    }
}
