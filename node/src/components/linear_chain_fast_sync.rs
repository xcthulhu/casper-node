//! Fast linear chain synchronizer.
mod event;
mod metrics;
mod peers;
mod state;
mod traits;

use std::{
    collections::BTreeMap,
    convert::Infallible,
    fmt::{Debug, Display},
    mem,
};

use datasize::DataSize;
use prometheus::Registry;
use tracing::{error, info, trace, warn};

use casper_types::{Key, PublicKey, U512};

use crate::{
    components::{
        fetcher::FetchResult,
        linear_chain_fast_sync::event::{
            BlockByHashResult, BlockByHeightResult, DeploysResult, FoundOrRetry, TrieResult,
        },
        storage::{self, Storage},
        Component,
    },
    effect::{EffectBuilder, EffectExt, EffectOptionExt, Effects},
    types::{Block, BlockByHeight, BlockHash, BlockHeader, Chainspec, FinalizedBlock, Item},
    NodeRng,
};

use casper_execution_engine::{
    shared::{newtypes::Blake2bHash, stored_value::StoredValue},
    storage::trie::Trie,
};
pub use event::Event;
pub use metrics::LinearChainSyncMetrics;
pub use peers::PeersState;
pub use state::State;
pub use traits::ReactorEventT;

#[derive(DataSize, Debug)]
pub(crate) struct LinearChainFastSync<I> {
    #[data_size(skip)]
    metrics: LinearChainSyncMetrics,

    /// The phase of the linear chain sync.
    // TODO: Deprecate me
    state: State,

    /// The trusted hash we are using to sync.
    trusted_hash: Option<BlockHash>,

    /// Initial peer
    // TODO: combine me with peers
    initial_peer_id: Option<I>,

    /// Peers to query for data.  Misbehaving peers will be banned.
    peers: PeersState<I>,

    /// The header for the trusted hash.
    trusted_header: Option<BlockHeader>,

    /// The first trie
    // TODO: delete me
    #[data_size(skip)]
    first_trie: Option<Box<Trie<Key, StoredValue>>>,

    /// During synchronization we might see new eras being created.
    /// Track the highest block height and wait until it's handled by consensus.
    highest_block_seen: u64,

    /// The validators and their weights from genesis.
    genesis_validator_weights: BTreeMap<PublicKey, U512>,

    /// As we process blocks with ascending eras, we track the validators for those eras.
    current_era_validators: Option<BTreeMap<PublicKey, U512>>,
}

