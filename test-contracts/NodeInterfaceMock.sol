// SPDX-License-Identifier: MIT

pragma solidity ^0.8.24;

contract NodeInterfaceMock {
    function constructOutboxProof(
        uint64 size,
        uint64 leaf
    ) external pure returns (bytes32 send, bytes32 root, bytes32[] memory proof) {
        send = keccak256(abi.encodePacked("send", size, leaf));
        root = keccak256(abi.encodePacked("root", size, leaf));
        proof = new bytes32[](1);
        proof[0] = keccak256(abi.encodePacked("proof", size, leaf));
    }
}
