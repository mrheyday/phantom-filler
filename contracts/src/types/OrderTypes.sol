// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title OrderTypes
/// @notice Shared type definitions for the Phantom Filler system.
/// @dev Mirrors UniswapX order structures for reactor compatibility.

/// @notice A signed order ready for submission to a reactor.
struct SignedOrder {
    /// @dev The ABI-encoded order data.
    bytes order;
    /// @dev The EIP-712 signature over the order.
    bytes sig;
}

/// @notice A token input required from the swapper.
struct InputToken {
    /// @dev The ERC-20 token address.
    address token;
    /// @dev The exact amount to transfer from the swapper.
    uint256 amount;
    /// @dev The maximum amount the swapper is willing to provide.
    uint256 maxAmount;
}

/// @notice A token output that must be delivered to the recipient.
struct OutputToken {
    /// @dev The ERC-20 token address.
    address token;
    /// @dev The minimum amount to deliver.
    uint256 amount;
    /// @dev The address that receives the output tokens.
    address recipient;
}

/// @notice A fully resolved order with decoded parameters.
struct ResolvedOrder {
    /// @dev The order signer (swapper).
    address signer;
    /// @dev The input token the swapper provides.
    InputToken input;
    /// @dev The output tokens that must be delivered.
    OutputToken[] outputs;
    /// @dev The unique order hash for deduplication.
    bytes32 orderHash;
    /// @dev Deadline timestamp after which the order expires.
    uint256 deadline;
}

/// @notice Parameters for a Dutch auction decay curve.
struct DutchAuctionParams {
    /// @dev Timestamp when the auction decay begins.
    uint256 decayStartTime;
    /// @dev Timestamp when the auction decay ends (price is at minimum).
    uint256 decayEndTime;
    /// @dev Starting output amount (maximum, before decay).
    uint256 startAmount;
    /// @dev Ending output amount (minimum, after full decay).
    uint256 endAmount;
}

/// @notice Result of a fill execution.
struct FillResult {
    /// @dev The order hash that was filled.
    bytes32 orderHash;
    /// @dev The filler address that executed the fill.
    address filler;
    /// @dev The actual input amount transferred.
    uint256 inputAmount;
    /// @dev The actual output amount delivered.
    uint256 outputAmount;
    /// @dev The block timestamp when the fill was executed.
    uint256 filledAt;
}
