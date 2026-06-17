// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @notice Axelar gateway — send GMP messages and validate inbound ones.
interface IAxelarGateway {
    /// @dev Send a cross-chain GMP message.
    function callContract(
        string calldata destinationChain,
        string calldata destinationContractAddress,
        bytes calldata payload
    ) external;

    /// @dev Validate that a GMP message was approved by Axelar consensus.
    function validateContractCall(
        bytes32 commandId,
        string calldata sourceChain,
        string calldata sourceAddress,
        bytes32 payloadHash
    ) external returns (bool);
}

/// @notice Axelar gas service — prepay gas for cross-chain calls.
interface IAxelarGasService {
    function payNativeGasForContractCall(
        address sender,
        string calldata destinationChain,
        string calldata destinationAddress,
        bytes calldata payload,
        address refundAddress
    ) external payable;
}

/// @notice Implement this interface to receive Axelar GMP messages.
interface IAxelarExecutable {
    function execute(
        bytes32 commandId,
        string calldata sourceChain,
        string calldata sourceAddress,
        bytes calldata payload
    ) external;
}
