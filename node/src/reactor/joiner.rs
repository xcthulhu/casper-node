//! Reactor used to join the network.

mod memory_metrics;

use std::{
    collections::BTreeMap,
    env,
    fmt::{self, Display, Formatter},
};

use datasize::DataSize;
use derive_more::From;
use memory_metrics::MemoryMetrics;
use prometheus::Registry;
use serde::Serialize;
use tracing::{debug, error, info, warn};

use casper_types::{Key, PublicKey, U512};

#[cfg(not(feature = "fast-sync"))]
use crate::components::linear_chain_sync::{self, LinearChainSync};
#[cfg(feature = "fast-sync")]
use crate::components::{
    linear_chain_fast_sync as linear_chain_sync,
    linear_chain_fast_sync::LinearChainFastSync as LinearChainSync,
};

#[cfg(test)]
use crate::testing::network::NetworkedReactor;
use crate::{
    components::{
        block_executor::{self, BlockExecutor},
        block_validator::{self, BlockValidator},
        chainspec_loader::{self, ChainspecLoader},
        consensus::{self, EraSupervisor, HighwayProtocol},
        contract_runtime::{self, ContractRuntime},
        deploy_acceptor::{self, DeployAcceptor},
        event_stream_server,
        event_stream_server::EventStreamServer,
        fetcher::{self, Fetcher},
        gossiper::{self, Gossiper},
        linear_chain,
        metrics::Metrics,
        network::{self, Network, NetworkIdentity, ENABLE_LIBP2P_NET_ENV_VAR},
        rest_server::{self, RestServer},
        small_network::{self, GossipedAddress, SmallNetwork, SmallNetworkIdentity},
        storage::{self, Storage},
        Component,
    },
    effect::{
        announcements::{
            BlockExecutorAnnouncement, ChainspecLoaderAnnouncement, ConsensusAnnouncement,
            DeployAcceptorAnnouncement, GossiperAnnouncement, LinearChainAnnouncement,
            NetworkAnnouncement,
        },
        requests::{
            BlockExecutorRequest, BlockProposerRequest, BlockValidationRequest,
            ChainspecLoaderRequest, ConsensusRequest, ContractRuntimeRequest, FetcherRequest,
            LinearChainRequest, MetricsRequest, NetworkInfoRequest, NetworkRequest, RestRequest,
            StateStoreRequest, StorageRequest,
        },
        EffectBuilder, Effects,
    },
    protocol::Message,
    reactor::{
        self,
        event_queue_metrics::EventQueueMetrics,
        initializer,
        validator::{self, Error, ValidatorInitConfig},
        EventQueueHandle, Finalize,
    },
    types::{Block, BlockByHeight, Deploy, NodeId, ProtoBlock, Tag, Timestamp},
    utils::{Source, WithDir},
    NodeRng,
};
use casper_execution_engine::{shared::stored_value::StoredValue, storage::trie::Trie};

/// Top-level event for the reactor.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, From, Serialize)]
#[must_use]
pub enum Event {
    /// Network event.
    #[from]
    Network(network::Event<Message>),

    /// Small Network event.
    #[from]
    SmallNetwork(small_network::Event<Message>),

