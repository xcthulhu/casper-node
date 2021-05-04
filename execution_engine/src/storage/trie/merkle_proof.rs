use std::collections::VecDeque;

use casper_types::bytesrepr::{self, Bytes, FromBytes, ToBytes};

use crate::{
    shared::newtypes::Blake2bHash,
    storage::trie::{hash_pair, Pointer, Trie, RADIX},
};

const TRIE_MERKLE_PROOF_STEP_NODE_ID: u8 = 0;
const TRIE_MERKLE_PROOF_STEP_EXTENSION_ID: u8 = 1;
const TRIE_MERKLE_PROOF_ROSE_LEAF_ROSE_ID: u8 = 2;
const TRIE_MERKLE_PROOF_ROSE_LEAF_LEAF_ID: u8 = 3;
const TRIE_MERKLE_PROOF_ROSE_EXTENSION_ROSE_ID: u8 = 4;
const TRIE_MERKLE_PROOF_ROSE_EXTENSION_EXTENSION_ID: u8 = 5;
const TRIE_MERKLE_PROOF_ROSE_NODE_ROSE_ID: u8 = 6;
const TRIE_MERKLE_PROOF_ROSE_NODE_NODE_ID: u8 = 7;

/// A component of a proof that an entry exists in the Merkle trie.
///
/// Every "rose" variant has two proof components: one for the _rose_ itself and one for the basic
/// variant it corresponds to.
///
/// While there is no corresponding proof component for a [Trie::Leaf], there _are_ corresponding
/// proof components for [Trie::RoseLeaf].  This is because you can't prove a leaf is in the tree if
/// it is adjacent to rose and vice versa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrieMerkleProofStep {
    /// Corresponds to [Trie::Node]
    Node {
        hole_index: u8,
        indexed_pointers_with_hole: Vec<(u8, Pointer)>,
    },
    /// Corresponds to [Trie::Extension]
    Extension { affix: Bytes },
    /// Corresponds to the _rose_ in a [Trie::RoseLeaf]
    RoseLeafRose { leaf_hash: Blake2bHash },
    /// Corresponds to the _leaf_ in a [Trie::RoseLeaf]
    RoseLeafLeaf { rose_hash: Blake2bHash },
    /// Corresponds to the _rose_ in a [Trie::RoseExtension]
    RoseExtensionRose { extension_hash: Blake2bHash },
    /// Corresponds to the _extension_ in a [Trie::RoseExtension]
    RoseExtensionExtension {
        rose_hash: Blake2bHash,
        affix: Bytes,
    },
    /// Corresponds to the _rose_ in a [Trie::RoseNode]
    RoseNodeRose { node_hash: Blake2bHash },
    /// Corresponds to the _node_ in a [Trie::RoseNode]
    RoseNodeNode {
        rose_hash: Blake2bHash,
        hole_index: u8,
        indexed_pointers_with_hole: Vec<(u8, Pointer)>,
    },
}

impl TrieMerkleProofStep {
    /// Constructor for  [`TrieMerkleProofStep::Node`]
    pub fn node(hole_index: u8, indexed_pointers_with_hole: Vec<(u8, Pointer)>) -> Self {
        Self::Node {
            hole_index,
            indexed_pointers_with_hole,
        }
    }

    /// Constructor for  [`TrieMerkleProofStep::Extension`]
    pub fn extension(affix: Vec<u8>) -> Self {
        Self::Extension {
            affix: affix.into(),
        }
    }
}

