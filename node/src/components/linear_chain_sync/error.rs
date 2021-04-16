use thiserror::Error;

use casper_execution_engine::{core::engine_state, shared::newtypes::Blake2bHash};

use crate::types::BlockHash;

#[allow(dead_code)]
#[derive(Error, Debug)]
pub enum LinearChainSyncError {
    #[error(transparent)]
    ExecutionEngineError(#[from] engine_state::Error),

    #[error("Could not fetch trie key: {trie_key}")]
    RanOutOfFetchTrieRetries { trie_key: Blake2bHash },

    #[error("Could not fetch block hash: {block_hash}")]
    RanOutOfHeaderByHashFetchRetries { block_hash: BlockHash },

    #[error("Could not fetch block by height: {height}")]
    RanOutOfHeaderByHeightFetchRetries { height: u64 },
}
