use std::iter;

use casper_execution_engine::shared::motes::Motes;

use crate::{
    components::consensus::{
        cl_context::ClContext,
        consensus_protocol::{ConsensusProtocol, ProtocolOutcome},
        highway_core::highway_testing::{new_test_chainspec, ALICE_PUBLIC_KEY, BOB_PUBLIC_KEY},
        tests::mock_proto::NodeId,
        traits::Context,
        HighwayProtocol,
    },
    testing::TestRng,
};

#[test]
fn test() {
    // Build a highway_protocol for instrumentation
    let mut highway_protocol: Box<dyn ConsensusProtocol<NodeId, ClContext>> = {
        let chainspec = new_test_chainspec(vec![(*ALICE_PUBLIC_KEY, 100)]);

        HighwayProtocol::<NodeId, ClContext>::new_boxed(
            ClContext::hash(&[123]),
            vec![
                (*ALICE_PUBLIC_KEY, Motes::new(100.into())),
                (*BOB_PUBLIC_KEY, Motes::new(10.into())),
            ],
            &iter::once(*BOB_PUBLIC_KEY).collect(),
            &chainspec,
            None,
            0.into(),
            0,
        )
    };

    let sender = NodeId(123);
    let msg = vec![];
    let mut rng = TestRng::new();
    let mut effects: Vec<ProtocolOutcome<NodeId, ClContext>> =
        highway_protocol.handle_message(sender.to_owned(), msg.to_owned(), false, &mut rng);

    assert_eq!(effects.len(), 1);

    let opt_protocol_outcome = effects.pop();

    match &opt_protocol_outcome {
        None => panic!("We just checked that effects has length 1!"),
        Some(ProtocolOutcome::InvalidIncomingMessage(invalid_msg, offending_sender, _err)) => {
            assert_eq!(
                invalid_msg, &msg,
                "Invalid message is not message that was sent."
            );
            assert_eq!(offending_sender, &sender, "Unexpected sender.")
        }
        Some(protocol_outcome) => panic!("Unexpected protocol outcome {:?}", protocol_outcome),
    }
}