impl ToBytes for TrieMerkleProofStep {
    fn to_bytes(&self) -> Result<Vec<u8>, bytesrepr::Error> {
        let mut ret: Vec<u8> = bytesrepr::allocate_buffer(self)?;
        match self {
            TrieMerkleProofStep::Node {
                hole_index,
                indexed_pointers_with_hole,
            } => {
                ret.push(TRIE_MERKLE_PROOF_STEP_NODE_ID);
                ret.push(*hole_index);
                ret.append(&mut indexed_pointers_with_hole.to_bytes()?)
            }
            TrieMerkleProofStep::Extension { affix } => {
                ret.push(TRIE_MERKLE_PROOF_STEP_EXTENSION_ID);
                ret.append(&mut affix.to_bytes()?)
            }
            TrieMerkleProofStep::RoseLeafRose { leaf_hash } => {
                ret.push(TRIE_MERKLE_PROOF_ROSE_LEAF_ROSE_ID);
                ret.append(&mut leaf_hash.to_bytes()?)
            }
            TrieMerkleProofStep::RoseLeafLeaf { rose_hash } => {
                ret.push(TRIE_MERKLE_PROOF_ROSE_LEAF_LEAF_ID);
                ret.append(&mut rose_hash.to_bytes()?)
            }
            TrieMerkleProofStep::RoseExtensionRose { extension_hash } => {
                ret.push(TRIE_MERKLE_PROOF_ROSE_EXTENSION_ROSE_ID);
                ret.append(&mut extension_hash.to_bytes()?)
            }
            TrieMerkleProofStep::RoseExtensionExtension { rose_hash, affix } => {
                ret.push(TRIE_MERKLE_PROOF_ROSE_EXTENSION_EXTENSION_ID);
                ret.append(&mut rose_hash.to_bytes()?);
                ret.append(&mut affix.to_bytes()?)
            }
            TrieMerkleProofStep::RoseNodeRose { node_hash } => {
                ret.push(TRIE_MERKLE_PROOF_ROSE_NODE_ROSE_ID);
                ret.append(&mut node_hash.to_bytes()?);
            }
            TrieMerkleProofStep::RoseNodeNode {
                rose_hash,
                hole_index,
                indexed_pointers_with_hole,
            } => {
                ret.push(TRIE_MERKLE_PROOF_STEP_NODE_ID);
                ret.append(&mut rose_hash.to_bytes()?);
                ret.push(*hole_index);
                ret.append(&mut indexed_pointers_with_hole.to_bytes()?)
            }
        };
        Ok(ret)
    }

    fn serialized_length(&self) -> usize {
        std::mem::size_of::<u8>()
            + match self {
                TrieMerkleProofStep::Node {
                    hole_index,
                    indexed_pointers_with_hole,
                } => {
                    hole_index.serialized_length() + indexed_pointers_with_hole.serialized_length()
                }
                TrieMerkleProofStep::Extension { affix } => affix.serialized_length(),
                TrieMerkleProofStep::RoseLeafRose { leaf_hash } => leaf_hash.serialized_length(),
                TrieMerkleProofStep::RoseLeafLeaf { rose_hash } => rose_hash.serialized_length(),
                TrieMerkleProofStep::RoseExtensionRose { extension_hash } => {
                    extension_hash.serialized_length()
                }
                TrieMerkleProofStep::RoseExtensionExtension { rose_hash, affix } => {
                    rose_hash.serialized_length() + affix.serialized_length()
                }
                TrieMerkleProofStep::RoseNodeRose { node_hash } => node_hash.serialized_length(),
                TrieMerkleProofStep::RoseNodeNode {
                    rose_hash,
                    hole_index,
                    indexed_pointers_with_hole,
                } => {
                    rose_hash.serialized_length()
                        + hole_index.serialized_length()
                        + indexed_pointers_with_hole.serialized_length()
                }
            }
    }
}

