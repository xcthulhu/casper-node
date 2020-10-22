use std::convert::{TryFrom, TryInto};

use serde::{Deserialize, Serialize};

use casper_execution_engine::{shared::stored_value, storage::trie::merkle_proof};
use casper_types::{
    bytesrepr::{self, ToBytes},
    Key,
};

use super::StoredValue;

/// TODO
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize, Debug)]
pub struct TrieMerkleProof {
    key: String,
    value: StoredValue,
    proof_steps: String,
}

impl TryFrom<merkle_proof::TrieMerkleProof<Key, stored_value::StoredValue>> for TrieMerkleProof {
    type Error = bytesrepr::Error;

    fn try_from(
        ee_trie_merkle_proof: merkle_proof::TrieMerkleProof<Key, stored_value::StoredValue>,
    ) -> Result<Self, Self::Error> {
        let key = ee_trie_merkle_proof.key().to_formatted_string();
        let value = ee_trie_merkle_proof.value().try_into()?;
        let proof_steps = hex::encode(ee_trie_merkle_proof.proof_steps().to_bytes()?);
        Ok(TrieMerkleProof {
            key,
            value,
            proof_steps,
        })
    }
}
