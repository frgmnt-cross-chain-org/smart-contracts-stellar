// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "../interfaces/IAxelarGateway.sol";

/// @notice Mock Axelar gateway for PoC testing.
/// In production: replaced by Axelar's deployed AxelarGateway.
contract MockAxelarGateway is IAxelarGateway {
    struct SentMessage {
        string destinationChain;
        string destinationAddress;
        bytes payload;
    }

    SentMessage[] public sentMessages;
    mapping(bytes32 => bool) public approvedCalls;

    event GmpSent(
        string destinationChain,
        string destinationAddress,
        bytes payload
    );

    function callContract(
        string calldata destinationChain,
        string calldata destinationContractAddress,
        bytes calldata payload
    ) external {
        sentMessages.push(SentMessage({
            destinationChain: destinationChain,
            destinationAddress: destinationContractAddress,
            payload: payload
        }));
        emit GmpSent(destinationChain, destinationContractAddress, payload);
    }

    // Test helper: pre-approve a GMP message to simulate Axelar consensus.
    function approveContractCall(
        bytes32 commandId,
        string calldata sourceChain,
        string calldata sourceAddress,
        bytes32 payloadHash
    ) external {
        bytes32 key = keccak256(abi.encode(commandId, sourceChain, sourceAddress, payloadHash));
        approvedCalls[key] = true;
    }

    function validateContractCall(
        bytes32 commandId,
        string calldata sourceChain,
        string calldata sourceAddress,
        bytes32 payloadHash
    ) external returns (bool) {
        bytes32 key = keccak256(abi.encode(commandId, sourceChain, sourceAddress, payloadHash));
        bool valid = approvedCalls[key];
        if (valid) {
            delete approvedCalls[key]; // one-time use
        }
        return valid;
    }

    function sentMessageCount() external view returns (uint256) {
        return sentMessages.length;
    }

    function lastSentMessage() external view returns (SentMessage memory) {
        return sentMessages[sentMessages.length - 1];
    }
}

/// @notice Mock gas service — no-op for PoC.
contract MockAxelarGasService is IAxelarGasService {
    event GasPaid(address sender, string destinationChain, uint256 value);

    function payNativeGasForContractCall(
        address sender,
        string calldata destinationChain,
        string calldata /* destinationAddress */,
        bytes calldata /* payload */,
        address /* refundAddress */
    ) external payable {
        emit GasPaid(sender, destinationChain, msg.value);
    }
}