impl FromBytes for TrieMerkleProofStep {
    fn from_bytes(bytes: &[u8]) -> Result<(Self, &[u8]), bytesrepr::Error> {
        let (tag, rem): (u8, &[u8]) = FromBytes::from_bytes(bytes)?;
        match tag {
            TRIE_MERKLE_PROOF_STEP_NODE_ID => {
                let (hole_index, rem): (u8, &[u8]) = FromBytes::from_bytes(rem)?;
                let (indexed_pointers_with_hole, rem): (Vec<(u8, Pointer)>, &[u8]) =
                    FromBytes::from_bytes(rem)?;
                Ok((
                    TrieMerkleProofStep::Node {
                        hole_index,
                        indexed_pointers_with_hole,
                    },
                    rem,
                ))
            }
            TRIE_MERKLE_PROOF_STEP_EXTENSION_ID => {
                let (affix, rem): (Bytes, &[u8]) = FromBytes::from_bytes(rem)?;
                Ok((TrieMerkleProofStep::Extension { affix }, rem))
            }
            TRIE_MERKLE_PROOF_ROSE_LEAF_ROSE_ID => {
                let (leaf_hash, rem): (Blake2bHash, &[u8]) = FromBytes::from_bytes(rem)?;
                Ok((TrieMerkleProofStep::RoseLeafRose { leaf_hash }, rem))
            }
            TRIE_MERKLE_PROOF_ROSE_LEAF_LEAF_ID => {
                let (rose_hash, rem): (Blake2bHash, &[u8]) = FromBytes::from_bytes(rem)?;
                Ok((TrieMerkleProofStep::RoseLeafLeaf { rose_hash }, rem))
            }
            TRIE_MERKLE_PROOF_ROSE_EXTENSION_ROSE_ID => {
                let (extension_hash, rem): (Blake2bHash, &[u8]) = FromBytes::from_bytes(rem)?;
                Ok((
                    TrieMerkleProofStep::RoseExtensionRose { extension_hash },
                    rem,
                ))
            }
            TRIE_MERKLE_PROOF_ROSE_EXTENSION_EXTENSION_ID => {
                let (rose_hash, rem): (Blake2bHash, &[u8]) = FromBytes::from_bytes(rem)?;
                let (affix, rem): (Bytes, &[u8]) = FromBytes::from_bytes(rem)?;
                Ok((
                    TrieMerkleProofStep::RoseExtensionExtension { rose_hash, affix },
                    rem,
                ))
            }
            TRIE_MERKLE_PROOF_ROSE_NODE_ROSE_ID => {
                let (node_hash, rem): (Blake2bHash, &[u8]) = FromBytes::from_bytes(rem)?;
                Ok((TrieMerkleProofStep::RoseNodeRose { node_hash }, rem))
            }
            TRIE_MERKLE_PROOF_ROSE_NODE_NODE_ID => {
                let (rose_hash, rem): (Blake2bHash, &[u8]) = FromBytes::from_bytes(rem)?;
                let (hole_index, rem): (u8, &[u8]) = FromBytes::from_bytes(rem)?;
                let (indexed_pointers_with_hole, rem): (Vec<(u8, Pointer)>, &[u8]) =
                    FromBytes::from_bytes(rem)?;
                Ok((
                    TrieMerkleProofStep::RoseNodeNode {
                        rose_hash,
                        hole_index,
                        indexed_pointers_with_hole,
                    },
                    rem,
                ))
            }

            _ => Err(bytesrepr::Error::Formatting),
        }
    }
}

/// A proof that a node with a specified `key` and `value` is present in the Merkle trie.
/// Given a state hash `x`, one can validate a proof `p` by checking `x == p.compute_state_hash()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrieMerkleProof<K, V> {
    key: K,
    value: V,
    proof_steps: VecDeque<TrieMerkleProofStep>,
}

impl<K, V> TrieMerkleProof<K, V> {
    /// Constructor for [`TrieMerkleProof`]
    pub fn new(key: K, value: V, proof_steps: VecDeque<TrieMerkleProofStep>) -> Self {
        TrieMerkleProof {
            key,
            value,
            proof_steps,
        }
    }

    /// Getter for the key in [`TrieMerkleProof`]
    pub fn key(&self) -> &K {
        &self.key
    }

    /// Getter for the value in [`TrieMerkleProof`]
    pub fn value(&self) -> &V {
        &self.value
    }

    /// Getter for the proof steps in [`TrieMerkleProof`]
    pub fn proof_steps(&self) -> &VecDeque<TrieMerkleProofStep> {
        &self.proof_steps
    }

    /// Transforms a [`TrieMerkleProof`] into the value it contains
    pub fn into_value(self) -> V {
        self.value
    }
}

