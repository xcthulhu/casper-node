use serde::{Deserialize, Serialize};

use casper_execution_engine::{shared::stored_value::StoredValue, storage::trie::merkle_proof};
use casper_types::Key;

/// TODO
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize, Debug)]
pub struct TrieMerkleProof {}

impl From<merkle_proof::TrieMerkleProof<Key, StoredValue>> for TrieMerkleProof {
    fn from(_: merkle_proof::TrieMerkleProof<Key, StoredValue>) -> Self {
        unimplemented!()
    }
}
