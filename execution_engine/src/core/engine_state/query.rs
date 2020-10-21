use casper_types::Key;

use crate::{
    core::tracking_copy::TrackingCopyQueryResult,
    shared::{newtypes::Blake2bHash, stored_value::StoredValue},
    storage::trie::merkle_proof::TrieMerkleProof,
};

#[derive(Debug)]
pub enum QueryResult {
    RootNotFound,
    ValueNotFound(String),
    CircularReference(String),
    Success(Vec<TrieMerkleProof<Key, StoredValue>>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryRequest {
    state_hash: Blake2bHash,
    key: Key,
    path: Vec<String>,
}

impl QueryRequest {
    pub fn new(state_hash: Blake2bHash, key: Key, path: Vec<String>) -> Self {
        QueryRequest {
            state_hash,
            key,
            path,
        }
    }

    pub fn state_hash(&self) -> Blake2bHash {
        self.state_hash
    }

    pub fn key(&self) -> Key {
        self.key
    }

    pub fn path(&self) -> &[String] {
        &self.path
    }
}

impl From<TrackingCopyQueryResult> for QueryResult {
    fn from(tracking_copy_query_result: TrackingCopyQueryResult) -> Self {
        match tracking_copy_query_result {
            TrackingCopyQueryResult::ValueNotFound(message) => QueryResult::ValueNotFound(message),
            TrackingCopyQueryResult::CircularReference(message) => {
                QueryResult::CircularReference(message)
            }
            TrackingCopyQueryResult::Success(value) => QueryResult::Success(value),
        }
    }
}