    /// Storage event.
    #[from]
    Storage(#[serde(skip_serializing)] storage::Event),

    #[from]
    /// REST server event.
    RestServer(#[serde(skip_serializing)] rest_server::Event),

    #[from]
    /// Event stream server event.
    EventStreamServer(#[serde(skip_serializing)] event_stream_server::Event),

    /// Metrics request.
    #[from]
    MetricsRequest(#[serde(skip_serializing)] MetricsRequest),

    #[from]
    /// Chainspec Loader event.
    ChainspecLoader(#[serde(skip_serializing)] chainspec_loader::Event),

    /// Chainspec info request
    #[from]
    ChainspecLoaderRequest(#[serde(skip_serializing)] ChainspecLoaderRequest),

    /// Network info request.
    #[from]
    NetworkInfoRequest(#[serde(skip_serializing)] NetworkInfoRequest<NodeId>),

    /// Linear chain fetcher event.
    #[from]
    BlockFetcher(#[serde(skip_serializing)] fetcher::Event<Block>),

    /// Trie fetcher event.
    #[from]
    TrieFetcher(#[serde(skip_serializing)] fetcher::Event<Trie<Key, StoredValue>>),

    /// Linear chain (by height) fetcher event.
    #[from]
    BlockByHeightFetcher(#[serde(skip_serializing)] fetcher::Event<BlockByHeight>),

    /// Deploy fetcher event.
    #[from]
    DeployFetcher(#[serde(skip_serializing)] fetcher::Event<Deploy>),

    /// Deploy acceptor event.
    #[from]
    DeployAcceptor(#[serde(skip_serializing)] deploy_acceptor::Event),

    /// Block validator event.
    #[from]
    BlockValidator(#[serde(skip_serializing)] block_validator::Event<Block, NodeId>),

    /// Linear chain event.
    #[from]
    LinearChainSync(#[serde(skip_serializing)] linear_chain_sync::Event<NodeId>),

    /// Block executor event.
    #[from]
    BlockExecutor(#[serde(skip_serializing)] block_executor::Event),

    /// Contract Runtime event.
    #[from]
    ContractRuntime(#[serde(skip_serializing)] contract_runtime::Event),

    /// Linear chain event.
    #[from]
    LinearChain(#[serde(skip_serializing)] linear_chain::Event<NodeId>),

    /// Consensus component event.
    #[from]
    Consensus(#[serde(skip_serializing)] consensus::Event<NodeId>),

    /// Address gossiper event.
    #[from]
    AddressGossiper(gossiper::Event<GossipedAddress>),

    /// Requests.
    /// Linear chain block by hash fetcher request.
    #[from]
    BlockFetcherRequest(#[serde(skip_serializing)] FetcherRequest<NodeId, Block>),

    /// Trie fetcher request.
    #[from]
    TrieFetcherRequest(#[serde(skip_serializing)] FetcherRequest<NodeId, Trie<Key, StoredValue>>),

    /// Linear chain block by height fetcher request.
    #[from]
    BlockByHeightFetcherRequest(#[serde(skip_serializing)] FetcherRequest<NodeId, BlockByHeight>),

    /// Deploy fetcher request.
    #[from]
    DeployFetcherRequest(#[serde(skip_serializing)] FetcherRequest<NodeId, Deploy>),

    /// Block validation request.
    #[from]
    BlockValidatorRequest(#[serde(skip_serializing)] BlockValidationRequest<Block, NodeId>),

    /// Block executor request.
    #[from]
    BlockExecutorRequest(#[serde(skip_serializing)] BlockExecutorRequest),

    /// Block proposer request.
    #[from]
    BlockProposerRequest(#[serde(skip_serializing)] BlockProposerRequest),

    /// Proto block validator request.
    #[from]
    ProtoBlockValidatorRequest(
        #[serde(skip_serializing)] BlockValidationRequest<ProtoBlock, NodeId>,
    ),

    /// Request for state storage.
    #[from]
    StateStoreRequest(StateStoreRequest),

    // Announcements
    /// Network announcement.
    #[from]
    NetworkAnnouncement(#[serde(skip_serializing)] NetworkAnnouncement<NodeId, Message>),

    /// Block executor announcement.
    #[from]
    BlockExecutorAnnouncement(#[serde(skip_serializing)] BlockExecutorAnnouncement),

    /// Consensus announcement.
    #[from]
    ConsensusAnnouncement(#[serde(skip_serializing)] ConsensusAnnouncement<NodeId>),

    /// Address Gossiper announcement.
    #[from]
    AddressGossiperAnnouncement(#[serde(skip_serializing)] GossiperAnnouncement<GossipedAddress>),

    /// DeployAcceptor announcement.
    #[from]
    DeployAcceptorAnnouncement(#[serde(skip_serializing)] DeployAcceptorAnnouncement<NodeId>),

    /// Linear chain announcement.
    #[from]
    LinearChainAnnouncement(#[serde(skip_serializing)] LinearChainAnnouncement),

    /// Chainspec loader announcement.
    #[from]
    ChainspecLoaderAnnouncement(#[serde(skip_serializing)] ChainspecLoaderAnnouncement),
}

impl From<LinearChainRequest<NodeId>> for Event {
    fn from(req: LinearChainRequest<NodeId>) -> Self {
        Event::LinearChain(linear_chain::Event::Request(req))
    }
}

impl From<StorageRequest> for Event {
    fn from(request: StorageRequest) -> Self {
        Event::Storage(request.into())
    }
}

impl From<NetworkRequest<NodeId, Message>> for Event {
    fn from(request: NetworkRequest<NodeId, Message>) -> Self {
        if env::var(ENABLE_LIBP2P_NET_ENV_VAR).is_ok() {
            Event::Network(network::Event::from(request))
        } else {
            Event::SmallNetwork(small_network::Event::from(request))
        }
    }
}

impl From<NetworkRequest<NodeId, gossiper::Message<GossipedAddress>>> for Event {
    fn from(request: NetworkRequest<NodeId, gossiper::Message<GossipedAddress>>) -> Self {
        Event::SmallNetwork(small_network::Event::from(
            request.map_payload(Message::from),
        ))
    }
}

impl From<ContractRuntimeRequest> for Event {
    fn from(request: ContractRuntimeRequest) -> Event {
        Event::ContractRuntime(contract_runtime::Event::Request(request))
    }
}

impl From<ConsensusRequest> for Event {
    fn from(request: ConsensusRequest) -> Self {
        Event::Consensus(consensus::Event::ConsensusRequest(request))
    }
}

impl From<RestRequest<NodeId>> for Event {
    fn from(request: RestRequest<NodeId>) -> Self {
        Event::RestServer(rest_server::Event::RestRequest(request))
    }
}

impl Display for Event {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Event::Network(event) => write!(f, "network: {}", event),
            Event::SmallNetwork(event) => write!(f, "small network: {}", event),
            Event::NetworkAnnouncement(event) => write!(f, "network announcement: {}", event),
            Event::Storage(request) => write!(f, "storage: {}", request),
            Event::RestServer(event) => write!(f, "rest server: {}", event),
            Event::EventStreamServer(event) => write!(f, "event stream server: {}", event),
            Event::MetricsRequest(req) => write!(f, "metrics request: {}", req),
            Event::ChainspecLoader(event) => write!(f, "chainspec loader: {}", event),
            Event::ChainspecLoaderRequest(req) => write!(f, "chainspec loader request: {}", req),
            Event::NetworkInfoRequest(req) => write!(f, "network info request: {}", req),
            Event::BlockFetcherRequest(request) => write!(f, "block fetcher request: {}", request),
            Event::BlockValidatorRequest(request) => {
                write!(f, "block validator request: {}", request)
            }
            Event::DeployFetcherRequest(request) => {
                write!(f, "deploy fetcher request: {}", request)
            }
            Event::LinearChainSync(event) => write!(f, "linear chain: {}", event),
            Event::BlockFetcher(event) => write!(f, "block fetcher: {}", event),
            Event::BlockByHeightFetcherRequest(request) => {
                write!(f, "block by height fetcher request: {}", request)
            }
            Event::BlockValidator(event) => write!(f, "block validator event: {}", event),
            Event::DeployFetcher(event) => write!(f, "deploy fetcher event: {}", event),
            Event::BlockExecutor(event) => write!(f, "block executor event: {}", event),
            Event::BlockExecutorRequest(request) => {
                write!(f, "block executor request: {}", request)
            }
            Event::BlockProposerRequest(req) => write!(f, "block proposer request: {}", req),
            Event::ContractRuntime(event) => write!(f, "contract runtime event: {}", event),
            Event::LinearChain(event) => write!(f, "linear chain event: {}", event),
            Event::BlockExecutorAnnouncement(announcement) => {
                write!(f, "block executor announcement: {}", announcement)
            }
            Event::Consensus(event) => write!(f, "consensus event: {}", event),
            Event::ConsensusAnnouncement(ann) => write!(f, "consensus announcement: {}", ann),
            Event::ProtoBlockValidatorRequest(req) => write!(f, "block validator request: {}", req),
            Event::AddressGossiper(event) => write!(f, "address gossiper: {}", event),
            Event::AddressGossiperAnnouncement(ann) => {
                write!(f, "address gossiper announcement: {}", ann)
            }
            Event::BlockByHeightFetcher(event) => {
                write!(f, "block by height fetcher event: {}", event)
            }
            Event::DeployAcceptorAnnouncement(ann) => {
                write!(f, "deploy acceptor announcement: {}", ann)
            }
            Event::DeployAcceptor(event) => write!(f, "deploy acceptor: {}", event),
            Event::LinearChainAnnouncement(ann) => write!(f, "linear chain announcement: {}", ann),
            Event::ChainspecLoaderAnnouncement(ann) => {
                write!(f, "chainspec loader announcement: {}", ann)
            }
            Event::StateStoreRequest(req) => write!(f, "state store request: {}", req),
            Event::TrieFetcher(trie) => {
                write!(f, "trie fetcher event: {}", trie)
            }
            Event::TrieFetcherRequest(req) => {
                write!(f, "trie fetcher request: {}", req)
            }
        }
    }
}

/// Joining node reactor.
#[derive(DataSize)]
pub struct Reactor {
    pub(super) metrics: Metrics,
    pub(super) network: Network<Event, Message>,
    pub(super) small_network: SmallNetwork<Event, Message>,
    pub(super) address_gossiper: Gossiper<GossipedAddress, Event>,
    pub(super) config: validator::Config,
    pub(super) chainspec_loader: ChainspecLoader,
    pub(super) storage: Storage,
    pub(super) contract_runtime: ContractRuntime,
    pub(super) linear_chain_fetcher: Fetcher<Block>,
    pub(super) linear_chain_sync: LinearChainSync<NodeId>,
    pub(super) block_validator: BlockValidator<Block, NodeId>,
    pub(super) deploy_fetcher: Fetcher<Deploy>,
    pub(super) block_executor: BlockExecutor,
    pub(super) linear_chain: linear_chain::LinearChain<NodeId>,
    pub(super) consensus: EraSupervisor<NodeId>,
    // Effects consensus component returned during creation.
    // In the `joining` phase we don't want to handle it,
    // so we carry them forward to the `validator` reactor.
    #[data_size(skip)]
    // Unfortunately, we have no way of inspecting the future and its heap allocations at all.
    pub(super) init_consensus_effects: Effects<consensus::Event<NodeId>>,
    // Handles request for linear chain block by height.
    pub(super) block_by_height_fetcher: Fetcher<BlockByHeight>,
    // Handles request for fetching tries from the network.
    pub(super) trie_fetcher: Fetcher<Trie<Key, StoredValue>>,
    #[data_size(skip)]
    pub(super) deploy_acceptor: DeployAcceptor,
    #[data_size(skip)]
    event_queue_metrics: EventQueueMetrics,
    #[data_size(skip)]
    pub(super) rest_server: RestServer,
    #[data_size(skip)]
    pub(super) event_stream_server: EventStreamServer,
    // Attach memory metrics for the joiner.
    #[data_size(skip)] // Never allocates data on the heap.
    pub(super) memory_metrics: MemoryMetrics,
}

impl reactor::Reactor for Reactor {
    type Event = Event;

    // The "configuration" is in fact the whole state of the initializer reactor, which we
    // deconstruct and reuse.
    type Config = WithDir<initializer::Reactor>;
    type Error = Error;

    fn new(
        initializer: Self::Config,
        registry: &Registry,
        event_queue: EventQueueHandle<Self::Event>,
        rng: &mut NodeRng,
    ) -> Result<(Self, Effects<Self::Event>), Self::Error> {
        let (root, initializer) = initializer.into_parts();

        let initializer::Reactor {
            config,
            chainspec_loader,
            storage,
            contract_runtime,
            small_network_identity,
            network_identity,
        } = initializer;

        // TODO: Remove wrapper around Reactor::Config instead.
        let (_, config) = config.into_parts();

        let memory_metrics = MemoryMetrics::new(registry.clone())?;

        let event_queue_metrics = EventQueueMetrics::new(registry.clone(), event_queue)?;

        let metrics = Metrics::new(registry.clone());

        let network_config = network::Config::from(&config.network);
        let (network, network_effects) = Network::new(
            event_queue,
            network_config,
            network_identity,
            chainspec_loader.chainspec(),
            false,
        )?;
        let network_name = chainspec_loader.chainspec().network_config.name.clone();
        let (small_network, small_network_effects) = SmallNetwork::new(
            event_queue,
            config.network.clone(),
            registry,
            small_network_identity,
            network_name,
            false,
        )?;

        let linear_chain_fetcher = Fetcher::new("linear_chain", config.fetcher, &registry)?;
        let trie_fetcher: Fetcher<Trie<Key, StoredValue>> =
            Fetcher::new("trie_fetcher", config.fetcher, &registry)?;

        let mut effects = reactor::wrap_effects(Event::Network, network_effects);
        effects.extend(reactor::wrap_effects(
            Event::SmallNetwork,
            small_network_effects,
        ));

        let address_gossiper =
            Gossiper::new_for_complete_items("address_gossiper", config.gossip, registry)?;

        let effect_builder = EffectBuilder::new(event_queue);

        let init_hash = config.node.trusted_hash;

        match init_hash {
            None => {
                let chainspec = chainspec_loader.chainspec();
                let era_duration = chainspec.core_config.era_duration;
                if Timestamp::now() > chainspec.network_config.timestamp + era_duration {
                    error!(
                        "Node started with no trusted hash after the expected end of \
                         the genesis era! Please specify a trusted hash and restart. \
                         Time: {}, End of genesis era: {}",
                        Timestamp::now(),
                        chainspec.network_config.timestamp + era_duration
                    );
                    panic!("should have trusted hash after genesis era")
                }
                info!("No synchronization of the linear chain will be done.")
            }
            Some(hash) => info!("Synchronizing linear chain from: {:?}", hash),
        }

        let rest_server = RestServer::new(config.rest_server.clone(), effect_builder)?;

        let event_stream_server =
            EventStreamServer::new(config.event_stream_server.clone(), effect_builder)?;

        let (block_validator, block_validator_effects) = BlockValidator::new(effect_builder);
        effects.extend(reactor::wrap_effects(
            Event::BlockValidator,
            block_validator_effects,
        ));

        let deploy_fetcher = Fetcher::new("deploy", config.fetcher, &registry)?;

        let block_by_height_fetcher = Fetcher::new("block_by_height", config.fetcher, &registry)?;

        let deploy_acceptor = DeployAcceptor::new(config.deploy_acceptor);

        let genesis_state_root_hash = chainspec_loader
            .genesis_state_root_hash()
            .expect("Should have Genesis state root hash");

        let block_executor = BlockExecutor::new(genesis_state_root_hash, registry.clone());

        let linear_chain = linear_chain::LinearChain::new(&registry)?;

        let validator_weights: BTreeMap<PublicKey, U512> = chainspec_loader
            .chainspec()
            .network_config
            .chainspec_validator_stakes()
            .into_iter()
            .map(|(pk, motes)| (pk, motes.value()))
            .collect();

        let linear_chain_sync = LinearChainSync::new::<Error>(
            registry,
            chainspec_loader.chainspec(),
            &storage,
            init_hash,
            validator_weights.clone(),
        )?;

        // Used to decide whether era should be activated.
        let timestamp = Timestamp::now();

        let (consensus, init_consensus_effects) = EraSupervisor::new(
            timestamp,
            WithDir::new(root, config.consensus.clone()),
            effect_builder,
            validator_weights,
            chainspec_loader.chainspec().into(),
            chainspec_loader
                .genesis_state_root_hash()
                .expect("should have genesis post state hash"),
            registry,
            Box::new(HighwayProtocol::new_boxed),
            rng,
        )?;

        Ok((
            Self {
                metrics,
                network,
                small_network,
                address_gossiper,
                config,
                chainspec_loader,
                storage,
                contract_runtime,
                linear_chain_sync,
                linear_chain_fetcher,
                trie_fetcher,
                block_validator,
                deploy_fetcher,
                block_executor,
                linear_chain,
                consensus,
                init_consensus_effects,
                block_by_height_fetcher,
                deploy_acceptor,
                event_queue_metrics,
                rest_server,
                event_stream_server,
                memory_metrics,
            },
            effects,
        ))
    }

    fn dispatch_event(
        &mut self,
        effect_builder: EffectBuilder<Self::Event>,
        rng: &mut NodeRng,
        event: Self::Event,
    ) -> Effects<Self::Event> {
        match event {
            Event::Network(event) => reactor::wrap_effects(
                Event::Network,
                self.network.handle_event(effect_builder, rng, event),
            ),
            Event::SmallNetwork(event) => reactor::wrap_effects(
                Event::SmallNetwork,
                self.small_network.handle_event(effect_builder, rng, event),
            ),
            Event::NetworkAnnouncement(NetworkAnnouncement::NewPeer(id)) => reactor::wrap_effects(
                Event::LinearChainSync,
                self.linear_chain_sync.handle_event(
                    effect_builder,
                    rng,
                    linear_chain_sync::Event::NewPeerConnected(id),
                ),
            ),
            Event::NetworkAnnouncement(NetworkAnnouncement::GossipOurAddress(gossiped_address)) => {
                let event = gossiper::Event::ItemReceived {
                    item_id: gossiped_address,
                    source: Source::<NodeId>::Client,
                };
                self.dispatch_event(effect_builder, rng, Event::AddressGossiper(event))
            }
            Event::NetworkAnnouncement(NetworkAnnouncement::MessageReceived {
                sender,
                payload,
            }) => match payload {
                Message::GetResponse {
                    tag: Tag::Block,
                    serialized_item,
                } => {
                    let block = match bincode::deserialize(&serialized_item) {
                        Ok(block) => Box::new(block),
                        Err(err) => {
                            error!("failed to decode block from {}: {}", sender, err);
                            return Effects::new();
                        }
                    };
                    let event = fetcher::Event::GotRemotely {
                        item: block,
                        source: Source::Peer(sender),
                    };
                    self.dispatch_event(effect_builder, rng, Event::BlockFetcher(event))
                }
                Message::GetResponse {
                    tag: Tag::BlockByHeight,
                    serialized_item,
                } => {
                    let block_at_height: BlockByHeight =
                        match bincode::deserialize(&serialized_item) {
                            Ok(maybe_block) => maybe_block,
                            Err(err) => {
                                error!("failed to decode block from {}: {}", sender, err);
                                return Effects::new();
                            }
                        };

                    let event = match block_at_height {
                        BlockByHeight::Absent(block_height) => fetcher::Event::AbsentRemotely {
                            id: block_height,
                            peer: sender,
                        },
                        BlockByHeight::Block(block) => fetcher::Event::GotRemotely {
                            item: Box::new(BlockByHeight::Block(block)),
                            source: Source::Peer(sender),
                        },
                    };
                    self.dispatch_event(effect_builder, rng, Event::BlockByHeightFetcher(event))
                }
                Message::GetResponse {
                    tag: Tag::Trie,
                    serialized_item,
                } => {
                    let trie: Box<Trie<Key, StoredValue>> =
                        match bincode::deserialize(&serialized_item) {
                            Ok(trie) => Box::new(trie),
                            Err(err) => {
                                error!("failed to decode trie from {}: {}", sender, err);
                                return Effects::new();
                            }
                        };
                    let event = fetcher::Event::GotRemotely {
                        item: trie,
                        source: Source::Peer(sender),
                    };
                    self.dispatch_event(effect_builder, rng, Event::TrieFetcher(event))
                }
                Message::GetResponse {
                    tag: Tag::Deploy,
                    serialized_item,
                } => {
                    let deploy = match bincode::deserialize(&serialized_item) {
                        Ok(deploy) => Box::new(deploy),
                        Err(err) => {
                            error!("failed to decode deploy from {}: {}", sender, err);
                            return Effects::new();
                        }
                    };
                    let event = Event::DeployAcceptor(deploy_acceptor::Event::Accept {
                        deploy,
                        source: Source::Peer(sender),
                        responder: None,
                    });
                    self.dispatch_event(effect_builder, rng, event)
                }
                Message::AddressGossiper(message) => {
                    let event = Event::AddressGossiper(gossiper::Event::MessageReceived {
                        sender,
                        message,
                    });
                    self.dispatch_event(effect_builder, rng, event)
                }
                Message::FinalitySignature(_) => {
                    debug!("finality signatures not handled in joiner reactor");
                    Effects::new()
                }
                other => {
                    debug!(?other, "network announcement ignored.");
                    Effects::new()
                }
            },
            Event::DeployAcceptorAnnouncement(DeployAcceptorAnnouncement::AcceptedNewDeploy {
                deploy,
                source,
            }) => {
                let event = fetcher::Event::GotRemotely {
                    item: deploy,
                    source,
                };
                self.dispatch_event(effect_builder, rng, Event::DeployFetcher(event))
            }
            Event::DeployAcceptorAnnouncement(DeployAcceptorAnnouncement::InvalidDeploy {
                deploy,
                source,
            }) => {
                let deploy_hash = *deploy.id();
                let peer = source;
                warn!(?deploy_hash, ?peer, "Invalid deploy received from a peer.");
                Effects::new()
            }
            Event::Storage(event) => reactor::wrap_effects(
                Event::Storage,
                self.storage.handle_event(effect_builder, rng, event),
            ),
            Event::BlockFetcherRequest(request) => {
                self.dispatch_event(effect_builder, rng, Event::BlockFetcher(request.into()))
            }
            Event::BlockValidatorRequest(request) => {
                self.dispatch_event(effect_builder, rng, Event::BlockValidator(request.into()))
            }
            Event::DeployAcceptor(event) => reactor::wrap_effects(
                Event::DeployAcceptor,
                self.deploy_acceptor
                    .handle_event(effect_builder, rng, event),
            ),
            Event::LinearChainSync(event) => reactor::wrap_effects(
                Event::LinearChainSync,
                self.linear_chain_sync
                    .handle_event(effect_builder, rng, event),
            ),
            Event::BlockFetcher(event) => reactor::wrap_effects(
                Event::BlockFetcher,
                self.linear_chain_fetcher
                    .handle_event(effect_builder, rng, event),
            ),
            Event::BlockValidator(event) => reactor::wrap_effects(
                Event::BlockValidator,
                self.block_validator
                    .handle_event(effect_builder, rng, event),
            ),
            Event::DeployFetcher(event) => reactor::wrap_effects(
                Event::DeployFetcher,
                self.deploy_fetcher.handle_event(effect_builder, rng, event),
            ),
            Event::BlockByHeightFetcher(event) => reactor::wrap_effects(
                Event::BlockByHeightFetcher,
                self.block_by_height_fetcher
                    .handle_event(effect_builder, rng, event),
            ),
            Event::TrieFetcher(event) => reactor::wrap_effects(
                Event::TrieFetcher,
                self.trie_fetcher.handle_event(effect_builder, rng, event),
            ),
            Event::TrieFetcherRequest(request) => {
                self.dispatch_event(effect_builder, rng, Event::TrieFetcher(request.into()))
            }
            Event::DeployFetcherRequest(request) => {
                self.dispatch_event(effect_builder, rng, Event::DeployFetcher(request.into()))
            }
            Event::BlockByHeightFetcherRequest(request) => self.dispatch_event(
                effect_builder,
                rng,
                Event::BlockByHeightFetcher(request.into()),
            ),
            Event::BlockExecutor(event) => reactor::wrap_effects(
                Event::BlockExecutor,
                self.block_executor.handle_event(effect_builder, rng, event),
            ),
            Event::BlockExecutorRequest(request) => {
                self.dispatch_event(effect_builder, rng, Event::BlockExecutor(request.into()))
            }
            Event::ContractRuntime(event) => reactor::wrap_effects(
                Event::ContractRuntime,
                self.contract_runtime
                    .handle_event(effect_builder, rng, event),
            ),
            Event::BlockExecutorAnnouncement(BlockExecutorAnnouncement::LinearChainBlock {
                block,
                execution_results,
            }) => {
                let mut effects = Effects::new();
                let block_hash = *block.hash();

                // send to linear chain
                let reactor_event = Event::LinearChain(linear_chain::Event::LinearChainBlock {
                    block: Box::new(block),
                    execution_results: execution_results
                        .iter()
                        .map(|(hash, (_header, results))| (*hash, results.clone()))
                        .collect(),
                });
                effects.extend(self.dispatch_event(effect_builder, rng, reactor_event));

                // send to event stream
                for (deploy_hash, (deploy_header, execution_result)) in execution_results {
                    let reactor_event =
                        Event::EventStreamServer(event_stream_server::Event::DeployProcessed {
                            deploy_hash,
                            deploy_header: Box::new(deploy_header),
                            block_hash,
                            execution_result: Box::new(execution_result),
                        });
                    effects.extend(self.dispatch_event(effect_builder, rng, reactor_event));
                }

                effects
            }
            Event::LinearChain(event) => reactor::wrap_effects(
                Event::LinearChain,
                self.linear_chain.handle_event(effect_builder, rng, event),
            ),
            Event::Consensus(event) => reactor::wrap_effects(
                Event::Consensus,
                self.consensus.handle_event(effect_builder, rng, event),
            ),
            Event::ConsensusAnnouncement(announcement) => match announcement {
                ConsensusAnnouncement::Handled(block) => {
                    let mut effects = Effects::new();
                    let reactor_event =
                        Event::LinearChainSync(linear_chain_sync::Event::BlockHandled(block));
                    effects.extend(self.dispatch_event(effect_builder, rng, reactor_event));
                    let reactor_event =
                        Event::ChainspecLoader(chainspec_loader::Event::CheckForNextUpgrade);
                    effects.extend(self.dispatch_event(effect_builder, rng, reactor_event));
                    effects
                }
                ConsensusAnnouncement::Finalized(_) => {
                    // A block was finalized.
                    Effects::new()
                }
                ConsensusAnnouncement::Fault {
                    era_id,
                    public_key,
                    timestamp,
                } => reactor::wrap_effects(
                    Event::EventStreamServer,
                    self.event_stream_server.handle_event(
                        effect_builder,
                        rng,
                        event_stream_server::Event::Fault {
                            era_id,
                            public_key: *public_key,
                            timestamp,
                        },
                    ),
                ),
                ConsensusAnnouncement::DisconnectFromPeer(_peer) => {
                    // TODO: handle the announcement and acutally disconnect
                    warn!("disconnecting from a given peer not yet implemented.");
                    Effects::new()
                }
            },
            Event::BlockProposerRequest(request) => {
                // Consensus component should not be trying to create new blocks during joining
                // phase.
                error!("ignoring block proposer request {}", request);
                Effects::new()
            }
            Event::ProtoBlockValidatorRequest(request) => {
                // During joining phase, consensus component should not be requesting
                // validation of the proto block.
                error!("ignoring proto block validation request {}", request);
                Effects::new()
            }
            Event::AddressGossiper(event) => reactor::wrap_effects(
                Event::AddressGossiper,
                self.address_gossiper
                    .handle_event(effect_builder, rng, event),
            ),
            Event::AddressGossiperAnnouncement(ann) => {
                let GossiperAnnouncement::NewCompleteItem(gossiped_address) = ann;
                let reactor_event = Event::SmallNetwork(small_network::Event::PeerAddressReceived(
                    gossiped_address,
                ));
                self.dispatch_event(effect_builder, rng, reactor_event)
            }

            Event::LinearChainAnnouncement(LinearChainAnnouncement::BlockAdded {
                block_hash,
                block_header,
            }) => reactor::wrap_effects(
                Event::EventStreamServer,
                self.event_stream_server.handle_event(
                    effect_builder,
                    rng,
                    event_stream_server::Event::BlockAdded {
                        block_hash,
                        block_header,
                    },
                ),
            ),
            Event::LinearChainAnnouncement(LinearChainAnnouncement::NewFinalitySignature(fs)) => {
                let reactor_event =
                    Event::EventStreamServer(event_stream_server::Event::FinalitySignature(fs));
                self.dispatch_event(effect_builder, rng, reactor_event)
            }
            Event::RestServer(event) => reactor::wrap_effects(
                Event::RestServer,
                self.rest_server.handle_event(effect_builder, rng, event),
            ),
            Event::EventStreamServer(event) => reactor::wrap_effects(
                Event::EventStreamServer,
                self.event_stream_server
                    .handle_event(effect_builder, rng, event),
            ),
            Event::MetricsRequest(req) => reactor::wrap_effects(
                Event::MetricsRequest,
                self.metrics.handle_event(effect_builder, rng, req),
            ),
            Event::ChainspecLoader(event) => reactor::wrap_effects(
                Event::ChainspecLoader,
                self.chainspec_loader
                    .handle_event(effect_builder, rng, event),
            ),
            Event::ChainspecLoaderRequest(req) => {
                self.dispatch_event(effect_builder, rng, Event::ChainspecLoader(req.into()))
            }
            Event::NetworkInfoRequest(req) => {
                let event = if env::var(ENABLE_LIBP2P_NET_ENV_VAR).is_ok() {
                    Event::Network(network::Event::from(req))
                } else {
                    Event::SmallNetwork(small_network::Event::from(req))
                };
                self.dispatch_event(effect_builder, rng, event)
            }
            Event::ChainspecLoaderAnnouncement(
                ChainspecLoaderAnnouncement::UpgradeActivationPointRead(next_upgrade),
            ) => {
                // TODO: Clean me up once fast-sync is stable
                let mut effects = Effects::new();

                #[cfg(not(feature = "fast-sync"))]
                effects.extend(self.dispatch_event(
                    effect_builder,
                    rng,
                    Event::LinearChainSync(linear_chain_sync::Event::GotUpgradeActivationPoint(
                        next_upgrade.clone().activation_point(),
                    )),
                ));

                let reactor_event =
                    Event::ChainspecLoader(chainspec_loader::Event::GotNextUpgrade(next_upgrade));
                effects.extend(self.dispatch_event(effect_builder, rng, reactor_event));

                effects
            }
            Event::StateStoreRequest(req) => {
                self.dispatch_event(effect_builder, rng, Event::Storage(req.into()))
            }
        }
    }

    fn is_stopped(&mut self) -> bool {
        self.linear_chain_sync.is_synced()
    }

    // TODO: clean me up when fast-sync is stable
    #[cfg(not(feature = "fast-sync"))]
    fn needs_upgrade(&mut self) -> bool {
        self.linear_chain_sync.stopped_for_upgrade()
    }

    fn update_metrics(&mut self, event_queue_handle: EventQueueHandle<Self::Event>) {
        self.memory_metrics.estimate(&self);
        self.event_queue_metrics
            .record_event_queue_counts(&event_queue_handle);
    }
}

impl Reactor {
    /// Deconstructs the reactor into config useful for creating a Validator reactor. Shuts down
    /// the network, closing all incoming and outgoing connections, and frees up the listening
    /// socket.
    pub async fn into_validator_config(self) -> Result<ValidatorInitConfig, Error> {
        // Clean the state of the linear_chain_sync before shutting it down.
        #[cfg(not(feature = "fast-sync"))]
        linear_chain_sync::clean_linear_chain_state(
            &self.storage,
            self.chainspec_loader.chainspec(),
        )?;
        let config = ValidatorInitConfig {
            chainspec_loader: self.chainspec_loader,
            config: self.config,
            contract_runtime: self.contract_runtime,
            storage: self.storage,
            consensus: self.consensus,
            init_consensus_effects: self.init_consensus_effects,
            latest_block: self.linear_chain.latest_block().clone(),
            event_stream_server: self.event_stream_server,
            small_network_identity: SmallNetworkIdentity::from(&self.small_network),
            network_identity: NetworkIdentity::from(&self.network),
        };
        self.network.finalize().await;
        self.small_network.finalize().await;
        self.rest_server.finalize().await;
        Ok(config)
    }
}

#[cfg(test)]
impl NetworkedReactor for Reactor {
    type NodeId = NodeId;
    fn node_id(&self) -> Self::NodeId {
        if env::var(ENABLE_LIBP2P_NET_ENV_VAR).is_err() {
            self.small_network.node_id()
        } else {
            self.network.node_id()
        }
    }
}

#[cfg(test)]
impl Reactor {
    /// Inspect consensus.
    pub(crate) fn consensus(&self) -> &EraSupervisor<NodeId> {
        &self.consensus
    }
    /// Inspect storage.
    pub(crate) fn storage(&self) -> &Storage {
        &self.storage
    }
}
