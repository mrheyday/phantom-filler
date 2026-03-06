// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {ResolvedOrder} from "../types/OrderTypes.sol";

/// @title IFiller
/// @notice Interface for filler contracts that receive callbacks from reactors.
/// @dev Implement this interface to participate in just-in-time (JIT) filling,
///      where the reactor calls back into the filler after resolving orders,
///      giving the filler a chance to source liquidity before settlement.
interface IFiller {
    /// @notice Called by the reactor after resolving orders during `executeWithCallback`.
    /// @dev The filler must ensure the required output tokens are available for
    ///      transfer by the end of this call. The reactor will pull output tokens
    ///      from the filler after this callback returns.
    ///
    ///      SECURITY: Must verify that `msg.sender` is a trusted reactor address.
    ///
    /// @param resolvedOrders The resolved orders with decoded input/output parameters.
    /// @param fillerData Arbitrary data forwarded from the original execute call.
    function reactorCallback(ResolvedOrder[] calldata resolvedOrders, bytes calldata fillerData) external;
}