impl<K, V> TrieMerkleProof<K, V>
where
    K: ToBytes + Copy + Clone,
    V: ToBytes + Clone,
{
    /// Recomputes a state root hash from a [`TrieMerkleProof`].
    /// This is done in the following steps:
    ///
    /// 1. Using [`TrieMerkleProof::key`] and [`TrieMerkleProof::value`], construct a
    /// [`Trie::Leaf`] and compute a hash for that leaf.
    ///
    /// 2. We then iterate over [`TrieMerkleProof::proof_steps`] left to right, using the hash from
    /// the previous step combined with the next step to compute a new hash.
    ///
    /// 3. When there are no more steps, we return the final hash we have computed.
    ///
    /// The steps in this function reflect `operations::rehash`.
    pub fn compute_state_hash(&self) -> Result<Blake2bHash, bytesrepr::Error> {
        let mut hash = Trie::leaf(self.key, self.value.to_owned()).merkle_hash()?;

        for (proof_step_index, proof_step) in self.proof_steps.iter().enumerate() {
            let pointer = if proof_step_index == 0 {
                Pointer::LeafPointer(hash)
            } else {
                Pointer::NodePointer(hash)
            };
            hash = match proof_step {
                TrieMerkleProofStep::Node {
                    hole_index,
                    indexed_pointers_with_hole,
                } => {
                    let hole_index = *hole_index;
                    assert!(hole_index as usize <= RADIX, "hole_index exceeded RADIX");
                    let mut indexed_pointers = indexed_pointers_with_hole.to_owned();
                    indexed_pointers.push((hole_index, pointer));
                    Trie::<K, V>::node(&indexed_pointers).merkle_hash()?
                }
                TrieMerkleProofStep::Extension { affix } => {
                    Trie::<K, V>::extension(affix.clone().into(), pointer).merkle_hash()?
                }
                TrieMerkleProofStep::RoseLeafRose { leaf_hash } => hash_pair(&hash, leaf_hash),
                TrieMerkleProofStep::RoseLeafLeaf { rose_hash } => hash_pair(rose_hash, &hash),
                TrieMerkleProofStep::RoseExtensionRose { extension_hash } => {
                    hash_pair(&hash, extension_hash)
                }
                TrieMerkleProofStep::RoseExtensionExtension { rose_hash, affix } => {
                    let affix_hash =
                        Trie::<K, V>::extension(affix.clone().into(), pointer).merkle_hash()?;
                    hash_pair(rose_hash, &affix_hash)
                }
                TrieMerkleProofStep::RoseNodeRose { node_hash } => hash_pair(&hash, node_hash),
                TrieMerkleProofStep::RoseNodeNode {
                    rose_hash,
                    hole_index,
                    indexed_pointers_with_hole,
                } => {
                    let node_hash = {
                        let hole_index = *hole_index;
                        assert!(hole_index as usize <= RADIX, "hole_index exceeded RADIX");
                        let mut indexed_pointers = indexed_pointers_with_hole.to_owned();
                        indexed_pointers.push((hole_index, pointer));
                        Trie::<K, V>::node(&indexed_pointers).merkle_hash()?
                    };
                    hash_pair(&rose_hash, &node_hash)
                }
            };
        }
        Ok(hash)
    }
}

impl<K, V> ToBytes for TrieMerkleProof<K, V>
where
    K: ToBytes,
    V: ToBytes,
{
    fn to_bytes(&self) -> Result<Vec<u8>, bytesrepr::Error> {
        let mut ret: Vec<u8> = bytesrepr::allocate_buffer(self)?;
        ret.append(&mut self.key.to_bytes()?);
        ret.append(&mut self.value.to_bytes()?);
        ret.append(&mut self.proof_steps.to_bytes()?);
        Ok(ret)
    }

    fn serialized_length(&self) -> usize {
        self.key.serialized_length()
            + self.value.serialized_length()
            + self.proof_steps.serialized_length()
    }
}

