// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "forge-std/Script.sol";
import {PhantomSettlement} from "../src/PhantomSettlement.sol";

/// @title DeploySettlement
/// @notice Deployment script for the PhantomSettlement contract.
/// @dev Usage:
///   forge script script/DeploySettlement.s.sol:DeploySettlement \
///     --rpc-url $RPC_URL --broadcast --verify \
///     -vvvv
///
///   Environment variables:
///     DEPLOYER_PRIVATE_KEY — Private key of the deployer/owner.
///     OWNER_ADDRESS — (Optional) Owner address. Defaults to deployer.
contract DeploySettlement is Script {
    function run() external {
        uint256 deployerKey = vm.envUint("DEPLOYER_PRIVATE_KEY");
        address owner = vm.envOr("OWNER_ADDRESS", vm.addr(deployerKey));

        vm.startBroadcast(deployerKey);

        PhantomSettlement settlement = new PhantomSettlement(owner);

        vm.stopBroadcast();

        console.log("PhantomSettlement deployed at:", address(settlement));
        console.log("Owner:", owner);
    }
}
