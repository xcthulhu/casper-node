use std::collections::VecDeque;

use datasize::DataSize;
use rand::{seq::SliceRandom, Rng};

const MAX_SUCCESSFUL_DATA_RETRIEVALS: u8 = 5;

#[derive(DataSize, Debug)]
pub struct PeersState<I> {
    /// Set of peers that we know of.
    peers_we_know_of: Vec<I>,

    /// Peers we have not yet requested data from.
    candidate_peers_to_try_to_get_data_from: Vec<I>,

    /// Peers we have previously successfully downloaded data from.
    /// These peers are preferred when trying to retrieve data from next.
    peers_we_have_previously_had_success_with: VecDeque<I>,

    /// A count of the number of times we have recently successfully downloaded data from a peer.
    number_of_recent_successful_retrievals_from_peers: u8,
}

impl<I: Clone + PartialEq + 'static> PeersState<I> {
    pub(crate) fn new() -> Self {
        PeersState {
            peers_we_know_of: Default::default(),
            candidate_peers_to_try_to_get_data_from: Default::default(),
            peers_we_have_previously_had_success_with: Default::default(),
            number_of_recent_successful_retrievals_from_peers: 0,
        }
    }

    /// Resets `peers_to_try` back to all `peers` we know of.
    pub(crate) fn reset_peers_to_try<R: Rng + ?Sized>(&mut self, rng: &mut R) {
        self.candidate_peers_to_try_to_get_data_from =
            self.peers_we_know_of.iter().cloned().collect();
        self.candidate_peers_to_try_to_get_data_from
            .as_mut_slice()
            .shuffle(rng);
    }

    /// Returns either a peer we have previously been successful getting data from,
    /// or a peer we have not tried yet.
    pub(crate) fn get_peer(&mut self) -> Option<I> {
        // We generally prefer peers we have had success with.
        // However, we don't want to DOS peers we are getting data from, so we periodically try a
        // new peer.  This happens after `MAX_SUCCESSFUL_DATA_RETRIEVALS`.
        if self.number_of_recent_successful_retrievals_from_peers < MAX_SUCCESSFUL_DATA_RETRIEVALS {
            self.get_peer_with_have_previously_had_success_with()
                .or_else(|| self.candidate_peers_to_try_to_get_data_from.pop())
        } else {
            self.number_of_recent_successful_retrievals_from_peers = 0;
            self.candidate_peers_to_try_to_get_data_from
                .pop()
                .or_else(|| self.get_peer_with_have_previously_had_success_with())
        }
    }

    /// Unsafe version of `random_peer`.
    /// Panics if no peer is available for querying.
    pub(crate) fn get_peer_unsafe(&mut self) -> I {
        self.get_peer().expect("At least one peer available.")
    }

    /// Peer misbehaved (returned us invalid data).
    /// Remove it from the set of nodes we request data from.
    pub(crate) fn ban(&mut self, peer: &I) {
        self.peers_we_know_of.retain(|p| p != peer);
        self.candidate_peers_to_try_to_get_data_from
            .retain(|p| p != peer);
        self.peers_we_have_previously_had_success_with
            .retain(|p| p != peer);
    }

    /// Add a new peer.
    pub(crate) fn add_peer(&mut self, peer: I) {
        if !self.peers_we_know_of.contains(&peer) {
            self.peers_we_know_of.push(peer.clone());
        }
        if !self.candidate_peers_to_try_to_get_data_from.contains(&peer) {
            self.candidate_peers_to_try_to_get_data_from.push(peer);
        }
    }

    /// Returns a peer, if any, that we have downloaded data from previously.
    /// Peer will be shuffled back in the set of `peers_we_have_previously_had_success_with`.
    fn get_peer_with_have_previously_had_success_with(&mut self) -> Option<I> {
        let peer = self.peers_we_have_previously_had_success_with.pop_front()?;
        self.peers_we_have_previously_had_success_with
            .push_back(peer.clone());
        Some(peer)
    }

    /// Peer did not respond or did not have the data we asked for.
    pub(crate) fn mark_peer_did_not_have_data(&mut self, peer: &I) {
        self.peers_we_have_previously_had_success_with
            .retain(|id| id != peer);
    }

    /// Peer had the data we asked for, so add them to the list of peers we had success with.
    pub(crate) fn mark_peer_as_had_success_with(&mut self, peer: I) {
        self.number_of_recent_successful_retrievals_from_peers += 1;
        self.peers_we_have_previously_had_success_with
            .push_back(peer);
    }
}
