use casper_execution_engine::{shared::stored_value::StoredValue, storage::trie::Trie};
use casper_types::Key;

use crate::{
    effect::requests::{
        BlockExecutorRequest, BlockValidationRequest, FetcherRequest, StorageRequest,
    },
    types::{Block, BlockByHeight},
};

pub trait ReactorEventT<I>:
    From<StorageRequest>
    + From<FetcherRequest<I, Block>>
    + From<FetcherRequest<I, BlockByHeight>>
    + From<FetcherRequest<I, Trie<Key, StoredValue>>>
    + From<BlockValidationRequest<Block, I>>
    + From<BlockExecutorRequest>
    + Send
{
}

impl<I, REv> ReactorEventT<I> for REv where
    REv: From<StorageRequest>
        + From<FetcherRequest<I, Block>>
        + From<FetcherRequest<I, BlockByHeight>>
        + From<FetcherRequest<I, Trie<Key, StoredValue>>>
        + From<BlockValidationRequest<Block, I>>
        + From<BlockExecutorRequest>
        + Send
{
}