impl<K, V> FromBytes for TrieMerkleProof<K, V>
where
    K: FromBytes,
    V: FromBytes,
{
    fn from_bytes(bytes: &[u8]) -> Result<(Self, &[u8]), bytesrepr::Error> {
        let (key, rem): (K, &[u8]) = FromBytes::from_bytes(bytes)?;
        let (value, rem): (V, &[u8]) = FromBytes::from_bytes(rem)?;
        let (proof_steps, rem): (VecDeque<TrieMerkleProofStep>, &[u8]) =
            FromBytes::from_bytes(rem)?;
        Ok((
            TrieMerkleProof {
                key,
                value,
                proof_steps,
            },
            rem,
        ))
    }
}

#[cfg(test)]
mod gens {
    use proptest::{collection::vec, prelude::*};

    use casper_types::{gens::key_arb, Key};

    use crate::{
        shared::stored_value::{gens::stored_value_arb, StoredValue},
        storage::trie::{
            gens::{blake2b_hash_arb, trie_pointer_arb},
            merkle_proof::{TrieMerkleProof, TrieMerkleProofStep},
            RADIX,
        },
    };

    const POINTERS_SIZE: usize = RADIX / 8;
    const AFFIX_SIZE: usize = 6;
    const STEPS_SIZE: usize = 6;

    pub fn trie_merkle_proof_step_arb() -> impl Strategy<Value = TrieMerkleProofStep> {
        prop_oneof![
            (
                <u8>::arbitrary(),
                vec((<u8>::arbitrary(), trie_pointer_arb()), POINTERS_SIZE),
            )
                .prop_map(|(hole_index, indexed_pointers_with_hole)| {
                    TrieMerkleProofStep::Node {
                        hole_index,
                        indexed_pointers_with_hole,
                    }
                }),
            vec(<u8>::arbitrary(), AFFIX_SIZE).prop_map(|affix| TrieMerkleProofStep::Extension {
                affix: affix.into(),
            }),
            blake2b_hash_arb()
                .prop_map(|leaf_hash| TrieMerkleProofStep::RoseLeafRose { leaf_hash }),
            blake2b_hash_arb()
                .prop_map(|rose_hash| TrieMerkleProofStep::RoseLeafLeaf { rose_hash }),
            blake2b_hash_arb().prop_map(|extension_hash| TrieMerkleProofStep::RoseExtensionRose {
                extension_hash,
            }),
            (blake2b_hash_arb(), vec(<u8>::arbitrary(), AFFIX_SIZE)).prop_map(
                |(rose_hash, affix)| TrieMerkleProofStep::RoseExtensionExtension {
                    rose_hash,
                    affix: affix.into(),
                },
            ),
            blake2b_hash_arb()
                .prop_map(|node_hash| TrieMerkleProofStep::RoseNodeRose { node_hash }),
            (
                blake2b_hash_arb(),
                <u8>::arbitrary(),
                vec((<u8>::arbitrary(), trie_pointer_arb()), POINTERS_SIZE),
            )
                .prop_map(|(rose_hash, hole_index, indexed_pointers_with_hole)| {
                    TrieMerkleProofStep::RoseNodeNode {
                        rose_hash,
                        hole_index,
                        indexed_pointers_with_hole,
                    }
                }),
        ]
    }

    pub fn trie_merkle_proof_arb() -> impl Strategy<Value = TrieMerkleProof<Key, StoredValue>> {
        (
            key_arb(),
            stored_value_arb(),
            vec(trie_merkle_proof_step_arb(), STEPS_SIZE),
        )
            .prop_map(|(key, value, proof_steps)| {
                TrieMerkleProof::new(key, value, proof_steps.into())
            })
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use casper_types::bytesrepr;

    use super::gens;

    proptest! {
        #[test]
        fn trie_merkle_proof_step_serialization_is_correct(
            step in gens::trie_merkle_proof_step_arb()
        ) {
            bytesrepr::test_serialization_roundtrip(&step)
        }

        #[test]
        fn trie_merkle_proof_serialization_is_correct(
            proof in gens::trie_merkle_proof_arb()
        ) {
            bytesrepr::test_serialization_roundtrip(&proof)
        }
    }
}