impl<I> LinearChainFastSync<I>
where
    I: Clone + PartialEq + Display + Send + 'static,
{
    pub fn new<Err>(
        registry: &Registry,
        _chainspec: &Chainspec,
        _storage: &Storage,
        trusted_hash: Option<BlockHash>,
        genesis_validator_weights: BTreeMap<PublicKey, U512>,
    ) -> Result<Self, Err>
    where
        Err: From<prometheus::Error> + From<storage::Error>,
    {
        let phase = State::sync_trusted_hash(genesis_validator_weights.clone());
        Ok(LinearChainFastSync {
            metrics: LinearChainSyncMetrics::new(registry)?,
            state: phase,
            trusted_hash,
            trusted_header: None,
            first_trie: None,
            initial_peer_id: None,
            peers: PeersState::new(),
            highest_block_seen: 0,
            genesis_validator_weights,
            current_era_validators: None,
        })
    }

    /// Returns `true` if we have finished syncing linear chain.
    pub fn is_synced(&self) -> bool {
        self.trusted_hash.is_none() || matches!(self.state, State::None)
    }

    /// Mark the process as finished syncing
    fn mark_as_done(&mut self) {
        // If there is no trusted hash, syncing is done (or never started)
        self.trusted_hash = None;
    }

    // TODO: move initial peer into peers object
    fn ban_peer(&mut self, initial_peer_id: I, peer: &I) -> I {
        self.peers.ban(peer);
        if peer == &initial_peer_id {
            match self.peers.get_peer() {
                None => {
                    panic!("All peers have been banned, cannot continue joining.")
                }
                Some(peer) => {
                    self.initial_peer_id = Some(peer.clone());
                    peer
                }
            }
        } else {
            initial_peer_id
        }
    }

    /// Extracts a `Box<Block>` from a [BlockByHashResult].
    /// Creates retry [Effects] to try to get it again if it was not found.
    pub fn extract_block_by_hash_result<'a, REv>(
        &mut self,
        effect_builder: EffectBuilder<REv>,
        rng: &mut NodeRng,
        initial_peer_id: I,
        block_hash: BlockHash,
        block_by_hash_result: &'a BlockByHashResult<I>,
    ) -> FoundOrRetry<&'a Box<Block>, Effects<Event<I>>>
    where
        REv: ReactorEventT<I>,
    {
        match block_by_hash_result {
            BlockByHashResult::Absent(peer) => {
                self.metrics.observe_get_block_by_hash();
                trace!(%block_hash, %peer, "failed to download block by hash. Trying next peer");
                self.peers.mark_peer_did_not_have_data(peer);
                FoundOrRetry::Retry(self.fetch_block_by_hash(
                    effect_builder,
                    rng,
                    initial_peer_id,
                    block_hash,
                ))
            }
            BlockByHashResult::FetchResult(FetchResult::FromStorage(block)) => {
                // We shouldn't get invalid data from the storage.
                // If we do, it's a bug.
                assert_eq!(*block.hash(), block_hash, "Block hash mismatch.");
                assert_eq!(
                    block.header().hash(),
                    block_hash,
                    "Hash of header mismatch."
                );
                assert_eq!(
                    block.body().hash(),
                    *block.header().body_hash(),
                    "Hash of body mismatch."
                );
                trace!(%block_hash, "linear block found in the local storage.");
                FoundOrRetry::Found(block)
            }
            BlockByHashResult::FetchResult(FetchResult::FromPeer(block, peer)) => {
                self.metrics.observe_get_block_by_hash();
                trace!(%block_hash, %peer, "block downloaded from a peer using hash");

                // Check the cryptographic hash of the header.
                // It must match the hash requested and the stated block header.
                // Body hash must also match body hash in header.
                // If not, ban the peer and request the block hash from another peer.
                let actual_block_hash = block.header().hash();
                let actual_body_hash = block.body().hash();
                if actual_block_hash != block_hash
                    || actual_block_hash != *block.hash()
                    || actual_body_hash != *block.header().body_hash()
                {
                    warn!(
                        "Block is cryptographically invalid.\n\
                         \n\
                         Expected block hash: {},\n\
                         What block reports its hash is: {}\n\
                         Actual hash of block header: {}.\n\
                         \n\
                         What header expects hash of block body to be: {}\n\
                         Actual block body hash: {}\n\
                         \n\
                         Disconnecting from peer: {}",
                        block_hash,
                        block.hash(),
                        actual_block_hash,
                        block.header().body_hash(),
                        actual_body_hash,
                        peer
                    );
                    // NOTE: Signal misbehaving validator to pool of peers.
                    let initial_peer_id = self.ban_peer(initial_peer_id, peer);
                    FoundOrRetry::Retry(self.fetch_block_by_hash(
                        effect_builder,
                        rng,
                        initial_peer_id,
                        block_hash,
                    ))
                } else {
                    self.peers.mark_peer_as_had_success_with(peer.clone());
                    FoundOrRetry::Found(block)
                }
            }
        }
    }

    /// Extracts a `Trie<Key, StoredValue>` from a [TrieResult].
    /// Creates retry [Effects] to try to get it again if it was not found.
    // TODO: Don't use references
    pub fn extract_trie_result<'a, REv>(
        &mut self,
        effect_builder: EffectBuilder<REv>,
        rng: &mut NodeRng,
        initial_peer_id: I,
        trie_key: Blake2bHash,
        trie_result: &'a TrieResult<I>,
    ) -> FoundOrRetry<&'a Box<Trie<Key, StoredValue>>, Effects<Event<I>>>
    where
        REv: ReactorEventT<I>,
    {
        match trie_result {
            TrieResult::Absent(peer) => {
                self.metrics.observe_get_block_by_hash();
                trace!(%trie_key, %peer, "failed to download trie. Trying next peer");
                self.peers.mark_peer_did_not_have_data(peer);
                FoundOrRetry::Retry(self.fetch_trie(effect_builder, rng, initial_peer_id, trie_key))
            }
            TrieResult::FetchResult(FetchResult::FromStorage(trie)) => {
                // We shouldn't get invalid data from the storage.
                // If we do, it's a bug.
                assert_eq!(trie.id(), trie_key, "Trie key mismatch.");
                trace!(%trie_key, "trie key already present in global state.");
                FoundOrRetry::Found(trie)
            }
            TrieResult::FetchResult(FetchResult::FromPeer(trie, peer)) => {
                self.metrics.observe_get_trie();
                trace!(%trie_key, %peer, "trie downloaded from a peer");

                // Check the cryptographic hash of the header.
                // It must match the hash requested and the stated block header.
                // Body hash must also match body hash in header.
                // If not, ban the peer and request the block hash from another peer.
                let actual_hash_of_trie = trie.id();
                if actual_hash_of_trie != trie_key {
                    warn!(
                        "Trie is cryptographically invalid.\n\
                         \n\
                         Expected trie_key: {},\n\
                         Actual hash of trie: {}",
                        trie_key, actual_hash_of_trie
                    );
                    // NOTE: Signal misbehaving validator to pool of peers.
                    let initial_peer_id = self.ban_peer(initial_peer_id, peer);
                    FoundOrRetry::Retry(self.fetch_trie(
                        effect_builder,
                        rng,
                        initial_peer_id,
                        trie_key,
                    ))
                } else {
                    self.peers.mark_peer_as_had_success_with(peer.clone());
                    FoundOrRetry::Found(trie)
                }
            }
        }
    }

    fn block_downloaded<REv>(
        &mut self,
        effect_builder: EffectBuilder<REv>,
        rng: &mut NodeRng,
        initial_peer_id: I,
        block: &Box<Block>,
    ) -> Effects<Event<I>>
    where
        REv: ReactorEventT<I>,
    {
        self.peers.reset_peers_to_try(rng);

        if block.height() > self.highest_block_seen {
            self.highest_block_seen = block.height();
        }

        match &mut self.state {
            State::None => unreachable!("State::None should never handle a block downloaded"),

            State::SyncingTrustedHash { linear_chain, .. } => {
                linear_chain.push(*block.clone());

                if block.header().is_genesis_child() {
                    info!("linear chain downloaded. Start downloading deploys.");
                    effect_builder
                        .immediately()
                        .event(move |_| Event::StartDownloadingDeploys)
                } else {
                    self.fetch_block_by_hash(
                        effect_builder,
                        rng,
                        initial_peer_id,
                        *block.header().parent_hash(),
                    )
                }
            }
            State::SyncingDescendants {
                ref mut latest_block,
                ..
            } => {
                *latest_block = block.clone();
                // When synchronizing descendants, we want to download block and execute it
                // before trying to download the next block in linear chain.
                self.fetch_next_block_deploys(effect_builder)
            }
        }
    }

    /// Handles an event indicating that a linear chain block has been executed and handled by
    /// consensus component. This is a signal that we can safely continue with the next blocks,
    /// without worrying about timing and/or ordering issues.
    /// Returns effects that are created as a response to that event.
    fn handle_block_processed_by_linear_chain<REv>(
        &mut self,
        effect_builder: EffectBuilder<REv>,
        rng: &mut NodeRng,
        initial_peer_id: I,
        block: Block,
    ) -> Effects<Event<I>>
    where
        REv: ReactorEventT<I>,
    {
        let hash = block.hash();
        let block_height = block.height();
        trace!(%hash, %block_height, "Downloaded linear chain block.");

        // Reset peers before creating new requests.
        self.peers.reset_peers_to_try(rng);
        let mut curr_state = mem::replace(&mut self.state, State::None);
        match curr_state {
            State::None => panic!("Block handled when in {:?} state.", &curr_state),
            // If the block we are handling is the highest block seen, transition to syncing
            // descendants
            State::SyncingTrustedHash {
                ref latest_block,
                validator_weights,
                ..
            } if self.highest_block_seen == block_height => {
                // TODO: Fail gracefully in these cases
                match latest_block.as_ref() {
                    Some(expected) => assert_eq!(
                        expected, &block,
                        "Block execution result doesn't match received block."
                    ),
                    None => panic!("Unexpected block execution results."),
                }

                info!(%block_height, "Finished synchronizing linear chain up until trusted hash.");
                let peer = self.peers.get_peer_unsafe();
                // Kick off syncing trusted hash descendants.
                self.state = State::sync_descendants(block, validator_weights);
                fetch_block_at_height(effect_builder, peer, block_height + 1)
            }
            // Keep syncing from genesis if we haven't reached the trusted block hash
            State::SyncingTrustedHash {
                ref mut validator_weights,
                ref latest_block,
                ..
            } => {
                match latest_block.as_ref() {
                    Some(expected) => assert_eq!(
                        expected, &block,
                        "Block execution result doesn't match received block."
                    ),
                    None => panic!("Unexpected block execution results."),
                }
                if let Some(validator_weights_for_new_era) =
                    block.header().next_era_validator_weights()
                {
                    *validator_weights = validator_weights_for_new_era.clone();
                }
                self.state = curr_state;
                self.fetch_next_block_deploys(effect_builder)
            }
            State::SyncingDescendants {
                ref latest_block,
                validators_for_next_block: ref mut validators_for_latest_block,
                ..
            } => {
                assert_eq!(
                    **latest_block, block,
                    "Block execution result doesn't match received block."
                );
                match block.header().next_era_validator_weights() {
                    None => (),
                    Some(validators_for_next_era) => {
                        *validators_for_latest_block = validators_for_next_era.clone();
                    }
                }
                self.state = curr_state;
                self.fetch_next_block(effect_builder, rng, initial_peer_id, &block.header())
            }
        }
    }

    /// Returns effects for fetching next block's deploys.
    fn fetch_next_block_deploys<REv>(
        &mut self,
        effect_builder: EffectBuilder<REv>,
    ) -> Effects<Event<I>>
    where
        REv: ReactorEventT<I>,
    {
        let peer = self.peers.get_peer_unsafe();

        let next_block = match &mut self.state {
            State::None => {
                panic!("Tried fetching next block when in {:?} state.", self.state)
            }
            State::SyncingTrustedHash {
                linear_chain,
                latest_block,
                ..
            } => match linear_chain.pop() {
                None => None,
                Some(block) => {
                    // Update `latest_block` so that we can verify whether result of execution
                    // matches the expected value.
                    latest_block.replace(block.clone());
                    Some(block)
                }
            },
            State::SyncingDescendants { latest_block, .. } => Some((**latest_block).clone()),
        };

        next_block.map_or_else(
            || {
                warn!("tried fetching next block deploys when there was no block.");
                Effects::new()
            },
            |block| {
                self.metrics.reset_start_time();
                fetch_block_deploys(effect_builder, peer, block)
            },
        )
    }

    // TODO: Don't get blocks get block headers
    fn fetch_block_by_hash<REv>(
        &mut self,
        effect_builder: EffectBuilder<REv>,
        rng: &mut NodeRng,
        initial_peer_id: I,
        block_hash: BlockHash,
    ) -> Effects<Event<I>>
    where
        REv: ReactorEventT<I>,
    {
        self.metrics.reset_start_time();
        self.peers.reset_peers_to_try(rng);
        let peer = self.peers.get_peer().unwrap_or(initial_peer_id);
        effect_builder
            .fetch_block(block_hash, peer.clone())
            .map_or_else(
                move |fetch_result| {
                    Event::GetBlockHashResult(
                        block_hash,
                        BlockByHashResult::FetchResult(fetch_result),
                    )
                },
                move || Event::GetBlockHashResult(block_hash, BlockByHashResult::Absent(peer)),
            )
    }

    fn fetch_trie<REv>(
        &mut self,
        effect_builder: EffectBuilder<REv>,
        rng: &mut NodeRng,
        initial_peer_id: I,
        trie_key: Blake2bHash,
    ) -> Effects<Event<I>>
    where
        REv: ReactorEventT<I>,
    {
        self.metrics.reset_start_time();
        self.peers.reset_peers_to_try(rng);
        // let peer = self.peers.get_peer().unwrap_or(initial_peer_id);
        let peer = initial_peer_id.clone();
        effect_builder
            .fetch_trie(trie_key, peer.clone())
            .map_or_else(
                move |fetch_result| {
                    Event::GetTrieResult(trie_key, TrieResult::FetchResult(fetch_result))
                },
                move || Event::GetTrieResult(trie_key, TrieResult::Absent(peer)),
            )
    }

    // TODO: Delete me
    fn fetch_next_block<REv>(
        &mut self,
        effect_builder: EffectBuilder<REv>,
        rng: &mut NodeRng,
        initial_peer: I,
        block_header: &BlockHeader,
    ) -> Effects<Event<I>>
    where
        REv: ReactorEventT<I>,
    {
        match self.state {
            State::SyncingTrustedHash { .. } => self.fetch_block_by_hash(
                effect_builder,
                rng,
                initial_peer,
                *block_header.parent_hash(),
            ),
            State::SyncingDescendants { .. } => {
                self.peers.reset_peers_to_try(rng);
                // Note: load bearing unsafe below, `.unwrap_or(initial_peer)`!
                let peer = self.peers.get_peer_unsafe();
                let next_height = block_header.height() + 1;
                self.metrics.reset_start_time();
                fetch_block_at_height(effect_builder, peer, next_height)
            }
            State::None => {
                unreachable!("Tried fetching block when in {:?} state", self.state)
            }
        }
    }

    fn latest_block(&self) -> Option<&Block> {
        match &self.state {
            State::SyncingTrustedHash { latest_block, .. } => Option::as_ref(&*latest_block),
            State::SyncingDescendants { latest_block, .. } => Some(&*latest_block),
            State::None => None,
        }
    }
}

