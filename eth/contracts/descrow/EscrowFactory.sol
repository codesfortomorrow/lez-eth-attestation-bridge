// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "@openzeppelin/contracts/access/Ownable.sol";
import "./EscrowAgreement.sol";

/// @title EscrowFactory
/// @notice Deploys EscrowAgreement contracts and holds the CURRENT global,
///         admin-tunable defaults (fees, timing windows, platform wallet). These
///         defaults are only ever used at the moment of deployment - each
///         EscrowAgreement snapshots them into its own immutable storage, so a
///         later change here never affects an agreement that already exists.
contract EscrowFactory is Ownable {
    address public platformWallet;

    uint256 public platformFeeBps = 200;               // 2%  - taken at deposit time
    uint256 public arbitratorFeeBps = 2000;             // 20% - taken only on arbitrated settlement
    uint256 public disputeAcceptWindow = 3 days;        // counterparty window to accept a dispute
    uint256 public arbitratorAcceptWindow = 3 days;     // arbitrator panel window to reach quorum
    uint256 public arbitrationResolutionWindow = 15 days; // max window for a proposal to reach 51%
    uint256 public defaultGracePeriod = 15 days;        // default seller-idle grace period
    uint256 public deploymentFee = 0;                   // native BNB fee to deploy (spam control)

    address[] public allAgreements;
    mapping(address => address[]) public agreementsByBuyer;
    mapping(address => address[]) public agreementsBySeller;

    event AgreementDeployed(
        address indexed agreement,
        address indexed initiator,
        address indexed buyer,
        address seller,
        address paymentToken,
        uint256 milestoneCount
    );
    event ConfigUpdated(string param, uint256 value);
    event PlatformWalletUpdated(address wallet);
    event DeploymentFeeRefunded(address indexed to, uint256 amount);

    constructor(address _platformWallet) Ownable(msg.sender) {
        require(_platformWallet != address(0), "bad platform wallet");
        platformWallet = _platformWallet;
    }

    /// @notice Deploys a new escrow agreement. The caller MUST be either `buyer` or
    ///         `seller` - a third party cannot create an agreement naming two other
    ///         addresses. The agreement stays inert (no funding possible) until the
    ///         OTHER named party calls `acknowledgeAgreement()` on it, so the caller
    ///         cannot unilaterally bind a counterparty to terms they never agreed to.
    ///
    /// @dev msg.value only needs to be >= deploymentFee. Only the exact deploymentFee
    ///      is forwarded to platformWallet; any excess the caller sent is refunded to
    ///      them at the end of this function. This was fixed after review - an
    ///      earlier version forwarded the ENTIRE msg.value to platformWallet
    ///      regardless of what deploymentFee actually was, so any overpayment (e.g.
    ///      a stale fee quote in a frontend, or the owner having lowered the fee
    ///      after a transaction was already signed) would have been swallowed
    ///      instead of returned.
    /// @param buyer the funding party
    /// @param seller the counterparty
    /// @param paymentToken the single ERC20 used for every milestone in this agreement
    /// @param arbitrators panel of 3, 5, 7, 9 or 11 distinct addresses
    /// @param milestoneInputs all milestones, fixed at deployment time (cannot be added later)
    function createEscrowAgreement(
        address buyer,
        address seller,
        address paymentToken,
        address[] calldata arbitrators,
        EscrowAgreement.MilestoneInput[] calldata milestoneInputs
    ) external payable returns (address agreement) {
        require(msg.sender == buyer || msg.sender == seller, "caller must be buyer or seller");
        require(msg.value >= deploymentFee, "insufficient deployment fee");

        // Cache the fee at the start of the call so the amount forwarded/refunded
        // below is internally consistent even though it's read again later, after
        // external calls (deploying EscrowAgreement doesn't call back into the
        // factory, but caching removes any doubt).
        uint256 fee = deploymentFee;

        EscrowAgreement.FactorySnapshot memory snapshot = EscrowAgreement.FactorySnapshot({
            platformWallet: platformWallet,
            platformFeeBps: platformFeeBps,
            arbitratorFeeBps: arbitratorFeeBps,
            disputeAcceptWindow: disputeAcceptWindow,
            arbitratorAcceptWindow: arbitratorAcceptWindow,
            arbitrationResolutionWindow: arbitrationResolutionWindow,
            defaultGracePeriod: defaultGracePeriod
        });

        EscrowAgreement deployed = new EscrowAgreement(
            msg.sender,
            buyer,
            seller,
            paymentToken,
            arbitrators,
            milestoneInputs,
            snapshot
        );
        agreement = address(deployed);

        // --- Effects: all state updates and the event happen before any external
        // value transfer below (checks-effects-interactions), so neither the
        // platform wallet nor the caller can re-enter mid-update. ---
        allAgreements.push(agreement);
        agreementsByBuyer[buyer].push(agreement);
        agreementsBySeller[seller].push(agreement);

        emit AgreementDeployed(agreement, msg.sender, buyer, seller, paymentToken, milestoneInputs.length);

        // --- Interactions: forward ONLY the deployment fee, refund any excess. ---
        if (fee > 0) {
            (bool ok, ) = platformWallet.call{value: fee}("");
            require(ok, "deployment fee transfer failed");
        }

        uint256 excess = msg.value - fee;
        if (excess > 0) {
            (bool refunded, ) = msg.sender.call{value: excess}("");
            require(refunded, "excess refund failed");
            emit DeploymentFeeRefunded(msg.sender, excess);
        }
    }

    // ---------------------------------------------------------------------
    // Admin configuration - affects only FUTURE deployments, never existing ones
    // ---------------------------------------------------------------------

    function setPlatformWallet(address wallet) external onlyOwner {
        require(wallet != address(0), "bad wallet");
        platformWallet = wallet;
        emit PlatformWalletUpdated(wallet);
    }

    function setPlatformFeeBps(uint256 bps) external onlyOwner {
        require(bps <= 1000, "max 10%");
        platformFeeBps = bps;
        emit ConfigUpdated("platformFeeBps", bps);
    }

    function setArbitratorFeeBps(uint256 bps) external onlyOwner {
        require(bps <= 5000, "max 50%");
        arbitratorFeeBps = bps;
        emit ConfigUpdated("arbitratorFeeBps", bps);
    }

    function setDisputeAcceptWindow(uint256 secs) external onlyOwner {
        require(secs > 0, "bad window");
        disputeAcceptWindow = secs;
        emit ConfigUpdated("disputeAcceptWindow", secs);
    }

    function setArbitratorAcceptWindow(uint256 secs) external onlyOwner {
        require(secs > 0, "bad window");
        arbitratorAcceptWindow = secs;
        emit ConfigUpdated("arbitratorAcceptWindow", secs);
    }

    function setArbitrationResolutionWindow(uint256 secs) external onlyOwner {
        require(secs > 0 && secs <= 15 days, "max 15 days");
        arbitrationResolutionWindow = secs;
        emit ConfigUpdated("arbitrationResolutionWindow", secs);
    }

    function setDefaultGracePeriod(uint256 secs) external onlyOwner {
        defaultGracePeriod = secs;
        emit ConfigUpdated("defaultGracePeriod", secs);
    }

    function setDeploymentFee(uint256 fee) external onlyOwner {
        deploymentFee = fee;
        emit ConfigUpdated("deploymentFee", fee);
    }

    // ---------------------------------------------------------------------
    // Views
    // ---------------------------------------------------------------------

    function allAgreementsCount() external view returns (uint256) { return allAgreements.length; }
    function getAgreementsByBuyer(address b) external view returns (address[] memory) { return agreementsByBuyer[b]; }
    function getAgreementsBySeller(address s) external view returns (address[] memory) { return agreementsBySeller[s]; }
}
