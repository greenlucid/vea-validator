// SPDX-License-Identifier: MIT

pragma solidity ^0.8.24;

import "../../../canonical/arbitrum/IArbSys.sol";

contract ArbSysMockForValidator is IArbSys {
    uint256 public nextPosition;

    event L2ToL1Tx(
        address indexed caller,
        address indexed destination,
        uint256 indexed hash,
        uint256 position,
        uint256 arbBlockNum,
        uint256 ethBlockNum,
        uint256 timestamp,
        uint256 callvalue,
        bytes data
    );

    function sendTxToL1(address destination, bytes calldata data) external payable returns (uint256) {
        uint256 position = nextPosition++;
        uint256 hash = uint256(keccak256(abi.encodePacked(msg.sender, destination, position, data)));

        emit L2ToL1Tx(
            msg.sender,
            destination,
            hash,
            position,
            block.number,
            0,
            block.timestamp,
            msg.value,
            data
        );

        return position;
    }

    function sendMerkleTreeState() external view returns (uint64 size, bytes32 root, bytes32[] memory partials) {
        size = uint64(nextPosition);
        root = bytes32(uint256(1));
        partials = new bytes32[](0);
    }
}
