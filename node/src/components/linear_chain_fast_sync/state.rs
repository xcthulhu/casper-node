use std::{collections::BTreeMap, fmt::Display};

use datasize::DataSize;

use crate::types::Block;
use casper_types::{PublicKey, U512};

#[derive(DataSize, Debug)]
pub enum State {
    /// No syncing of the linear chain configured.
    None,
    /// Synchronizing the linear chain up until trusted hash.
    SyncingTrustedHash {
        /// Chain of downloaded blocks from the linear chain.
        /// We will `pop()` when executing blocks.
        linear_chain: Vec<Block>,
        /// The most recent block we started to execute. This is updated whenever we start
        /// downloading deploys for the next block to be executed.
        latest_block: Box<Option<Block>>,
        /// The weights of the validators for latest block being added.
        validator_weights: BTreeMap<PublicKey, U512>,
    },
    /// Synchronizing the descendants of the trusted hash.
    SyncingDescendants {
        /// The most recent block we started to execute. This is updated whenever we start
        /// downloading deploys for the next block to be executed.
        latest_block: Box<Block>,
        /// The validator set for the most recent block being synchronized.
        validators_for_next_block: BTreeMap<PublicKey, U512>,
    },
}

impl Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl State {
    pub fn sync_trusted_hash(validator_weights: BTreeMap<PublicKey, U512>) -> Self {
        State::SyncingTrustedHash {
            linear_chain: Vec::new(),
            latest_block: Box::new(None),
            validator_weights,
        }
    }

    pub fn sync_descendants(
        latest_block: Block,
        validators_for_latest_block: BTreeMap<PublicKey, U512>,
    ) -> Self {
        State::SyncingDescendants {
            latest_block: Box::new(latest_block),
            validators_for_next_block: validators_for_latest_block,
        }
    }
}
