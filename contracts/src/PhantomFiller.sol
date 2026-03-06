// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {IReactor} from "./interfaces/IReactor.sol";
import {IFiller} from "./interfaces/IFiller.sol";
import {SignedOrder, ResolvedOrder} from "./types/OrderTypes.sol";

/// @title PhantomFiller
/// @notice Core filler contract for the Phantom Filler intent execution engine.
/// @dev Interacts with UniswapX-style reactors to fill signed swap intents.
///      Supports direct fills and callback-based JIT fills. Access-controlled
///      via whitelisted reactors and authorized filler addresses.
contract PhantomFiller is IFiller, Ownable, ReentrancyGuard {
    using SafeERC20 for IERC20;

    // ─── Events ──────────────────────────────────────────────────────

    /// @notice Emitted when a reactor is added to the whitelist.
    event ReactorAdded(address indexed reactor);

    /// @notice Emitted when a reactor is removed from the whitelist.
    event ReactorRemoved(address indexed reactor);

    /// @notice Emitted when a filler address is authorized.
    event FillerAuthorized(address indexed filler);

    /// @notice Emitted when a filler address is deauthorized.
    event FillerDeauthorized(address indexed filler);

    /// @notice Emitted when a fill is executed via a reactor.
    event FillExecuted(address indexed reactor, address indexed filler);

    /// @notice Emitted when tokens are withdrawn by the owner.
    event TokenWithdrawn(address indexed token, address indexed to, uint256 amount);

    // ─── Errors ──────────────────────────────────────────────────────

    /// @notice Caller is not an authorized filler.
    error UnauthorizedFiller();

    /// @notice The reactor address is not whitelisted.
    error UnauthorizedReactor();

    /// @notice The address is the zero address.
    error ZeroAddress();

    // ─── State ───────────────────────────────────────────────────────

    /// @notice Whitelisted reactor contracts that this filler trusts.
    mapping(address => bool) public whitelistedReactors;

    /// @notice Authorized filler EOAs that can trigger fills.
    mapping(address => bool) public authorizedFillers;

    // ─── Constructor ─────────────────────────────────────────────────

    /// @notice Initializes the filler contract with the owner.
    /// @param _owner The initial owner of the contract.
    constructor(address _owner) Ownable(_owner) {
        authorizedFillers[_owner] = true;
        emit FillerAuthorized(_owner);
    }

    // ─── Modifiers ───────────────────────────────────────────────────

    /// @dev Restricts access to authorized filler addresses.
    modifier onlyAuthorizedFiller() {
        _checkAuthorizedFiller();
        _;
    }

    /// @dev Restricts access to whitelisted reactor contracts.
    modifier onlyWhitelistedReactor() {
        _checkWhitelistedReactor();
        _;
    }

    function _checkAuthorizedFiller() internal view {
        if (!authorizedFillers[msg.sender]) revert UnauthorizedFiller();
    }

    function _checkWhitelistedReactor() internal view {
        if (!whitelistedReactors[msg.sender]) revert UnauthorizedReactor();
    }

    // ─── Admin Functions ─────────────────────────────────────────────

    /// @notice Adds a reactor to the whitelist.
    /// @param reactor The reactor contract address.
    function addReactor(address reactor) external onlyOwner {
        if (reactor == address(0)) revert ZeroAddress();
        whitelistedReactors[reactor] = true;
        emit ReactorAdded(reactor);
    }

    /// @notice Removes a reactor from the whitelist.
    /// @param reactor The reactor contract address.
    function removeReactor(address reactor) external onlyOwner {
        whitelistedReactors[reactor] = false;
        emit ReactorRemoved(reactor);
    }

    /// @notice Authorizes a filler address to trigger fills.
    /// @param filler The filler EOA address.
    function authorizeFiller(address filler) external onlyOwner {
        if (filler == address(0)) revert ZeroAddress();
        authorizedFillers[filler] = true;
        emit FillerAuthorized(filler);
    }

    /// @notice Deauthorizes a filler address.
    /// @param filler The filler EOA address.
    function deauthorizeFiller(address filler) external onlyOwner {
        authorizedFillers[filler] = false;
        emit FillerDeauthorized(filler);
    }

    /// @notice Approves a token for a reactor to spend.
    /// @dev Required before the reactor can pull output tokens from this contract.
    /// @param token The ERC-20 token address.
    /// @param spender The address to approve (typically a reactor).
    /// @param amount The approval amount.
    function approveToken(address token, address spender, uint256 amount) external onlyOwner {
        IERC20(token).forceApprove(spender, amount);
    }

    /// @notice Withdraws tokens from the contract to a specified address.
    /// @param token The ERC-20 token address.
    /// @param to The recipient address.
    /// @param amount The amount to withdraw.
    function withdrawToken(address token, address to, uint256 amount) external onlyOwner {
        if (to == address(0)) revert ZeroAddress();
        IERC20(token).safeTransfer(to, amount);
        emit TokenWithdrawn(token, to, amount);
    }

    /// @notice Withdraws ETH from the contract.
    /// @param to The recipient address.
    /// @param amount The amount of ETH to withdraw.
    function withdrawEth(address payable to, uint256 amount) external onlyOwner {
        if (to == address(0)) revert ZeroAddress();
        (bool success,) = to.call{value: amount}("");
        require(success, "ETH transfer failed");
    }

    // ─── Fill Functions ──────────────────────────────────────────────

    /// @notice Fills a single order by calling the reactor's execute function.
    /// @dev The contract must have sufficient output tokens and reactor approval.
    /// @param reactor The reactor contract to execute through.
    /// @param order The signed order to fill.
    /// @param fillerData Arbitrary data forwarded to the reactor.
    function fill(address reactor, SignedOrder calldata order, bytes calldata fillerData)
        external
        onlyAuthorizedFiller
        nonReentrant
    {
        if (!whitelistedReactors[reactor]) revert UnauthorizedReactor();
        IReactor(reactor).execute(order, fillerData);
        emit FillExecuted(reactor, msg.sender);
    }

    /// @notice Fills a single order using the callback pattern for JIT liquidity.
    /// @dev The reactor will call `reactorCallback` on this contract during execution.
    /// @param reactor The reactor contract to execute through.
    /// @param order The signed order to fill.
    /// @param fillerData Arbitrary data forwarded to the callback.
    function fillWithCallback(address reactor, SignedOrder calldata order, bytes calldata fillerData)
        external
        onlyAuthorizedFiller
        nonReentrant
    {
        if (!whitelistedReactors[reactor]) revert UnauthorizedReactor();
        IReactor(reactor).executeWithCallback(order, fillerData);
        emit FillExecuted(reactor, msg.sender);
    }

    /// @notice Fills multiple orders in a single transaction.
    /// @param reactor The reactor contract to execute through.
    /// @param orders The signed orders to fill.
    /// @param fillerData Arbitrary data forwarded to the reactor.
    function fillBatch(address reactor, SignedOrder[] calldata orders, bytes calldata fillerData)
        external
        onlyAuthorizedFiller
        nonReentrant
    {
        if (!whitelistedReactors[reactor]) revert UnauthorizedReactor();
        IReactor(reactor).executeBatch(orders, fillerData);
        emit FillExecuted(reactor, msg.sender);
    }

    // ─── Callback ────────────────────────────────────────────────────

    /// @inheritdoc IFiller
    /// @dev Called by the reactor during executeWithCallback. The filler must
    ///      ensure output tokens are available. Only whitelisted reactors can call.
    function reactorCallback(ResolvedOrder[] calldata, bytes calldata) external onlyWhitelistedReactor {
        // The reactor calls this during executeWithCallback.
        // Output tokens should already be in the contract or sourced here.
        // In production, this is where JIT liquidity sourcing logic goes
        // (e.g., DEX swaps, flash loans, cross-chain bridges).
        //
        // For now, we rely on pre-funded balances in the contract.
        // The reactor will pull approved tokens after this returns.
    }

    /// @notice Allows the contract to receive ETH (e.g., from WETH unwrapping).
    receive() external payable {}
}