impl<I, REv> Component<REv> for LinearChainFastSync<I>
where
    I: Display + Clone + Send + PartialEq + Debug + 'static,
    REv: ReactorEventT<I>,
{
    type Event = Event<I>;
    type ConstructionError = Infallible;

    fn handle_event(
        &mut self,
        effect_builder: EffectBuilder<REv>,
        rng: &mut NodeRng,
        event: Self::Event,
    ) -> Effects<Self::Event> {
        //////////////////
        // Trusted Hash //
        //////////////////

        let trusted_hash = match self.trusted_hash {
            Some(trusted_hash) => trusted_hash.clone(),
            // If there is no trusted hash, there is nothing to sync and we don't emit any effects.
            None => {
                warn!(%event, "No trusted block hash, joiner dropping event");
                return Effects::new();
            }
        };

        /////////////////////////
        // Await Network Peers //
        /////////////////////////

        // If there is an incoming peer, add them and set the initial peer as necessary
        if let Event::NewPeerConnected(peer_id) = &event {
            trace!(%peer_id, "new peer connected");
            if self.initial_peer_id.is_none() {
                self.initial_peer_id = Some(peer_id.clone());
            }
            self.peers.add_peer(peer_id.clone());
        }

        let initial_peer_id = match self.initial_peer_id.as_ref() {
            Some(initial_peer_id) => initial_peer_id.clone(),
            // If we have no peers, there's nothing we can do so we don't emit any effects.
            None => {
                warn!(%event, "No peers, joiner dropping event");
                return Effects::new();
            }
        };

        ////////////////////////////////
        // Await Trusted Block Header //
        ////////////////////////////////

        // If there is an incoming block with the trusted header, set it.
        // TODO: Create fetcher for block headers and use that
        if let Event::GetBlockHashResult(block_hash, block_by_hash_result) = &event {
            match self.extract_block_by_hash_result(
                effect_builder,
                rng,
                initial_peer_id.clone(),
                *block_hash,
                block_by_hash_result,
            ) {
                FoundOrRetry::Found(block) if *block.hash() == trusted_hash => {
                    self.trusted_header = Some(block.header().clone());
                }
                FoundOrRetry::Retry(effects) => {
                    return effects;
                }
                // If it's not the trusted header don't do anything for now.
                // This will be handled differently depending on what phase we are in below.
                _ => (),
            }
        }

        // If we don't already have the trusted header, fetch it.
        let trusted_header = match self.trusted_header.as_ref() {
            None => {
                warn!(%event, "No trusted header, dropping event");
                return self.fetch_block_by_hash(
                    effect_builder,
                    rng,
                    initial_peer_id.clone(),
                    trusted_hash,
                );
            }
            Some(trusted_header) => trusted_header.clone(),
        };

        ///////////////////////////////////
        // Get Trusted Validator Weights //
        ///////////////////////////////////

        // TODO: Needed for finality signatures

        // Procedure:
        // Keep calling fetch_block_by_hash for ancestors of linear chain
        // ...until we hit a switch block and use that

        /////////////////////////////
        // Get Recent Block Header //
        /////////////////////////////

        // TODO

        // Procedure:
        // Keep getting block headers by height (or era id?)
        // ...until we have a recent block with our version.

        /////////////////////
        // Copy Trie Store //
        /////////////////////

        if let Event::GetTrieResult(trie_key, trie_result) = &event {
            match self.extract_trie_result(
                effect_builder,
                rng,
                initial_peer_id.clone(),
                *trie_key,
                trie_result,
            ) {
                FoundOrRetry::Found(trie) => {
                    info!(%trie_key, "Got Trie!");
                    self.first_trie = Some(trie.clone());

                    // TODO: Delete me, for legacy sync
                    return self.fetch_block_by_hash(
                        effect_builder,
                        rng,
                        initial_peer_id.clone(),
                        trusted_hash,
                    );
                }
                FoundOrRetry::Retry(effects) => {
                    info!(%trie_key, "Trying to get trie again");
                    return effects;
                }
            }
        }

        if self.first_trie.is_none() {
            let trie_key = Blake2bHash::from(trusted_header.state_root_hash().clone());
            info!(%trie_key, "Getting initial trie");
            return self.fetch_trie(effect_builder, rng, initial_peer_id.clone(), trie_key);
        }

        info!("Got initial trie");

        /////////////////////
        // Sync To Current //
        /////////////////////

        // TODO

        ///////////////////////////////////////////////////////////

        // Legacy Sync

        match event {
            Event::GetTrieResult(_, _) => {
                // Do nothing for now
                Effects::new()
            }
            Event::NewPeerConnected(_peer_id) => {
                // This event was already handled when we were getting the initial peer id.
                // Do nothing.
                Effects::new()
            }
            Event::GetBlockHeightResult(block_height, fetch_result) => {
                match fetch_result {
                    BlockByHeightResult::Absent(peer) => {
                        self.metrics.observe_get_block_by_height();
                        trace!(%block_height, %peer, "failed to download block by height. Trying next peer");
                        self.peers.mark_peer_did_not_have_data(&peer);
                        match self.peers.get_peer() {
                            None => {
                                // `block_height` not found on any of the peers.
                                // We have synchronized all, currently existing, descendants of
                                // trusted hash.

                                // TODO: check stopping condition?
                                self.mark_as_done();
                                info!("finished synchronizing descendants of the trusted hash.");
                                Effects::new()
                            }
                            Some(peer) => {
                                self.metrics.reset_start_time();
                                fetch_block_at_height(effect_builder, peer, block_height)
                            }
                        }
                    }
                    BlockByHeightResult::FetchResult(FetchResult::FromStorage(block)) => {
                        // We shouldn't get invalid data from the storage.
                        // If we do, it's a bug.
                        assert_eq!(block.height(), block_height, "Block height mismatch.");
                        trace!(%block_height, "Linear block found in the local storage.");
                        // When syncing descendants of a trusted hash, we might have some of them in
                        // our local storage. If that's the case, just continue.
                        self.block_downloaded(effect_builder, rng, initial_peer_id.clone(), &block)
                    }
                    BlockByHeightResult::FetchResult(FetchResult::FromPeer(block, peer)) => {
                        self.metrics.observe_get_block_by_height();
                        trace!(%block_height, %peer, "linear chain block downloaded from a peer");
                        // TODO: check block body
                        if block.height() != block_height
                            || *block.header().parent_hash()
                                != self.latest_block().unwrap().header().hash()
                        {
                            warn!(
                                %peer,
                                got_height = block.height(),
                                expected_height = block_height,
                                got_parent = %block.header().parent_hash(),
                                expected_parent = %self.latest_block().unwrap().hash(),
                                "block mismatch",
                            );
                            // NOTE: Signal misbehaving validator to networking layer.
                            self.peers.ban(&peer);
                            return self.handle_event(
                                effect_builder,
                                rng,
                                Event::GetBlockHeightResult(
                                    block_height,
                                    BlockByHeightResult::Absent(peer),
                                ),
                            );
                        }
                        self.peers.mark_peer_as_had_success_with(peer);
                        self.block_downloaded(effect_builder, rng, initial_peer_id.clone(), &block)
                    }
                }
            }

            Event::GetBlockHashResult(block_hash, block_by_hash_result) => {
                let block = match self.extract_block_by_hash_result(
                    effect_builder,
                    rng,
                    initial_peer_id.clone(),
                    block_hash,
                    &block_by_hash_result,
                ) {
                    FoundOrRetry::Found(block) => block,
                    FoundOrRetry::Retry(effects) => {
                        return effects;
                    }
                };
                self.block_downloaded(effect_builder, rng, initial_peer_id.clone(), block)
            }

            Event::GetDeploysResult(fetch_result) => {
                self.metrics.observe_get_deploys();
                match fetch_result {
                    event::DeploysResult::Found(block) => {
                        let block_hash = block.hash();
                        trace!(%block_hash, "deploys for linear chain block found");
                        // Reset used peers so we can download next block with the full set.
                        self.peers.reset_peers_to_try(rng);
                        // Execute block
                        let finalized_block: FinalizedBlock = (*block).into();
                        effect_builder.execute_block(finalized_block).ignore()
                    }
                    event::DeploysResult::NotFound(block, peer) => {
                        let block_hash = block.hash();
                        trace!(%block_hash, %peer, "deploy for linear chain block not found. Trying next peer");
                        self.peers.mark_peer_did_not_have_data(&peer);
                        match self.peers.get_peer() {
                            None => {
                                error!(%block_hash,
                                "could not download deploys from linear chain block.");
                                panic!("Failed to download linear chain deploys.")
                            }
                            Some(peer) => {
                                self.metrics.reset_start_time();
                                fetch_block_deploys(effect_builder, peer, *block)
                            }
                        }
                    }
                }
            }
            Event::StartDownloadingDeploys => {
                // Start downloading deploys from the first block of the linear chain.
                self.peers.reset_peers_to_try(rng);
                self.fetch_next_block_deploys(effect_builder)
            }
            Event::BlockHandled(block) => {
                let block_height = block.height();
                let block_hash = *block.hash();
                let effects = self.handle_block_processed_by_linear_chain(
                    effect_builder,
                    rng,
                    initial_peer_id.clone(),
                    *block,
                );
                trace!(%block_height, %block_hash, "block handled");
                effects
            }
        }
    }
}

