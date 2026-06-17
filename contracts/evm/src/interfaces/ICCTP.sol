// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @notice Minimal Circle CCTP v2 TokenMessenger interface.
interface ITokenMessengerV2 {
    /// @dev Burns `amount` of `burnToken` and initiates CCTP transfer to `destinationDomain`.
    /// @param amount          Amount of USDC to burn (6 decimals).
    /// @param destinationDomain  Circle CCTP domain of the destination chain.
    /// @param mintRecipient   Destination address padded to 32 bytes.
    /// @param burnToken       Address of USDC on this chain.
    /// @param destinationCaller  Allowlisted caller on destination (zero = permissionless).
    /// @param maxFee          Maximum relay fee the sender accepts (6 decimals).
    /// @param minFinalityThreshold  1000 = confirmed, 2000 = finalized.
    /// @return nonce          Unique nonce for this burn.
    function depositForBurnWithHook(
        uint256 amount,
        uint32 destinationDomain,
        bytes32 mintRecipient,
        address burnToken,
        bytes32 destinationCaller,
        uint256 maxFee,
        uint32 minFinalityThreshold,
        bytes calldata hookData
    ) external returns (uint64 nonce);
}

/// @notice Minimal Circle CCTP v2 MessageTransmitter interface.
interface IMessageTransmitterV2 {
    function receiveMessage(bytes calldata message, bytes calldata attestation) external returns (bool);
    function usedNonces(bytes32 sourceAndNonce) external view returns (uint256);
}
