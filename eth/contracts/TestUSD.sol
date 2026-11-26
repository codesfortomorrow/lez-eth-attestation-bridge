// SPDX-License-Identifier: MIT OR Apache-2.0
pragma solidity 0.8.24;

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";

/// @notice 6-decimal test token. Anyone can mint; testnet only.
contract TestUSD is ERC20 {
    constructor(uint256 initialSupply) ERC20("Test USD (LEZ PoC)", "tUSD") {
        _mint(msg.sender, initialSupply);
    }

    function decimals() public pure override returns (uint8) {
        return 6;
    }

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }
}
