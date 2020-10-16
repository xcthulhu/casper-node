use std::collections::VecDeque;

use casper_types::{bytesrepr, bytesrepr::ToBytes};

use crate::{
    shared::newtypes::Blake2bHash,
    storage::trie::{Pointer, Trie, RADIX},
};

pub enum TrieMerkleProofStep {
    Node {
        hole_index: usize,
        indexed_pointers_with_hole: Vec<(usize, Pointer)>,
    },
    Extension {
        affix: Vec<u8>,
    },
}

impl TrieMerkleProofStep {
    pub fn node(hole_index: usize, indexed_pointers_with_hole: Vec<(usize, Pointer)>) -> Self {
        Self::Node {
            hole_index,
            indexed_pointers_with_hole,
        }
    }

    pub fn extension(affix: Vec<u8>) -> Self {
        Self::Extension { affix }
    }
}

pub struct TrieMerkleProof<K, V> {
    key: K,
    value: V,
    proof_steps: VecDeque<TrieMerkleProofStep>,
}

impl<K, V> TrieMerkleProof<K, V> {
    pub fn new(key: K, value: V, proof_steps: VecDeque<TrieMerkleProofStep>) -> Self {
        TrieMerkleProof {
            key,
            value,
            proof_steps,
        }
    }

    pub fn value(&self) -> &V {
        &self.value
    }
}

impl<K, V> TrieMerkleProof<K, V>
where
    K: ToBytes + Copy,
    V: ToBytes + Copy,
{
    pub fn compute_state_hash(&self) -> Result<Blake2bHash, Error> {
        let mut hash = {
            let leaf_bytes = Trie::leaf(self.key, self.value).to_bytes()?;
            Blake2bHash::new(&leaf_bytes)
        };

        let mut proof_step_index: usize = 0;
        for proof_step in &self.proof_steps {
            let pointer = if proof_step_index == 0 {
                Pointer::LeafPointer(hash)
            } else {
                Pointer::NodePointer(hash)
            };
            let proof_step_bytes = match proof_step {
                TrieMerkleProofStep::Node {
                    hole_index,
                    indexed_pointers_with_hole,
                } => {
                    let hole_index = *hole_index;
                    if hole_index > RADIX {
                        return Err(Error::HoleIndexOutOfRange {
                            proof_step_index,
                            hole_index,
                        });
                    }
                    let mut indexed_pointers = indexed_pointers_with_hole.to_owned();
                    indexed_pointers.push((hole_index, pointer));
                    Trie::<K, V>::node(&indexed_pointers).to_bytes()?
                }
                TrieMerkleProofStep::Extension { affix } => {
                    Trie::<K, V>::extension(affix.to_owned(), pointer).to_bytes()?
                }
            };
            hash = Blake2bHash::new(&proof_step_bytes);
            proof_step_index += 1
        }
        Ok(hash)
    }
}

#[derive(Debug, Clone)]
pub enum Error {
    HoleIndexOutOfRange {
        hole_index: usize,
        proof_step_index: usize,
    },
    BytesreprError(bytesrepr::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::HoleIndexOutOfRange{ proof_step_index: proof_step, hole_index} => write!(
                f,
                "`hole_index` with value {hole_index} must be less than {RADIX} (proof step {proof_step})",
                hole_index = hole_index,
                RADIX = RADIX,
                proof_step = proof_step
            ),
            Error::BytesreprError(cause) => write!(f, "Byte representation error: {}", cause),
        }
    }
}

impl std::error::Error for Error {
    fn cause(&self) -> Option<&dyn std::error::Error> {
        match self {
            Error::HoleIndexOutOfRange { .. } => None,
            Error::BytesreprError(cause) => Some(cause),
        }
    }
}

impl From<bytesrepr::Error> for Error {
    fn from(cause: bytesrepr::Error) -> Self {
        Error::BytesreprError(cause)
    }
}
