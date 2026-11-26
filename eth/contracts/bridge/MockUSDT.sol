// SPDX-License-Identifier: MIT OR Apache-2.0
pragma solidity 0.8.24;

/// @notice Test token that, like USDT, returns nothing from transfer and transferFrom.
contract MockUSDT {
    string public constant name = "Mock Tether (LEZ PoC)";
    string public constant symbol = "mUSDT";
    uint8 public constant decimals = 6;
    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    function mint(address to, uint256 v) external { balanceOf[to] += v; totalSupply += v; emit Transfer(address(0), to, v); }

    function approve(address s, uint256 v) external { allowance[msg.sender][s] = v; emit Approval(msg.sender, s, v); }

    function transfer(address to, uint256 v) external {
        require(balanceOf[msg.sender] >= v);
        balanceOf[msg.sender] -= v; balanceOf[to] += v; emit Transfer(msg.sender, to, v);
    }

    function transferFrom(address f, address to, uint256 v) external {
        require(balanceOf[f] >= v && allowance[f][msg.sender] >= v);
        allowance[f][msg.sender] -= v; balanceOf[f] -= v; balanceOf[to] += v; emit Transfer(f, to, v);
    }
}
