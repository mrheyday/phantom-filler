// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {SignedOrder, ResolvedOrder} from "../types/OrderTypes.sol";

/// @title IReactor
/// @notice Interface for the UniswapX-style reactor that resolves and settles orders.
/// @dev Reactors validate signatures, resolve order parameters, and coordinate
///      token transfers between swappers and fillers.
interface IReactor {
    /// @notice Emitted when an order is successfully filled.
    /// @param orderHash The unique hash identifying the filled order.
    /// @param filler The address that filled the order.
    /// @param swapper The address of the order creator.
    /// @param nonce The order nonce (for replay protection).
    event OrderFilled(bytes32 indexed orderHash, address indexed filler, address indexed swapper, uint256 nonce);

    /// @notice Executes a single signed order.
    /// @dev The caller (filler) must have sufficient tokens approved.
    ///      The reactor validates the signature, resolves the order, and
    ///      coordinates token transfers.
    /// @param order The signed order to execute.
    /// @param fillerData Arbitrary data passed to the filler callback (if applicable).
    function execute(SignedOrder calldata order, bytes calldata fillerData) external;

    /// @notice Executes multiple signed orders in a single transaction.
    /// @dev All orders must be fillable; reverts if any order fails.
    /// @param orders The signed orders to execute.
    /// @param fillerData Arbitrary data passed to the filler callback.
    function executeBatch(SignedOrder[] calldata orders, bytes calldata fillerData) external;

    /// @notice Executes a single order with a callback to the filler contract.
    /// @dev After resolving the order, the reactor calls `reactorCallback` on
    ///      `msg.sender`, allowing the filler to source liquidity just-in-time.
    /// @param order The signed order to execute.
    /// @param fillerData Arbitrary data forwarded to the callback.
    function executeWithCallback(SignedOrder calldata order, bytes calldata fillerData) external;

    /// @notice Resolves a signed order without executing it.
    /// @dev Useful for off-chain simulation and fill evaluation.
    /// @param order The signed order to resolve.
    /// @return resolved The fully resolved order with decoded parameters.
    function resolve(SignedOrder calldata order) external view returns (ResolvedOrder memory resolved);
}
