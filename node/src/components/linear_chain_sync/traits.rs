use crate::{
    effect::{
        announcements::ControlAnnouncement,
        requests::{
            BlockExecutorRequest, BlockValidationRequest, FetcherRequest, StateStoreRequest,
            StorageRequest,
        },
    },
    types::{Block, BlockHeaderAndMetadata},
};
pub trait ReactorEventT<I>:
    From<StorageRequest>
    + From<FetcherRequest<I, Block>>
    + From<FetcherRequest<I, BlockHeaderAndMetadata>>
    + From<BlockValidationRequest<Block, I>>
    + From<BlockExecutorRequest>
    + From<StateStoreRequest>
    + From<ControlAnnouncement>
    + Send
{
}

impl<I, REv> ReactorEventT<I> for REv where
    REv: From<StorageRequest>
        + From<FetcherRequest<I, Block>>
        + From<FetcherRequest<I, BlockHeaderAndMetadata>>
        + From<BlockValidationRequest<Block, I>>
        + From<BlockExecutorRequest>
        + From<StateStoreRequest>
        + From<ControlAnnouncement>
        + Send
{
}
