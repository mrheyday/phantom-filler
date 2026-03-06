// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "forge-std/Script.sol";
import {PhantomFiller} from "../src/PhantomFiller.sol";
import {PhantomSettlement} from "../src/PhantomSettlement.sol";

/// @title DeployAll
/// @notice Deploys the full Phantom Filler contract suite in a single transaction batch.
/// @dev Usage:
///   forge script script/DeployAll.s.sol:DeployAll \
///     --rpc-url $RPC_URL --broadcast --verify \
///     -vvvv
///
///   Environment variables:
///     DEPLOYER_PRIVATE_KEY — Private key of the deployer/owner.
///     OWNER_ADDRESS — (Optional) Owner address. Defaults to deployer.
///
///   After deployment, the filler contract is registered as an authorized
///   recorder on the settlement contract.
contract DeployAll is Script {
    function run() external {
        uint256 deployerKey = vm.envUint("DEPLOYER_PRIVATE_KEY");
        address owner = vm.envOr("OWNER_ADDRESS", vm.addr(deployerKey));

        vm.startBroadcast(deployerKey);

        PhantomFiller filler = new PhantomFiller(owner);
        PhantomSettlement settlement = new PhantomSettlement(owner);

        // Authorize the filler contract to record settlements.
        settlement.addRecorder(address(filler));

        vm.stopBroadcast();

        console.log("=== Phantom Filler Suite Deployed ===");
        console.log("PhantomFiller:     ", address(filler));
        console.log("PhantomSettlement: ", address(settlement));
        console.log("Owner:             ", owner);
        console.log("Filler authorized as settlement recorder");
    }
}
