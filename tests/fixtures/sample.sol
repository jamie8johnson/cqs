// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @title Simple ERC20 Token
/// @notice A basic token implementation for testing
interface IERC20 {
    function totalSupply() external view returns (uint256);
    function balanceOf(address account) external view returns (uint256);
    function transfer(address to, uint256 amount) external returns (bool);
    event Transfer(address indexed from, address indexed to, uint256 value);
}

library SafeMath {
    function add(uint256 a, uint256 b) internal pure returns (uint256) {
        uint256 c = a + b;
        require(c >= a, "SafeMath: overflow");
        return c;
    }

    function sub(uint256 a, uint256 b) internal pure returns (uint256) {
        require(b <= a, "SafeMath: underflow");
        return a - b;
    }
}

struct TokenInfo {
    string name;
    string symbol;
    uint8 decimals;
}

enum Status { Active, Paused, Stopped }

error InsufficientBalance(uint256 available, uint256 required);

contract Token is IERC20 {
    using SafeMath for uint256;

    string public name;
    string public symbol;
    uint256 public totalSupply;
    mapping(address => uint256) private balances;

    event Mint(address indexed to, uint256 amount);

    modifier onlyPositive(uint256 amount) {
        require(amount > 0, "Amount must be positive");
        _;
    }

    constructor(string memory _name, string memory _symbol, uint256 _initialSupply) {
        name = _name;
        symbol = _symbol;
        totalSupply = _initialSupply;
        balances[msg.sender] = _initialSupply;
    }

    function balanceOf(address account) external view returns (uint256) {
        return balances[account];
    }

    function transfer(address to, uint256 amount) external onlyPositive(amount) returns (bool) {
        if (balances[msg.sender] < amount) {
            revert InsufficientBalance(balances[msg.sender], amount);
        }
        balances[msg.sender] = balances[msg.sender].sub(amount);
        balances[to] = balances[to].add(amount);
        emit Transfer(msg.sender, to, amount);
        return true;
    }

    function mint(address to, uint256 amount) external {
        totalSupply = totalSupply.add(amount);
        balances[to] = balances[to].add(amount);
        emit Mint(to, amount);
    }
}
