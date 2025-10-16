use alloy::sol;

sol! {
    #[derive(Debug)]
    #[sol(rpc)]
    interface IVeaInboxArbToEth {
        event MessageSent(bytes _nodeData);
        event SnapshotSaved(bytes32 _snapshot, uint256 _epoch, uint64 _count);
        event SnapshotSent(uint256 indexed _epochSent, bytes32 _ticketId);

        function sendMessage(address to, bytes calldata data) external returns (uint64);
        function saveSnapshot() external;
        function count() external view returns (uint64);
        function epochPeriod() external view returns (uint256);
        function snapshots(uint256 epoch) external view returns (bytes32);
        function epochNow() external view returns (uint256);
        function epochFinalized() external view returns (uint256);
        function sendSnapshot(uint256 _epoch, Claim memory _claim) external;

        struct Claim {
            bytes32 stateRoot;
            address claimer;
            uint32 timestampClaimed;
            uint32 timestampVerification;
            uint32 blocknumberVerification;
            Party honest;
            address challenger;
        }

        enum Party {
            None,
            Claimer,
            Challenger
        }
    }

    #[derive(Debug)]
    #[sol(rpc)]
    interface IVeaOutboxArbToEth {
        struct Claim {
            bytes32 stateRoot;
            address claimer;
            uint32 timestampClaimed;
            uint32 timestampVerification;
            uint32 blocknumberVerification;
            Party honest;
            address challenger;
        }

        enum Party {
            None,
            Claimer,
            Challenger
        }

        event Claimed(address indexed _claimer, uint256 indexed _epoch, bytes32 indexed _stateRoot);
        event Challenged(uint256 indexed _epoch, address indexed _challenger);
        event MessageRelayed(uint64 _msgId);
        event VerificationStarted(uint256 indexed _epoch);
        event Verified(uint256 indexed _epoch);

        function claim(uint256 _epoch, bytes32 _stateRoot) external payable;
        function challenge(uint256 _epoch, Claim memory _claim, address _withdrawalAddress) external payable;
        function startVerification(uint256 _epoch, Claim memory _claim) external;
        function verifySnapshot(uint256 _epoch, Claim memory _claim) external;
        function withdrawClaimDeposit(uint256 _epoch, Claim memory _claim) external;
        function deposit() external view returns (uint256);
        function epochPeriod() external view returns (uint256);
        function claimHashes(uint256 epoch) external view returns (bytes32);
        function hashClaim(Claim memory _claim) external pure returns (bytes32);
    }

    // ============================================
    // Arbitrum to Gnosis contracts
    // ============================================

    #[derive(Debug)]
    #[sol(rpc)]
    interface IVeaInboxArbToGnosis {
        event MessageSent(bytes _nodeData);
        event SnapshotSaved(bytes32 _snapshot, uint256 _epoch, uint64 _count);
        event SnapshotSent(uint256 indexed _epochSent, bytes32 _ticketId);

        function sendMessage(address to, bytes calldata data) external returns (uint64);
        function saveSnapshot() external;
        function count() external view returns (uint64);
        function epochPeriod() external view returns (uint256);
        function snapshots(uint256 epoch) external view returns (bytes32);
        function epochNow() external view returns (uint256);
        function epochFinalized() external view returns (uint256);
        function sendSnapshot(uint256 _epoch, uint256 _gasLimit, Claim memory _claim) external;

        struct Claim {
            bytes32 stateRoot;
            address claimer;
            uint32 timestampClaimed;
            uint32 timestampVerification;
            uint32 blocknumberVerification;
            Party honest;
            address challenger;
        }

        enum Party {
            None,
            Claimer,
            Challenger
        }
    }

    #[derive(Debug)]
    #[sol(rpc)]
    interface IVeaOutboxArbToGnosis {
        struct Claim {
            bytes32 stateRoot;
            address claimer;
            uint32 timestampClaimed;
            uint32 timestampVerification;
            uint32 blocknumberVerification;
            Party honest;
            address challenger;
        }

        enum Party {
            None,
            Claimer,
            Challenger
        }

        event Claimed(address indexed _claimer, uint256 indexed _epoch, bytes32 indexed _stateRoot);
        event Challenged(uint256 indexed _epoch, address indexed _challenger);
        event MessageRelayed(uint64 _msgId);
        event VerificationStarted(uint256 indexed _epoch);
        event Verified(uint256 indexed _epoch);
        event SequencerDelayLimitUpdateReceived(uint256 _newSequencerDelayLimit);

        function claim(uint256 _epoch, bytes32 _stateRoot) external payable;
        function challenge(uint256 _epoch, Claim memory _claim) external payable;
        function startVerification(uint256 _epoch, Claim memory _claim) external;
        function verifySnapshot(uint256 _epoch, Claim memory _claim) external;
        function withdrawClaimDeposit(uint256 _epoch, Claim memory _claim) external;
        function deposit() external view returns (uint256);
        function epochPeriod() external view returns (uint256);
        function claimHashes(uint256 epoch) external view returns (bytes32);
        function hashClaim(Claim memory _claim) external pure returns (bytes32);
        function sequencerDelayLimit() external view returns (uint256);
    }

    #[derive(Debug)]
    #[sol(rpc)]
    interface IRouterArbToGnosis {
        event Routed(uint256 indexed _epoch, bytes32 _ticketID);
        event SequencerDelayLimitSent(bytes32 _ticketID);
        event SequencerDelayLimitUpdated(uint256 _newSequencerDelayLimit);
        event SequencerDelayLimitDecreaseRequested(uint256 _requestedSequencerDelayLimit);

        function route(uint256 _epoch, bytes32 _stateroot, Claim memory _claim) external;
        function updateSequencerDelayLimit() external;
        function sendSequencerDelayLimit() external;
        function executeSequencerDelayLimitDecreaseRequest() external;

        struct Claim {
            bytes32 stateRoot;
            address claimer;
            uint32 timestampClaimed;
            uint32 timestampVerification;
            uint32 blocknumberVerification;
            Party honest;
            address challenger;
        }

        enum Party {
            None,
            Claimer,
            Challenger
        }
    }

    #[derive(Debug)]
    #[sol(rpc)]
    interface IAMB {
        function requireToPassMessage(address _contract, bytes memory _data, uint256 _gas) external returns (bytes32);
        function executeMessageCall(address _contract, address _sender, bytes memory _data, bytes32 _messageId, uint256 _gas) external;
        function messageSender() external view returns (address);
        function maxGasPerTx() external view returns (uint256);
        function messageId() external view returns (bytes32);
        function messageSourceChainId() external view returns (bytes32);
    }

    #[derive(Debug)]
    #[sol(rpc)]
    interface IWETH {
        function deposit() external payable;
        function withdraw(uint256 wad) external;
        function approve(address guy, uint256 wad) external returns (bool);
        function transfer(address dst, uint256 wad) external returns (bool);
        function transferFrom(address src, address dst, uint256 wad) external returns (bool);
        function balanceOf(address) external view returns (uint256);
        function allowance(address owner, address spender) external view returns (uint256);
        function mintMock(address to, uint256 amount) external;
        function burn(uint256 wad) external;
    }
}

// Type aliases to reduce verbosity throughout the codebase
// All Claim structs are identical, so we standardize on IVeaOutboxArbToEth's version
pub type Claim = IVeaOutboxArbToEth::Claim;
pub type Party = IVeaOutboxArbToEth::Party;
