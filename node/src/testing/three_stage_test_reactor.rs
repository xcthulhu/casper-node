pub mod test_chain;

use std::{
    fmt::{self, Display, Formatter},
    mem,
    path::PathBuf,
};

use derive_more::From;
use once_cell::sync::Lazy;
use prometheus::Registry;
use serde::Serialize;
use thiserror::Error;
use tokio::runtime::Handle;
use tracing::warn;

use crate::{
    components::consensus::EraSupervisor,
    effect::{EffectBuilder, Effects},
    reactor::{
        initializer::Reactor as InitializerReactor, joiner::Reactor as JoinerReactor,
        validator::Reactor as ValidatorReactor, wrap_effects, EventQueueHandle, QueueKind, Reactor,
        Scheduler,
    },
    testing::network::NetworkedReactor,
    types::NodeId,
    utils::{self, WithDir, RESOURCES_PATH},
    NodeRng,
};

pub static CONFIG_DIR: Lazy<PathBuf> = Lazy::new(|| RESOURCES_PATH.join("local"));

#[derive(Debug, Error)]
pub enum ThreeStageError {
    #[error("Could not make initializer reactor: {0}")]
    CouldNotMakeInitializerReactor(<InitializerReactor as Reactor>::Error),

    #[error(transparent)]
    PrometheusError(#[from] prometheus::Error),
}

#[derive(Debug, From, Serialize)]
pub enum ThreeStageTestEvent {
    #[from]
    InitializerEvent(<InitializerReactor as Reactor>::Event),
    #[from]
    JoinerEvent(<JoinerReactor as Reactor>::Event),
    #[from]
    ValidatorEvent(<ValidatorReactor as Reactor>::Event),
}

impl Display for ThreeStageTestEvent {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            ThreeStageTestEvent::InitializerEvent(ev) => {
                write!(f, "initializer event: {}", ev)
            }
            ThreeStageTestEvent::JoinerEvent(ev) => {
                write!(f, "joiner event: {}", ev)
            }
            ThreeStageTestEvent::ValidatorEvent(ev) => {
                write!(f, "validator event: {}", ev)
            }
        }
    }
}

pub(crate) enum ThreeStageTestReactor {
    Deactivated,
    Initializer {
        initializer_reactor: InitializerReactor,
        initializer_event_queue_handle: EventQueueHandle<<InitializerReactor as Reactor>::Event>,
        registry: Registry,
    },
    Joiner {
        joiner_reactor: JoinerReactor,
        joiner_event_queue_handle: EventQueueHandle<<JoinerReactor as Reactor>::Event>,
        registry: Registry,
    },
    Validator {
        validator_reactor: ValidatorReactor,
        validator_event_queue_handle: EventQueueHandle<<ValidatorReactor as Reactor>::Event>,
    },
}

impl ThreeStageTestReactor {
    pub fn consensus(&self) -> Option<&EraSupervisor<NodeId>> {
        match self {
            ThreeStageTestReactor::Deactivated => unreachable!(),
            ThreeStageTestReactor::Initializer { .. } => None,
            ThreeStageTestReactor::Joiner { joiner_reactor, .. } => {
                Some(joiner_reactor.consensus())
            }
            ThreeStageTestReactor::Validator {
                validator_reactor, ..
            } => Some(validator_reactor.consensus()),
        }
    }
}

/// Long-running task that forwards events arriving on one scheduler to another.
async fn forward_to_queue<I, O>(source: &Scheduler<I>, target_queue: EventQueueHandle<O>)
where
    O: From<I>,
{
    // Note: This will keep waiting forever if the sending end disappears, which is fine for tests.
    loop {
        let (event, queue_kind) = source.pop().await;
        target_queue.schedule(event, queue_kind).await;
    }
}

impl Reactor for ThreeStageTestReactor {
    type Event = ThreeStageTestEvent;

    type Config = <InitializerReactor as Reactor>::Config;

    type Error = ThreeStageError;

