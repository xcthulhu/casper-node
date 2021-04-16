//! Common types used across multiple components.

pub(crate) mod appendable_block;
mod block;
pub mod chainspec;
mod deploy;
mod exit_code;
pub(crate) mod item;
pub mod json_compatibility;
mod node_config;
mod node_id;
mod peers_map;
mod status_feed;
mod timestamp;

use rand::{CryptoRng, RngCore};
#[cfg(not(test))]
use rand_chacha::ChaCha20Rng;

pub use block::{
    json_compatibility::JsonBlock, Block, BlockBody, BlockHash, BlockHeader, BlockSignatures,
    BlockValidationError, FinalitySignature,
};
pub(crate) use block::{BlockAndMetadata, BlockHeaderAndMetadata, FinalizedBlock, ProtoBlock};
pub(crate) use chainspec::ActivationPoint;
pub use chainspec::Chainspec;
pub use deploy::{
    Approval, Deploy, DeployHash, DeployHeader, DeployMetadata, DeployValidationFailure,
    Error as DeployError,
};
pub use exit_code::ExitCode;
pub use item::{Item, Tag};
pub use node_config::NodeConfig;
pub(crate) use node_id::NodeId;
pub use peers_map::PeersMap;
pub use status_feed::{ChainspecInfo, GetStatusResult, StatusFeed};
pub use timestamp::{TimeDiff, Timestamp};

/// An object-safe RNG trait that requires a cryptographically strong random number generator.
pub trait CryptoRngCore: CryptoRng + RngCore {}

impl<T> CryptoRngCore for T where T: CryptoRng + RngCore + ?Sized {}

/// The cryptographically secure RNG used throughout the node.
#[cfg(not(test))]
pub type NodeRng = ChaCha20Rng;

/// The RNG used throughout the node for testing.
#[cfg(test)]
pub type NodeRng = crate::testing::TestRng;
