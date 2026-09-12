// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;
import "./Ownable.sol";
interface IGreeter { function greet() external view returns (string memory); }
contract Greeter is IGreeter, Ownable {
    string private greeting;
    event GreetingChanged(string greeting);
    constructor(string memory _greeting) { greeting = _greeting; }
    function greet() external view override returns (string memory) { return greeting; }
    function setGreeting(string memory _greeting) external onlyOwner {
        greeting = _greeting;
        emit GreetingChanged(_greeting);
    }
    function shout() external view returns (string memory) { return _upper(greeting); }
    function _upper(string memory s) internal pure returns (string memory) { return s; }
}
