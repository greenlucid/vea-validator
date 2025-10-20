#![allow(clippy::too_many_arguments)]

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
    #[derive(Debug)]
    #[sol(rpc)]
    interface IArbSys {
        event L2ToL1Tx(address caller, address indexed destination, uint256 indexed hash, uint256 indexed position, uint256 arbBlockNum, uint256 ethBlockNum, uint256 timestamp, uint256 callvalue, bytes data);
        function sendTxToL1(address destination, bytes calldata data) external payable returns (uint256);
        function sendMerkleTreeState() external view returns (uint256 size, bytes32 root, bytes32[] memory partials);
    }
    #[derive(Debug)]
    #[sol(rpc)]
    interface IOutbox {
        function l2ToL1Sender() external view returns (address);
        function executeTransaction(
            bytes32[] calldata proof,
            uint256 index,
            address l2Sender,
            address to,
            uint256 l2Block,
            uint256 l1Block,
            uint256 l2Timestamp,
            uint256 value,
            bytes calldata data
        ) external;
    }
}

pub type Claim = IVeaOutboxArbToEth::Claim;
pub type Party = IVeaOutboxArbToEth::Party;
