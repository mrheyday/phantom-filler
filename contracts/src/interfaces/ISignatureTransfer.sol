// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// @title ISignatureTransfer
/// @notice Simplified interface for Permit2 signature-based token transfers.
/// @dev Used by UniswapX reactors to transfer tokens from swappers via
///      off-chain EIP-712 signatures instead of on-chain approvals.
interface ISignatureTransfer {
    /// @notice Token permission details for a transfer.
    struct TokenPermissions {
        /// @dev The ERC-20 token address.
        address token;
        /// @dev The maximum amount that can be transferred.
        uint256 amount;
    }

    /// @notice Transfer details for a permitted transfer.
    struct SignatureTransferDetails {
        /// @dev The recipient of the tokens.
        address to;
        /// @dev The amount to transfer (must be <= permitted amount).
        uint256 requestedAmount;
    }

    /// @notice The permit data for a single token transfer.
    struct PermitTransferFrom {
        /// @dev The token permission details.
        TokenPermissions permitted;
        /// @dev Unique nonce for replay protection.
        uint256 nonce;
        /// @dev Deadline timestamp for the permit.
        uint256 deadline;
    }

    /// @notice Transfers tokens using a signed permit.
    /// @dev Validates the signature, checks deadline and nonce, then transfers.
    /// @param permit The permit data signed by the token owner.
    /// @param transferDetails The transfer destination and amount.
    /// @param owner The token owner who signed the permit.
    /// @param signature The EIP-712 signature from the owner.
    function permitTransferFrom(
        PermitTransferFrom calldata permit,
        SignatureTransferDetails calldata transferDetails,
        address owner,
        bytes calldata signature
    ) external;

    /// @notice Returns the nonce bitmap for an address and word position.
    /// @dev Used for checking which nonces have been consumed.
    /// @param owner The address to check.
    /// @param wordPos The word position in the nonce bitmap.
    /// @return The bitmap of consumed nonces for the given word position.
    function nonceBitmap(address owner, uint256 wordPos) external view returns (uint256);
}
