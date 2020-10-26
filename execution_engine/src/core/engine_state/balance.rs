use casper_types::{Key, URef, U512};

use crate::{
    shared::{newtypes::Blake2bHash, stored_value::StoredValue},
    storage::trie::merkle_proof::TrieMerkleProof,
};

#[derive(Debug)]
pub enum BalanceResult {
    RootNotFound,
    Success {
        motes: U512,
        main_purse_proof: Box<TrieMerkleProof<Key, StoredValue>>,
        balance_proof: Box<TrieMerkleProof<Key, StoredValue>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceRequest {
    state_hash: Blake2bHash,
    purse_uref: URef,
}

impl BalanceRequest {
    pub fn new(state_hash: Blake2bHash, purse_uref: URef) -> Self {
        BalanceRequest {
            state_hash,
            purse_uref,
        }
    }

    pub fn state_hash(&self) -> Blake2bHash {
        self.state_hash
    }

    pub fn purse_uref(&self) -> URef {
        self.purse_uref
    }
}
