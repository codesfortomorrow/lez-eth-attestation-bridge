// SPDX-License-Identifier: MIT OR Apache-2.0
pragma solidity 0.8.24;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";

/// @title LezVault
/// @notice Ethereum side of the LEZ bridge (inbound leg). A deposit stores only a
///         commitment to the LEZ recipient. Release is not implemented yet.
contract LezVault is ReentrancyGuard {
    using SafeERC20 for IERC20;

    /// LEZ proves slot 0 (commitment) and slot 1 (token | amount) of each record.
    struct Deposit {
        bytes32 commitment;
        address token;   // address(0) = native ETH
        uint96 amount;
    }

    struct TokenConfig {
        bool registered;
        bool frozen;
        uint256 capPerWindow;   // max value deposited per window (rate limiter)
        uint256 windowStart;
        uint256 usedInWindow;
    }

    uint256 public constant CAP_WINDOW = 1 days;
    bytes32 public constant COMMITMENT_DOMAIN = keccak256("LEZ-BRIDGE-DEPOSIT-v1");

    address public immutable admin;
    address public immutable freezer;

    mapping(uint256 => Deposit) public deposits;               // slot 0
    uint256 public nextDepositId;                              // slot 1
    mapping(address => TokenConfig) public tokens;             // slot 2
    mapping(address => mapping(uint256 => bool)) public permittedAmount; // slot 3
    bool public globallyFrozen;                                // slot 4

    event Deposited(uint256 indexed id, address indexed token, uint256 amount, bytes32 commitment);
    event TokenRegistered(address indexed token, uint256 capPerWindow);
    event DenominationSet(address indexed token, uint256 amount, bool permitted);
    event Frozen(address indexed token, bool frozen);
    event GlobalFrozen(bool frozen);

    error NotAdmin();
    error NotFreezer();
    error UnsupportedToken();
    error InvalidAmount();
    error CapExceeded();
    error IsFrozen();
    error BadCommitment();
    error FeeOnTransferNotSupported();

    constructor(address _admin, address _freezer) {
        admin = _admin;
        freezer = _freezer;
    }

    function registerToken(address token, uint256 capPerWindow, uint256[] calldata denominations) external {
        if (msg.sender != admin) revert NotAdmin();
        TokenConfig storage t = tokens[token];
        t.registered = true;
        t.capPerWindow = capPerWindow;
        for (uint256 i = 0; i < denominations.length; i++) {
            permittedAmount[token][denominations[i]] = true;
            emit DenominationSet(token, denominations[i], true);
        }
        emit TokenRegistered(token, capPerWindow);
    }

    function setFrozen(address token, bool frozen) external {
        if (msg.sender != freezer) revert NotFreezer();
        tokens[token].frozen = frozen;
        emit Frozen(token, frozen);
    }

    function setGlobalFrozen(bool frozen) external {
        if (msg.sender != freezer) revert NotFreezer();
        globallyFrozen = frozen;
        emit GlobalFrozen(frozen);
    }

    /// @param token address(0) for native ETH
    /// @param commitment keccak256(COMMITMENT_DOMAIN, lezChainId, secret, lezRecipient)
    function deposit(address token, uint256 amount, bytes32 commitment) external payable nonReentrant returns (uint256 id) {
        if (commitment == bytes32(0)) revert BadCommitment();
        TokenConfig storage t = tokens[token];
        if (!t.registered) revert UnsupportedToken();
        if (globallyFrozen || t.frozen) revert IsFrozen();
        if (!permittedAmount[token][amount] || amount > type(uint96).max) revert InvalidAmount();

        if (block.timestamp >= t.windowStart + CAP_WINDOW) {
            t.windowStart = block.timestamp;
            t.usedInWindow = 0;
        }
        if (t.usedInWindow + amount > t.capPerWindow) revert CapExceeded();
        t.usedInWindow += amount;

        if (token == address(0)) {
            if (msg.value != amount) revert InvalidAmount();
        } else {
            if (msg.value != 0) revert InvalidAmount();
            uint256 before = IERC20(token).balanceOf(address(this));
            // SafeERC20 covers tokens that return no bool, like USDT
            IERC20(token).safeTransferFrom(msg.sender, address(this), amount);
            // amounts must arrive exactly, so fee-on-transfer tokens are rejected
            if (IERC20(token).balanceOf(address(this)) - before != amount) revert FeeOnTransferNotSupported();
        }

        id = nextDepositId++;
        deposits[id] = Deposit({commitment: commitment, token: token, amount: uint96(amount)});
        emit Deposited(id, token, amount, commitment);
    }

    /// For tests. Real clients compute this off-chain so the recipient stays out of calldata.
    function computeCommitment(uint256 lezChainId, bytes32 secret, bytes32 lezRecipient) external pure returns (bytes32) {
        return keccak256(abi.encode(COMMITMENT_DOMAIN, lezChainId, secret, lezRecipient));
    }
}
