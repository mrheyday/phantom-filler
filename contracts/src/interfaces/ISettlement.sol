// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {FillResult} from "../types/OrderTypes.sol";

/// @title ISettlement
/// @notice Interface for settlement tracking and verification.
/// @dev Provides on-chain verification of fill execution results and
///      settlement status for the Phantom Filler system.
interface ISettlement {
    /// @notice Emitted when a fill is recorded in the settlement ledger.
    /// @param orderHash The filled order hash.
    /// @param filler The filler address.
    /// @param inputAmount The actual input amount transferred.
    /// @param outputAmount The actual output amount delivered.
    event FillSettled(
        bytes32 indexed orderHash, address indexed filler, uint256 inputAmount, uint256 outputAmount
    );

    /// @notice Emitted when a fill is disputed.
    /// @param orderHash The disputed order hash.
    /// @param disputedBy The address that raised the dispute.
    event FillDisputed(bytes32 indexed orderHash, address indexed disputedBy);

    /// @notice Records a completed fill in the settlement ledger.
    /// @dev Only callable by authorized fillers or the reactor.
    /// @param result The fill execution result to record.
    function recordFill(FillResult calldata result) external;

    /// @notice Returns the settlement status for a given order hash.
    /// @param orderHash The order hash to query.
    /// @return settled Whether the order has been settled.
    /// @return filler The filler address (zero if not settled).
    /// @return settledAt The block timestamp of settlement (0 if not settled).
    function getSettlement(bytes32 orderHash) external view returns (bool settled, address filler, uint256 settledAt);

    /// @notice Checks whether an order has been filled.
    /// @param orderHash The order hash to check.
    /// @return True if the order has been settled.
    function isFilled(bytes32 orderHash) external view returns (bool);

    /// @notice Returns the total number of settled fills.
    /// @return The count of all recorded settlements.
    function totalSettlements() external view returns (uint256);
}
