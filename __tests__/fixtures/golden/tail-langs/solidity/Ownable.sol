// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;
abstract contract Ownable {
    address public owner;
    modifier onlyOwner() { require(msg.sender == owner, "not owner"); _; }
    constructor() { owner = msg.sender; }
    function transferOwnership(address to) external onlyOwner { owner = to; }
}
