// SPDX-License-Identifier: MIT

pragma solidity ^0.8.24;

import "../../../canonical/arbitrum/IOutbox.sol";
import "./BridgeMock.sol";

contract OutboxMock is IOutbox {
    address public bridge;
    mapping(uint256 => bool) public spent;
    address private _l2ToL1Sender;

    constructor(address _bridge) {
        bridge = _bridge;
    }

    function l2ToL1Sender() external view returns (address) {
        return _l2ToL1Sender;
    }

    function isSpent(uint256 index) external view returns (bool) {
        return spent[index];
    }

    function executeTransaction(
        bytes32[] calldata,
        uint256 index,
        address l2Sender,
        address to,
        uint256,
        uint256,
        uint256,
        uint256,
        bytes calldata data
    ) external {
        require(!spent[index], "Already spent");
        spent[index] = true;

        _l2ToL1Sender = l2Sender;
        BridgeMock(bridge).executeL1Message(to, data);
        _l2ToL1Sender = address(0);
    }

    receive() external payable {}
}
