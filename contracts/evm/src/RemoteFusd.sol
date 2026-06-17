// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "@openzeppelin/contracts/token/ERC20/ERC20.sol";

/// @notice EVM-side fUSD token. Hub-authorized mint/burn only.
/// Mirrors the SEP-41 fusd-token Soroban contract on the EVM spoke chain.
///
/// Security model:
/// - Only the designated `router` (RemoteRouter) may call mint/burn.
/// - Supply on this chain is always backed by a MintAuth from the Stellar hub.
/// - 6 decimals to match hub accounting and CCTP USDC.
contract RemoteFusd is ERC20 {
    address public immutable router;

    event RouterMint(address indexed to, uint256 amount);
    event RouterBurn(address indexed from, uint256 amount);

    error OnlyRouter();
    error ZeroAmount();

    modifier onlyRouter() {
        if (msg.sender != router) revert OnlyRouter();
        _;
    }

    constructor(address _router) ERC20("Fragmented USD", "fUSD") {
        require(_router != address(0), "RemoteFusd: zero router");
        router = _router;
    }

    /// @notice Mint fUSD to `to`. Called by RemoteRouter after hub MintAuth verified.
    function mint(address to, uint256 amount) external onlyRouter {
        if (amount == 0) revert ZeroAmount();
        _mint(to, amount);
        emit RouterMint(to, amount);
    }

    /// @notice Burn fUSD from `from`. Called by RemoteRouter before initiating CCTP redeem.
    function burn(address from, uint256 amount) external onlyRouter {
        if (amount == 0) revert ZeroAmount();
        _burn(from, amount);
        emit RouterBurn(from, amount);
    }

    /// @dev Override decimals — fUSD is always 6 decimal places.
    function decimals() public pure override returns (uint8) {
        return 6;
    }
}