fn fetch_block_deploys<I: Clone + Send + 'static, REv>(
    effect_builder: EffectBuilder<REv>,
    peer: I,
    block: Block,
) -> Effects<Event<I>>
where
    REv: ReactorEventT<I>,
{
    let block_timestamp = block.header().timestamp();
    effect_builder
        .validate_block(peer.clone(), block, block_timestamp)
        .event(move |(found, block)| {
            if found {
                Event::GetDeploysResult(DeploysResult::Found(Box::new(block)))
            } else {
                Event::GetDeploysResult(DeploysResult::NotFound(Box::new(block), peer))
            }
        })
}

fn fetch_block_at_height<I: Send + Clone + 'static, REv>(
    effect_builder: EffectBuilder<REv>,
    peer: I,
    block_height: u64,
) -> Effects<Event<I>>
where
    REv: ReactorEventT<I>,
{
    effect_builder
        .fetch_block_by_height(block_height, peer.clone())
        .map_or_else(
            move |fetch_result| {
                let block_by_height_result = match fetch_result {
                    FetchResult::FromPeer(result, peer) => match *result {
                        BlockByHeight::Absent(ret_height) => {
                            warn!(
                                "Fetcher returned result for invalid height. Expected {}, got {}",
                                block_height, ret_height
                            );
                            BlockByHeightResult::Absent(peer)
                        }
                        BlockByHeight::Block(block) => {
                            BlockByHeightResult::FetchResult(FetchResult::FromPeer(block, peer))
                        }
                    },
                    FetchResult::FromStorage(result) => match *result {
                        BlockByHeight::Absent(_) => {
                            // Fetcher should try downloading the block from a peer
                            // when it can't find it in the storage.
                            panic!("Should not return `Absent` in `FromStorage`.")
                        }
                        BlockByHeight::Block(block) => {
                            BlockByHeightResult::FetchResult(FetchResult::FromStorage(block))
                        }
                    },
                };
                Event::GetBlockHeightResult(block_height, block_by_height_result)
            },
            move || Event::GetBlockHeightResult(block_height, BlockByHeightResult::Absent(peer)),
        )
}
