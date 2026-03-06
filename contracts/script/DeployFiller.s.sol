// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "forge-std/Script.sol";
import {PhantomFiller} from "../src/PhantomFiller.sol";

/// @title DeployFiller
/// @notice Deployment script for the PhantomFiller contract.
/// @dev Usage:
///   forge script script/DeployFiller.s.sol:DeployFiller \
///     --rpc-url $RPC_URL --broadcast --verify \
///     -vvvv
///
///   Environment variables:
///     DEPLOYER_PRIVATE_KEY — Private key of the deployer/owner.
///     OWNER_ADDRESS — (Optional) Owner address. Defaults to deployer.
contract DeployFiller is Script {
    function run() external {
        uint256 deployerKey = vm.envUint("DEPLOYER_PRIVATE_KEY");
        address owner = vm.envOr("OWNER_ADDRESS", vm.addr(deployerKey));

        vm.startBroadcast(deployerKey);

        PhantomFiller filler = new PhantomFiller(owner);

        vm.stopBroadcast();

        console.log("PhantomFiller deployed at:", address(filler));
        console.log("Owner:", owner);
    }
}
