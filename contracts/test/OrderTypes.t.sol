// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import {SignedOrder, InputToken, OutputToken, ResolvedOrder, DutchAuctionParams, FillResult} from "../src/types/OrderTypes.sol";

/// @title OrderTypesTest
/// @notice Tests that order type structs compile and can be constructed.
contract OrderTypesTest is Test {
    function test_inputToken_creation() public pure {
        InputToken memory input = InputToken({
            token: address(0x1),
            amount: 1 ether,
            maxAmount: 2 ether
        });
        assertEq(input.token, address(0x1));
        assertEq(input.amount, 1 ether);
        assertEq(input.maxAmount, 2 ether);
    }

    function test_outputToken_creation() public pure {
        OutputToken memory output = OutputToken({
            token: address(0x2),
            amount: 500,
            recipient: address(0x3)
        });
        assertEq(output.token, address(0x2));
        assertEq(output.amount, 500);
        assertEq(output.recipient, address(0x3));
    }

    function test_resolvedOrder_creation() public view {
        InputToken memory input = InputToken({
            token: address(0x1),
            amount: 1 ether,
            maxAmount: 1 ether
        });

        OutputToken[] memory outputs = new OutputToken[](1);
        outputs[0] = OutputToken({
            token: address(0x2),
            amount: 500,
            recipient: address(0x3)
        });

        ResolvedOrder memory order = ResolvedOrder({
            signer: address(0x10),
            input: input,
            outputs: outputs,
            orderHash: keccak256("test"),
            deadline: block.timestamp + 3600
        });

        assertEq(order.signer, address(0x10));
        assertEq(order.outputs.length, 1);
    }

    function test_dutchAuctionParams_creation() public pure {
        DutchAuctionParams memory params = DutchAuctionParams({
            decayStartTime: 1000,
            decayEndTime: 2000,
            startAmount: 100 ether,
            endAmount: 90 ether
        });
        assertEq(params.decayStartTime, 1000);
        assertEq(params.endAmount, 90 ether);
        assertGt(params.startAmount, params.endAmount);
    }

    function test_fillResult_creation() public pure {
        FillResult memory result = FillResult({
            orderHash: keccak256("order1"),
            filler: address(0x20),
            inputAmount: 1 ether,
            outputAmount: 2000e6,
            filledAt: 1700000000
        });
        assertEq(result.filler, address(0x20));
        assertEq(result.inputAmount, 1 ether);
    }

    function test_signedOrder_creation() public pure {
        SignedOrder memory signed = SignedOrder({
            order: hex"deadbeef",
            sig: hex"cafebabe"
        });
        assertEq(signed.order.length, 4);
        assertEq(signed.sig.length, 4);
    }
}
