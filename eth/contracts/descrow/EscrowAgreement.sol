// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/utils/ReentrancyGuard.sol";

/// @title EscrowAgreement
/// @notice A single milestone-based escrow agreement between a buyer and a seller,
///         with an arbitrator panel for dispute resolution. Deployed exclusively by
///         EscrowFactory.
///
/// @dev v2 changes from the original MilestoneEscrow design, per developer review:
///      1. All factory parameters (fees, timing windows, platform wallet) are
///         SNAPSHOTTED into immutable storage at construction time instead of being
///         read live from the factory. Every deployed agreement is therefore a
///         permanent, self-contained historical record of the terms it was created
///         under - a later change to the factory's defaults cannot retroactively
///         change what any existing agreement charges or enforces.
///      2. Contract creation now takes explicit `buyer` and `seller` addresses (the
///         caller must be one of the two), and the agreement stays inert - no
///         funding possible - until the OTHER named party explicitly calls
///         `acknowledgeAgreement()`. This closes two problems at once: a third
///         party can no longer spin up a contract naming two strangers, and the
///         seller (or buyer) can no longer be bound to fee/timing terms they never
///         actively agreed to.
///      3. Renamed from `MilestoneEscrow`; the payment token is called `paymentToken`
///         rather than `stablecoin`, since nothing here actually enforces the token
///         is pegged - it's just whatever ERC20 the parties agree to use. The
///         constructor also runs a best-effort on-chain sanity check that the
///         address behaves like an ERC20 token.
///      4. Every inbound transfer now measures the ACTUAL balance delta rather than
///         trusting the nominal amount, so fee-on-transfer and other
///         non-standard-transfer ERC20s no longer break the escrow accounting.
contract EscrowAgreement is ReentrancyGuard {
    using SafeERC20 for IERC20;

    // ---------------------------------------------------------------------
    // Types
    // ---------------------------------------------------------------------

    enum MilestoneStatus {
        Pending,             // 0  created at deploy time, not yet funded
        Funded,              // 1  escrowed, milestone "active"
        CompletionRequested, // 2  seller marked done, review period running
        Disputed,            // 3  dispute raised, awaiting counterparty accept/timeout
        ArbitrationPending,  // 4  escalated, awaiting arbitrator quorum to accept case
        ArbitrationActive,   // 5  quorum reached, proposals/voting live
        Settled,             // 6  paid out (normal flow or arbitrated)
        Refunded,            // 7  fully refunded to buyer (missed deadline)
        Locked               // 8  no quorum / no resolution reached in time - frozen
    }

    enum ContractStatus { AwaitingAcknowledgement, Created, Active, InProcess, Completed }

    struct MilestoneInput {
        uint256 amount;              // nominal deposit amount (token's own decimals)
        string termsHash;            // content-integrity reference for this milestone's terms &
                                      // conditions document. Storage-agnostic by design - it can
                                      // hold an IPFS CID, a sha-256 hex digest of a file kept in
                                      // conventional (web2) storage, or any other scheme, as long
                                      // as off-chain code agrees on how to interpret it. See the
                                      // README for the recommended prefix convention.
        uint16 defaultSellerBps;     // default split on normal settlement, e.g. 10000 = 100% to seller
        uint256 reviewPeriod;        // seconds; window after completion request before auto-pay
        uint256 completionDeadline;  // seconds from funding; seller must request completion by then
        uint256 gracePeriod;         // seconds; extra buffer after deadline before buyer can reclaim (0 = use snapshotted default)
    }

    struct Milestone {
        uint256 amount;               // nominal amount the buyer is expected to send
        uint256 escrowedAmount;       // ACTUAL amount held after platform fee, based on real transfer-in
        string termsHash;             // see MilestoneInput.termsHash - copied through as-is
        uint16 defaultSellerBps;
        uint16 currentSellerBps;      // mutable; changed only by an executed arbitration proposal
        uint256 reviewPeriod;
        uint256 completionDeadline;
        uint256 gracePeriod;
        uint256 fundedAt;
        uint256 completionRequestedAt;
        MilestoneStatus status;
    }

    struct Dispute {
        address raisedBy;
        uint256 raisedAt;
        uint16 proposedSellerBps;          // informational: what the raiser is asking for
        bool counterpartyAccepted;
        uint256 arbitrationPendingStart;
        uint256 arbitrationActiveStart;
        uint256 acceptedCount;
        uint256 proposalCount;
        mapping(address => bool) arbitratorAccepted;
        mapping(address => bool) hasVoted;   // has this arbitrator voted on ANY proposal in this dispute
        address[] voters;                    // arbitrators who actually cast a vote - only they earn the fee
    }

    struct Proposal {
        address proposer;
        uint16 newSellerBps;
        uint256 votes;
        bool executed;
        mapping(address => bool) votedOnThis;
    }

    // ---------------------------------------------------------------------
    // Immutable deal parties & token
    // ---------------------------------------------------------------------

    address public immutable factory;       // provenance only - never called back into for config
    address public immutable buyer;
    address public immutable seller;
    address public immutable initiator;     // whichever of buyer/seller actually deployed this
    IERC20 public immutable paymentToken;

    address[] public arbitrators;
    mapping(address => bool) public isArbitrator;

    // ---------------------------------------------------------------------
    // Snapshotted factory parameters (immutable - fixed forever at deployment)
    // ---------------------------------------------------------------------

    address public immutable platformWallet;
    uint256 public immutable platformFeeBps;
    uint256 public immutable arbitratorFeeBps;
    uint256 public immutable disputeAcceptWindow;
    uint256 public immutable arbitratorAcceptWindow;
    uint256 public immutable arbitrationResolutionWindow;
    uint256 public immutable defaultGracePeriod;
    uint256 public immutable createdAt;

    // ---------------------------------------------------------------------
    // Two-sided acknowledgement handshake
    // ---------------------------------------------------------------------

    bool public buyerAcknowledged;
    bool public sellerAcknowledged;
    bool public dealActive;

    // ---------------------------------------------------------------------
    // Milestones & disputes
    // ---------------------------------------------------------------------

    Milestone[] public milestones;
    mapping(uint256 => Dispute) private disputes;                       // milestoneId => Dispute
    mapping(uint256 => mapping(uint256 => Proposal)) private proposals; // milestoneId => proposalId => Proposal

    // ---------------------------------------------------------------------
    // Events
    // ---------------------------------------------------------------------

    event AgreementInitiated(address indexed initiator, address indexed buyer, address indexed seller);
    event AgreementAcknowledged(address indexed party);
    event AgreementActivated();

    event MilestoneFunded(uint256 indexed id, uint256 nominalAmount, uint256 actualReceived, uint256 escrowedAmount);
    event CompletionRequested(uint256 indexed id, uint256 reviewEndsAt);
    event MilestoneApproved(uint256 indexed id);
    event MilestoneSettled(uint256 indexed id, uint256 sellerAmount, uint256 buyerAmount, uint256 arbitratorFee);
    event MilestoneRefunded(uint256 indexed id, uint256 amount);
    event DisputeRaised(uint256 indexed id, address indexed raisedBy, uint16 proposedSellerBps);
    event DisputeAccepted(uint256 indexed id, address indexed acceptedBy);
    event ArbitrationEscalated(uint256 indexed id);
    event ArbitratorAcceptedCase(uint256 indexed id, address indexed arbitrator);
    event ArbitrationActivated(uint256 indexed id);
    event ProposalCreated(uint256 indexed id, uint256 indexed proposalId, address proposer, uint16 newSellerBps);
    event ProposalVoted(uint256 indexed id, uint256 indexed proposalId, address arbitrator);
    event ProposalExecuted(uint256 indexed id, uint256 indexed proposalId, uint16 newSellerBps);
    event MilestoneLocked(uint256 indexed id, string reason);

    // ---------------------------------------------------------------------
    // Modifiers
    // ---------------------------------------------------------------------

    modifier onlyBuyer() { require(msg.sender == buyer, "not buyer"); _; }
    modifier onlySeller() { require(msg.sender == seller, "not seller"); _; }
    modifier onlyParty() { require(msg.sender == buyer || msg.sender == seller, "not party"); _; }
    modifier onlyArbitrator() { require(isArbitrator[msg.sender], "not arbitrator"); _; }
    modifier validMilestone(uint256 id) { require(id < milestones.length, "bad milestone id"); _; }
    modifier onlyActiveDeal() { require(dealActive, "deal not acknowledged by both parties yet"); _; }

    // ---------------------------------------------------------------------
    // Constructor
    // ---------------------------------------------------------------------

    struct FactorySnapshot {
        address platformWallet;
        uint256 platformFeeBps;
        uint256 arbitratorFeeBps;
        uint256 disputeAcceptWindow;
        uint256 arbitratorAcceptWindow;
        uint256 arbitrationResolutionWindow;
        uint256 defaultGracePeriod;
    }

    constructor(
        address _initiator,
        address _buyer,
        address _seller,
        address _paymentToken,
        address[] memory _arbitrators,
        MilestoneInput[] memory _milestones,
        FactorySnapshot memory _snapshot
    ) {
        require(_buyer != address(0) && _seller != address(0) && _buyer != _seller, "bad parties");
        require(_initiator == _buyer || _initiator == _seller, "initiator must be a party");
        require(_paymentToken != address(0), "bad token");
        require(
            _arbitrators.length == 3 || _arbitrators.length == 5 ||
            _arbitrators.length == 7 || _arbitrators.length == 9 ||
            _arbitrators.length == 11,
            "panel must be 3/5/7/9/11"
        );
        require(_milestones.length > 0 && _milestones.length <= 20, "bad milestone count");
        require(_snapshot.platformWallet != address(0), "bad platform wallet");

        _validateLooksLikeERC20(_paymentToken);

        factory = msg.sender;
        initiator = _initiator;
        buyer = _buyer;
        seller = _seller;
        paymentToken = IERC20(_paymentToken);
        createdAt = block.timestamp;

        platformWallet = _snapshot.platformWallet;
        platformFeeBps = _snapshot.platformFeeBps;
        arbitratorFeeBps = _snapshot.arbitratorFeeBps;
        disputeAcceptWindow = _snapshot.disputeAcceptWindow;
        arbitratorAcceptWindow = _snapshot.arbitratorAcceptWindow;
        arbitrationResolutionWindow = _snapshot.arbitrationResolutionWindow;
        defaultGracePeriod = _snapshot.defaultGracePeriod;

        for (uint256 i = 0; i < _arbitrators.length; i++) {
            address a = _arbitrators[i];
            require(a != address(0) && a != _buyer && a != _seller, "bad arbitrator address");
            require(!isArbitrator[a], "duplicate arbitrator");
            isArbitrator[a] = true;
            arbitrators.push(a);
        }

        for (uint256 i = 0; i < _milestones.length; i++) {
            MilestoneInput memory mi = _milestones[i];
            require(mi.amount > 0, "bad milestone amount");
            require(mi.defaultSellerBps <= 10000, "bad bps");
            require(bytes(mi.termsHash).length > 0, "missing terms hash");
            require(mi.reviewPeriod > 0, "bad review period");
            require(mi.completionDeadline > 0, "bad completion deadline");

            milestones.push(Milestone({
                amount: mi.amount,
                escrowedAmount: 0,
                termsHash: mi.termsHash,
                defaultSellerBps: mi.defaultSellerBps,
                currentSellerBps: mi.defaultSellerBps,
                reviewPeriod: mi.reviewPeriod,
                completionDeadline: mi.completionDeadline,
                gracePeriod: mi.gracePeriod,
                fundedAt: 0,
                completionRequestedAt: 0,
                status: MilestoneStatus.Pending
            }));
        }

        // The deploying party is implicitly acknowledging the terms they just set;
        // the other party must explicitly opt in before anything can be funded.
        if (_initiator == _buyer) {
            buyerAcknowledged = true;
        } else {
            sellerAcknowledged = true;
        }

        emit AgreementInitiated(_initiator, _buyer, _seller);
    }

    /// @dev Best-effort sanity check that `token` behaves like an ERC20. This cannot
    ///      guarantee full compliance (a contract can implement these selectors and
    ///      still misbehave), but it reliably rejects EOAs, non-contracts, and
    ///      obviously-wrong addresses at deployment time rather than failing later
    ///      inside a fund transfer.
    function _validateLooksLikeERC20(address token) internal view {
        require(token.code.length > 0, "payment token is not a contract");

        (bool okSupply, bytes memory dataSupply) =
            token.staticcall(abi.encodeWithSignature("totalSupply()"));
        require(okSupply && dataSupply.length >= 32, "token missing totalSupply()");

        (bool okBalance, bytes memory dataBalance) =
            token.staticcall(abi.encodeWithSignature("balanceOf(address)", address(this)));
        require(okBalance && dataBalance.length >= 32, "token missing balanceOf()");
    }

    // ---------------------------------------------------------------------
    // Acknowledgement handshake
    // ---------------------------------------------------------------------

    /// @notice The party who did NOT deploy this agreement must call this to accept
    ///         the snapshotted terms (fees, windows, milestones, arbitrator panel)
    ///         before any milestone can be funded.
    function acknowledgeAgreement() external onlyParty {
        if (msg.sender == buyer) {
            require(!buyerAcknowledged, "already acknowledged");
            buyerAcknowledged = true;
        } else {
            require(!sellerAcknowledged, "already acknowledged");
            sellerAcknowledged = true;
        }
        emit AgreementAcknowledged(msg.sender);

        if (buyerAcknowledged && sellerAcknowledged) {
            dealActive = true;
            emit AgreementActivated();
        }
    }

    // ---------------------------------------------------------------------
    // Funding (fee-on-transfer safe: uses actual balance delta, not nominal amount)
    // ---------------------------------------------------------------------

    /// @notice Buyer deposits a milestone. The contract measures what it actually
    ///         received rather than trusting `amount`, so fee-on-transfer or other
    ///         non-standard ERC20s can't desync the internal accounting.
    function fundMilestone(uint256 id) external nonReentrant onlyBuyer onlyActiveDeal validMilestone(id) {
        Milestone storage m = milestones[id];
        require(m.status == MilestoneStatus.Pending, "not fundable");

        uint256 balBefore = paymentToken.balanceOf(address(this));
        paymentToken.safeTransferFrom(msg.sender, address(this), m.amount);
        uint256 received = paymentToken.balanceOf(address(this)) - balBefore;
        require(received > 0, "no funds actually received");

        uint256 fee = (received * platformFeeBps) / 10000;
        if (fee > 0) {
            paymentToken.safeTransfer(platformWallet, fee);
        }

        m.escrowedAmount = received - fee;
        m.fundedAt = block.timestamp;
        m.status = MilestoneStatus.Funded;

        emit MilestoneFunded(id, m.amount, received, m.escrowedAmount);
    }

    // ---------------------------------------------------------------------
    // Completion / normal settlement
    // ---------------------------------------------------------------------

    function requestCompletion(uint256 id) external onlySeller validMilestone(id) {
        Milestone storage m = milestones[id];
        require(m.status == MilestoneStatus.Funded, "not funded");
        m.completionRequestedAt = block.timestamp;
        m.status = MilestoneStatus.CompletionRequested;
        emit CompletionRequested(id, block.timestamp + m.reviewPeriod);
    }

    /// @notice Buyer can approve early - settles immediately.
    function approveCompletion(uint256 id) external onlyBuyer validMilestone(id) nonReentrant {
        Milestone storage m = milestones[id];
        require(m.status == MilestoneStatus.CompletionRequested, "not requested");
        emit MilestoneApproved(id);
        _settleNormal(id);
    }

    /// @notice Anyone can trigger auto-pay once the review period has elapsed with no dispute.
    function claimAfterReview(uint256 id) external validMilestone(id) nonReentrant {
        Milestone storage m = milestones[id];
        require(m.status == MilestoneStatus.CompletionRequested, "not requested");
        require(block.timestamp >= m.completionRequestedAt + m.reviewPeriod, "review period not over");
        _settleNormal(id);
    }

    function _settleNormal(uint256 id) internal {
        Milestone storage m = milestones[id];
        uint256 escrowed = m.escrowedAmount;
        uint256 sellerAmount = (escrowed * m.currentSellerBps) / 10000;
        uint256 buyerAmount = escrowed - sellerAmount;

        m.status = MilestoneStatus.Settled;

        if (sellerAmount > 0) paymentToken.safeTransfer(seller, sellerAmount);
        if (buyerAmount > 0) paymentToken.safeTransfer(buyer, buyerAmount);

        emit MilestoneSettled(id, sellerAmount, buyerAmount, 0);
    }

    // ---------------------------------------------------------------------
    // Seller-idle / missed-deadline refund
    // ---------------------------------------------------------------------

    /// @notice If the seller never requests completion within deadline + grace,
    ///         anyone can trigger a full refund of the escrowed balance to the buyer.
    ///         Note: the platform fee taken at deposit time is NOT refunded.
    function reclaimExpiredMilestone(uint256 id) external validMilestone(id) nonReentrant {
        Milestone storage m = milestones[id];
        require(m.status == MilestoneStatus.Funded, "not in funded state");

        uint256 grace = m.gracePeriod > 0 ? m.gracePeriod : defaultGracePeriod;
        require(block.timestamp >= m.fundedAt + m.completionDeadline + grace, "deadline not yet passed");

        uint256 refund = m.escrowedAmount;
        m.status = MilestoneStatus.Refunded;
        paymentToken.safeTransfer(buyer, refund);

        emit MilestoneRefunded(id, refund);
    }

    // ---------------------------------------------------------------------
    // Disputes
    // ---------------------------------------------------------------------

    /// @notice Either party can raise a dispute while a milestone is funded or
    ///         pending buyer approval. proposedSellerBps is informational context
    ///         for what the raiser believes the fair split should be.
    function raiseDispute(uint256 id, uint16 proposedSellerBps) external onlyParty validMilestone(id) {
        Milestone storage m = milestones[id];
        require(
            m.status == MilestoneStatus.Funded || m.status == MilestoneStatus.CompletionRequested,
            "not disputable in current state"
        );
        require(proposedSellerBps <= 10000, "bad bps");

        Dispute storage d = disputes[id];
        d.raisedBy = msg.sender;
        d.raisedAt = block.timestamp;
        d.proposedSellerBps = proposedSellerBps;

        m.status = MilestoneStatus.Disputed;
        emit DisputeRaised(id, msg.sender, proposedSellerBps);
    }

    /// @notice The counterparty (not the raiser) acknowledges the dispute. This does
    ///         NOT resolve it directly - it only fast-tracks escalation to arbitration
    ///         instead of waiting out the full accept window.
    function acceptDispute(uint256 id) external onlyParty validMilestone(id) {
        Milestone storage m = milestones[id];
        Dispute storage d = disputes[id];
        require(m.status == MilestoneStatus.Disputed, "no active dispute");
        require(msg.sender != d.raisedBy, "raiser cannot accept own dispute");
        require(!d.counterpartyAccepted, "already accepted");

        d.counterpartyAccepted = true;
        emit DisputeAccepted(id, msg.sender);
        _escalateToArbitration(id);
    }

    /// @notice Anyone can escalate once the accept window has elapsed without a response.
    function escalateToArbitration(uint256 id) external validMilestone(id) {
        Milestone storage m = milestones[id];
        Dispute storage d = disputes[id];
        require(m.status == MilestoneStatus.Disputed, "no active dispute");
        require(!d.counterpartyAccepted, "already escalated");

        require(block.timestamp >= d.raisedAt + disputeAcceptWindow, "accept window still open");
        _escalateToArbitration(id);
    }

    function _escalateToArbitration(uint256 id) internal {
        Milestone storage m = milestones[id];
        Dispute storage d = disputes[id];
        m.status = MilestoneStatus.ArbitrationPending;
        d.arbitrationPendingStart = block.timestamp;
        emit ArbitrationEscalated(id);
    }

    // ---------------------------------------------------------------------
    // Arbitration
    // ---------------------------------------------------------------------

    /// @notice A panel arbitrator accepts the case. Once >=51% of the WHOLE panel
    ///         has accepted, arbitration activates and proposals/voting can begin.
    function acceptArbitration(uint256 id) external onlyArbitrator validMilestone(id) {
        Milestone storage m = milestones[id];
        Dispute storage d = disputes[id];
        require(m.status == MilestoneStatus.ArbitrationPending, "not pending arbitration");

        require(block.timestamp <= d.arbitrationPendingStart + arbitratorAcceptWindow, "arbitrator accept window closed");
        require(!d.arbitratorAccepted[msg.sender], "already accepted");

        d.arbitratorAccepted[msg.sender] = true;
        d.acceptedCount += 1;
        emit ArbitratorAcceptedCase(id, msg.sender);

        if (_meetsPanelMajority(d.acceptedCount)) {
            m.status = MilestoneStatus.ArbitrationActive;
            d.arbitrationActiveStart = block.timestamp;
            emit ArbitrationActivated(id);
        }
    }

    /// @notice If arbitrator quorum is never reached in time, anyone can lock the
    ///         milestone. There is deliberately no admin override - funds stay
    ///         locked until the parties resolve the situation off-chain.
    function lockIfNoQuorum(uint256 id) external validMilestone(id) {
        Milestone storage m = milestones[id];
        Dispute storage d = disputes[id];
        require(m.status == MilestoneStatus.ArbitrationPending, "not pending arbitration");

        require(block.timestamp > d.arbitrationPendingStart + arbitratorAcceptWindow, "accept window still open");

        m.status = MilestoneStatus.Locked;
        emit MilestoneLocked(id, "no arbitrator quorum reached");
    }

    /// @notice Any arbitrator who has accepted the case can propose a new seller/buyer split.
    function createProposal(uint256 id, uint16 newSellerBps) external onlyArbitrator validMilestone(id) returns (uint256 proposalId) {
        Milestone storage m = milestones[id];
        Dispute storage d = disputes[id];
        require(m.status == MilestoneStatus.ArbitrationActive, "arbitration not active");
        require(d.arbitratorAccepted[msg.sender], "must accept case first");
        require(newSellerBps <= 10000, "bad bps");

        require(block.timestamp <= d.arbitrationActiveStart + arbitrationResolutionWindow, "resolution window closed");

        proposalId = d.proposalCount++;
        Proposal storage p = proposals[id][proposalId];
        p.proposer = msg.sender;
        p.newSellerBps = newSellerBps;

        emit ProposalCreated(id, proposalId, msg.sender, newSellerBps);
    }

    /// @notice Arbitrators who accepted the case vote on a proposal. Execution requires
    ///         >=51% of the WHOLE panel (not just those who accepted). Only arbitrators
    ///         who actually cast a vote earn a share of the arbitrator fee.
    function voteProposal(uint256 id, uint256 proposalId) external onlyArbitrator validMilestone(id) nonReentrant {
        Milestone storage m = milestones[id];
        Dispute storage d = disputes[id];
        require(m.status == MilestoneStatus.ArbitrationActive, "arbitration not active");
        require(d.arbitratorAccepted[msg.sender], "must accept case first");

        require(block.timestamp <= d.arbitrationActiveStart + arbitrationResolutionWindow, "resolution window closed");

        Proposal storage p = proposals[id][proposalId];
        require(p.proposer != address(0), "no such proposal");
        require(!p.executed, "already executed");
        require(!p.votedOnThis[msg.sender], "already voted on this proposal");

        p.votedOnThis[msg.sender] = true;
        p.votes += 1;

        if (!d.hasVoted[msg.sender]) {
            d.hasVoted[msg.sender] = true;
            d.voters.push(msg.sender);
        }

        emit ProposalVoted(id, proposalId, msg.sender);

        if (_meetsPanelMajority(p.votes)) {
            _executeProposal(id, proposalId);
        }
    }

    function _executeProposal(uint256 id, uint256 proposalId) internal {
        Milestone storage m = milestones[id];
        Dispute storage d = disputes[id];
        Proposal storage p = proposals[id][proposalId];

        p.executed = true;
        m.currentSellerBps = p.newSellerBps;
        m.status = MilestoneStatus.Settled;

        emit ProposalExecuted(id, proposalId, p.newSellerBps);
        _settleArbitrated(id, d.voters);
    }

    function _settleArbitrated(uint256 id, address[] memory voters) internal {
        Milestone storage m = milestones[id];
        uint256 escrowed = m.escrowedAmount;

        uint256 arbitratorFee = (escrowed * arbitratorFeeBps) / 10000;
        uint256 remaining = escrowed - arbitratorFee;

        uint256 sellerAmount = (remaining * m.currentSellerBps) / 10000;
        uint256 buyerAmount = remaining - sellerAmount;

        if (sellerAmount > 0) paymentToken.safeTransfer(seller, sellerAmount);
        if (buyerAmount > 0) paymentToken.safeTransfer(buyer, buyerAmount);

        uint256 voterCount = voters.length;
        if (arbitratorFee > 0 && voterCount > 0) {
            uint256 share = arbitratorFee / voterCount;
            uint256 dust = arbitratorFee - (share * voterCount);
            for (uint256 i = 0; i < voterCount; i++) {
                uint256 amt = share;
                if (i == voterCount - 1) amt += dust; // remainder to avoid stuck dust
                if (amt > 0) paymentToken.safeTransfer(voters[i], amt);
            }
        }

        emit MilestoneSettled(id, sellerAmount, buyerAmount, arbitratorFee);
    }

    /// @notice If no proposal reaches panel majority within the resolution window,
    ///         anyone can lock the milestone. No admin override, by design.
    function lockIfNoResolution(uint256 id) external validMilestone(id) {
        Milestone storage m = milestones[id];
        Dispute storage d = disputes[id];
        require(m.status == MilestoneStatus.ArbitrationActive, "arbitration not active");

        require(block.timestamp > d.arbitrationActiveStart + arbitrationResolutionWindow, "resolution window still open");

        m.status = MilestoneStatus.Locked;
        emit MilestoneLocked(id, "no resolution reached in time");
    }

    /// @dev true once `count` reaches a strict majority (>=51%) of the whole panel.
    function _meetsPanelMajority(uint256 count) internal view returns (bool) {
        return count * 2 >= arbitrators.length + 1;
    }

    // ---------------------------------------------------------------------
    // Views
    // ---------------------------------------------------------------------

    function milestonesCount() external view returns (uint256) { return milestones.length; }
    function arbitratorsList() external view returns (address[] memory) { return arbitrators; }
    function panelSize() external view returns (uint256) { return arbitrators.length; }

    function disputeInfo(uint256 id) external view validMilestone(id) returns (
        address raisedBy,
        uint256 raisedAt,
        uint16 proposedSellerBps,
        bool counterpartyAccepted,
        uint256 arbitrationPendingStart,
        uint256 arbitrationActiveStart,
        uint256 acceptedCount,
        uint256 proposalCount
    ) {
        Dispute storage d = disputes[id];
        return (
            d.raisedBy,
            d.raisedAt,
            d.proposedSellerBps,
            d.counterpartyAccepted,
            d.arbitrationPendingStart,
            d.arbitrationActiveStart,
            d.acceptedCount,
            d.proposalCount
        );
    }

    function proposalInfo(uint256 id, uint256 proposalId) external view returns (
        address proposer,
        uint16 newSellerBps,
        uint256 votes,
        bool executed
    ) {
        Proposal storage p = proposals[id][proposalId];
        return (p.proposer, p.newSellerBps, p.votes, p.executed);
    }

    /// @notice Contract-wide status derived from acknowledgement + all milestone statuses.
    function contractStatus() external view returns (ContractStatus) {
        if (!dealActive) return ContractStatus.AwaitingAcknowledgement;

        bool anyFunded = false;
        bool anyTrouble = false;
        bool allFinal = true;

        uint256 len = milestones.length;
        for (uint256 i = 0; i < len; i++) {
            MilestoneStatus s = milestones[i].status;
            if (s != MilestoneStatus.Pending) anyFunded = true;

            if (
                s == MilestoneStatus.Disputed ||
                s == MilestoneStatus.ArbitrationPending ||
                s == MilestoneStatus.ArbitrationActive ||
                s == MilestoneStatus.Locked
            ) {
                anyTrouble = true;
            }

            if (s != MilestoneStatus.Settled && s != MilestoneStatus.Refunded) {
                allFinal = false;
            }
        }

        if (anyTrouble) return ContractStatus.InProcess;
        if (allFinal && anyFunded) return ContractStatus.Completed;
        if (anyFunded) return ContractStatus.Active;
        return ContractStatus.Created;
    }
}