    fn new(
        config: Self::Config,
        registry: &Registry,
        event_queue: EventQueueHandle<Self::Event>,
        rng: &mut NodeRng,
    ) -> Result<(Self, Effects<Self::Event>), Self::Error> {
        let initializer_scheduler = utils::leak(Scheduler::new(QueueKind::weights()));
        let initializer_event_queue_handle: EventQueueHandle<
            <InitializerReactor as Reactor>::Event,
        > = EventQueueHandle::new(initializer_scheduler);

        tokio::spawn(forward_to_queue(initializer_scheduler, event_queue));

        let (initializer_reactor, initializer_effects) =
            InitializerReactor::new(config, &registry, initializer_event_queue_handle, rng)
                .map_err(ThreeStageError::CouldNotMakeInitializerReactor)?;

        Ok((
            ThreeStageTestReactor::Initializer {
                initializer_reactor,
                initializer_event_queue_handle,
                registry: registry.clone(),
            },
            wrap_effects(ThreeStageTestEvent::InitializerEvent, initializer_effects),
        ))
    }

    fn dispatch_event(
        &mut self,
        effect_builder: EffectBuilder<Self::Event>,
        rng: &mut NodeRng,
        event: ThreeStageTestEvent,
    ) -> Effects<Self::Event> {
        // We need to enforce node ids stay constant through state transitions
        let old_node_id = self.node_id();

        // Take ownership of self
        let mut three_stage_test_reactor = mem::replace(self, ThreeStageTestReactor::Deactivated);

        // Keep track of whether the event signalled we should do a state transition
        let mut should_transition = false;

        // Process the event
        let mut effects = match (event, &mut three_stage_test_reactor) {
            (event, ThreeStageTestReactor::Deactivated) => {
                unreachable!(
                    "Event sent to deactivated three stage test reactor: {}",
                    event
                )
            }
            (
                ThreeStageTestEvent::InitializerEvent(initializer_event),
                ThreeStageTestReactor::Initializer {
                    ref mut initializer_reactor,
                    initializer_event_queue_handle,
                    ..
                },
            ) => {
                let effect_builder = EffectBuilder::new(*initializer_event_queue_handle);

                let effects = wrap_effects(
                    ThreeStageTestEvent::InitializerEvent,
                    initializer_reactor.dispatch_event(effect_builder, rng, initializer_event),
                );

                if initializer_reactor.is_stopped() {
                    if !initializer_reactor.stopped_successfully() {
                        panic!("failed to transition from initializer to joiner");
                    }

                    should_transition = true;
                }

                effects
            }
            (
                ThreeStageTestEvent::JoinerEvent(joiner_event),
                ThreeStageTestReactor::Joiner {
                    ref mut joiner_reactor,
                    joiner_event_queue_handle,
                    ..
                },
            ) => {
                let effect_builder = EffectBuilder::new(*joiner_event_queue_handle);

                let effects = wrap_effects(
                    ThreeStageTestEvent::JoinerEvent,
                    joiner_reactor.dispatch_event(effect_builder, rng, joiner_event),
                );

                if joiner_reactor.is_stopped() {
                    should_transition = true;
                }

                effects
            }
            (
                ThreeStageTestEvent::ValidatorEvent(validator_event),
                ThreeStageTestReactor::Validator {
                    ref mut validator_reactor,
                    validator_event_queue_handle,
                    ..
                },
            ) => {
                let effect_builder = EffectBuilder::new(*validator_event_queue_handle);

                let effects = wrap_effects(
                    ThreeStageTestEvent::ValidatorEvent,
                    validator_reactor.dispatch_event(effect_builder, rng, validator_event),
                );

                if validator_reactor.is_stopped() {
                    panic!("validator reactor should never stop");
                }

                effects
            }
            (event, three_stage_test_reactor) => {
                let stage = match three_stage_test_reactor {
                    ThreeStageTestReactor::Deactivated => "Deactivated",
                    ThreeStageTestReactor::Initializer { .. } => "Initializing",
                    ThreeStageTestReactor::Joiner { .. } => "Joining",
                    ThreeStageTestReactor::Validator { .. } => "Validating",
                };

                warn!(
                    ?event,
                    ?stage,
                    "discarded event due to not being in the right stage"
                );

                // Shouldn't be reachable; otherwise do `Effects::new()`
                panic!()
            }
        };

        if should_transition {
            match three_stage_test_reactor {
                ThreeStageTestReactor::Deactivated => {
                    // We will never transition when `Deactivated`
                    unreachable!()
                }
                ThreeStageTestReactor::Initializer {
                    initializer_reactor,
                    initializer_event_queue_handle,
                    registry,
                } => {
                    let dropped_events_count = effects.len();
                    if dropped_events_count != 0 {
                        warn!("when transitioning from initializer to joiner, left {} effects unhandled", dropped_events_count)
                    }

                    assert_eq!(
                        initializer_event_queue_handle
                            .event_queues_counts()
                            .values()
                            .sum::<usize>(),
                        0,
                        "before transitioning from joiner to validator, \
                         there should be no unprocessed events"
                    );

                    let joiner_scheduler = utils::leak(Scheduler::new(QueueKind::weights()));
                    let joiner_event_queue_handle = EventQueueHandle::new(joiner_scheduler);

                    tokio::spawn(forward_to_queue(
                        joiner_scheduler,
                        effect_builder.into_inner(),
                    ));

                    let (joiner_reactor, joiner_effects) = JoinerReactor::new(
                        WithDir::new(&*CONFIG_DIR, initializer_reactor),
                        &registry,
                        joiner_event_queue_handle,
                        rng,
                    )
                    .expect("joiner initialization failed");

                    *self = ThreeStageTestReactor::Joiner {
                        joiner_reactor,
                        joiner_event_queue_handle,
                        registry,
                    };

                    effects.extend(
                        wrap_effects(ThreeStageTestEvent::JoinerEvent, joiner_effects).into_iter(),
                    )
                }
                ThreeStageTestReactor::Joiner {
                    joiner_reactor,
                    joiner_event_queue_handle,
                    registry,
                } => {
                    let dropped_events_count = effects.len();
                    if dropped_events_count != 0 {
                        warn!("when transitioning from joiner to validator, left {} effects unhandled", dropped_events_count)
                    }

                    assert_eq!(
                        joiner_event_queue_handle
                            .event_queues_counts()
                            .values()
                            .sum::<usize>(),
                        0,
                        "before transitioning from joiner to validator, \
                         there should be no unprocessed events"
                    );

                    // `into_validator_config` is just waiting for networking sockets to shut down
                    // and will not stall on disabled event processing, so it is safe to block here.
                    let validator_config = std::thread::spawn(move || {
                        let mut tokio_runtime = tokio::runtime::Runtime::new().unwrap();
                        tokio_runtime.block_on(joiner_reactor.into_validator_config())
                    })
                    .join()
                    .unwrap();

                    let validator_scheduler = utils::leak(Scheduler::new(QueueKind::weights()));
                    let validator_event_queue_handle = EventQueueHandle::new(validator_scheduler);

                    tokio::spawn(forward_to_queue(
                        validator_scheduler,
                        effect_builder.into_inner(),
                    ));

                    let (validator_reactor, validator_effects) = ValidatorReactor::new(
                        validator_config,
                        &registry,
                        validator_event_queue_handle,
                        rng,
                    )
                    .expect("validator intialization failed");

                    *self = ThreeStageTestReactor::Validator {
                        validator_reactor,
                        validator_event_queue_handle,
                    };

                    effects.extend(
                        wrap_effects(ThreeStageTestEvent::ValidatorEvent, validator_effects)
                            .into_iter(),
                    )
                }
                ThreeStageTestReactor::Validator { .. } => {
                    // Validator reactors don't transition to anything
                    unreachable!()
                }
            }
        } else {
            // The reactor's state didn't transition, so put back the reactor we seized ownership of
            *self = three_stage_test_reactor;
        }

        let new_node_id = self.node_id();
        assert_eq!(old_node_id, new_node_id);

        match self {
            ThreeStageTestReactor::Deactivated => {
                panic!("Reactor should not longer be Deactivated!")
            }
            _ => (),
        }

        effects
    }
}

impl NetworkedReactor for ThreeStageTestReactor {
    type NodeId = NodeId;
    fn node_id(&self) -> Self::NodeId {
        match self {
            ThreeStageTestReactor::Deactivated => unreachable!(),
            ThreeStageTestReactor::Initializer {
                initializer_reactor,
                ..
            } => initializer_reactor.node_id(),
            ThreeStageTestReactor::Joiner { joiner_reactor, .. } => joiner_reactor.node_id(),
            ThreeStageTestReactor::Validator {
                validator_reactor, ..
            } => validator_reactor.node_id(),
        }
    }
}
