use std::fmt::{Debug, Display};

use crate::{
    components::fetcher::FetchResult,
    types::{Block, BlockHash},
};

#[derive(Debug)]
pub enum Event<I> {
    /// New peer connected event.  The joiner process requires a network peer to start.
    NewPeerConnected(I),

    /// Signal to ourselves that we are done syncing
    Done,

    //////////////////////////////////////////////////////////////
    /// Result of requesting a block by hash from network peers.
    GetBlockHashResult(BlockHash, BlockByHashResult<I>),

    /// Result of requesting a block by height from network peers.
    GetBlockHeightResult(u64, BlockByHeightResult<I>),

    /// Result of validating a block?
    // TODO: Not clear what this has to do with deploys
    GetDeploysResult(DeploysResult<I>),

    /// Signal to start downloading deploys
    StartDownloadingDeploys,

    /// Signal from linear chain that block has been processed.
    BlockHandled(Box<Block>),
}

#[derive(Debug)]
pub enum DeploysResult<I> {
    Found(Box<Block>),
    NotFound(Box<Block>, I),
}

#[derive(Debug)]
pub enum FetchResultOrAbsent<T, I> {
    FetchResult(FetchResult<T, I>),
    Absent(I),
}

pub type BlockByHashResult<I> = FetchResultOrAbsent<Block, I>;
pub type BlockByHeightResult<I> = FetchResultOrAbsent<Block, I>;

/// Contains either a found object pointer or a retry strategy.
pub enum FoundOrRetry<O, R> {
    Found(O),
    Retry(R),
}

impl<I> Display for Event<I>
where
    I: Debug + Display,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::GetBlockHashResult(block_hash, r) => {
                write!(f, "Get block result for {}: {:?}", block_hash, r)
            }
            Event::GetDeploysResult(result) => {
                write!(f, "Get deploys for block result {:?}", result)
            }
            Event::StartDownloadingDeploys => write!(f, "Start downloading deploys event."),
            Event::NewPeerConnected(peer_id) => write!(f, "A new peer connected: {}", peer_id),
            Event::BlockHandled(block) => {
                let hash = block.hash();
                let height = block.height();
                write!(
                    f,
                    "Block has been handled by consensus. Hash {}, height {}",
                    hash, height
                )
            }
            Event::GetBlockHeightResult(height, res) => {
                write!(f, "Get block result for height {}: {:?}", height, res)
            }
            Event::Done => {
                write!(f, "Handled event")
            }
        }
    }
}
