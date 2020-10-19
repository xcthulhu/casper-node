#[cfg(test)]
mod tests;

use std::{cmp, collections::VecDeque, mem};

use casper_types::bytesrepr::{self, FromBytes, ToBytes};

use crate::{
    shared::newtypes::{Blake2bHash, CorrelationId},
    storage::{
        transaction_source::{Readable, Writable},
        trie::{
            merkle_proof::{TrieMerkleProof, TrieMerkleProofStep},
            Parents, Pointer, Trie, RADIX,
        },
        trie_store::TrieStore,
    },
};

#[derive(Debug, PartialEq, Eq)]
pub enum ReadResult<V> {
    Found(V),
    NotFound,
    RootNotFound,
}

/// Returns a value from the corresponding key at a given root in a given store
pub fn read<K, V, T, S, E>(
    _correlation_id: CorrelationId,
    txn: &T,
    store: &S,
    root: &Blake2bHash,
    key: &K,
) -> Result<ReadResult<V>, E>
where
    K: ToBytes + FromBytes + Eq + std::fmt::Debug,
    V: ToBytes + FromBytes,
    T: Readable<Handle = S::Handle>,
    S: TrieStore<K, V>,
    S::Error: From<T::Error>,
    E: From<S::Error> + From<bytesrepr::Error>,
{
    let path: Vec<u8> = key.to_bytes()?;

    let mut depth: usize = 0;
    let mut current: Trie<K, V> = match store.get(txn, root)? {
        Some(root) => root,
        None => return Ok(ReadResult::RootNotFound),
    };

    loop {
        match current {
            Trie::Leaf {
                key: leaf_key,
                value: leaf_value,
            } => {
                let result = if *key == leaf_key {
                    ReadResult::Found(leaf_value)
                } else {
                    // Keys may not match in the case of a compressed path from
                    // a Node directly to a Leaf
                    ReadResult::NotFound
                };
                return Ok(result);
            }
            Trie::Node { pointer_block } => {
                let index: usize = {
                    assert!(depth < path.len(), "depth must be < {}", path.len());
                    path[depth].into()
                };
                let maybe_pointer: Option<Pointer> = {
                    assert!(index < RADIX, "key length must be < {}", RADIX);
                    pointer_block[index]
                };
                match maybe_pointer {
                    Some(pointer) => match store.get(txn, pointer.hash())? {
                        Some(next) => {
                            depth += 1;
                            current = next;
                        }
                        None => {
                            panic!(
                                "No trie value at key: {:?} (reading from key: {:?})",
                                pointer.hash(),
                                key
                            );
                        }
                    },
                    None => {
                        return Ok(ReadResult::NotFound);
                    }
                }
            }
            Trie::Extension { affix, pointer } => {
                let sub_path = &path[depth..depth + affix.len()];
                if sub_path == affix.as_slice() {
                    match store.get(txn, pointer.hash())? {
                        Some(next) => {
                            depth += affix.len();
                            current = next;
                        }
                        None => {
                            panic!(
                                "No trie value at key: {:?} (reading from key: {:?})",
                                pointer.hash(),
                                key
                            );
                        }
                    }
                } else {
                    return Ok(ReadResult::NotFound);
                }
            }
        }
    }
}

pub fn read_with_proof<K, V, T, S, E>(
    _correlation_id: CorrelationId,
    txn: &T,
    store: &S,
    root: &Blake2bHash,
    key: &K,
) -> Result<ReadResult<TrieMerkleProof<K, V>>, E>
where
    K: ToBytes + FromBytes + Eq + std::fmt::Debug,
    V: ToBytes + FromBytes,
    T: Readable<Handle = S::Handle>,
    S: TrieStore<K, V>,
    S::Error: From<T::Error>,
    E: From<S::Error> + From<bytesrepr::Error>,
{
    let mut proof_steps = VecDeque::new();
    let path: Vec<u8> = key.to_bytes()?;

    let mut depth: usize = 0;
    let mut current: Trie<K, V> = match store.get(txn, root)? {
        Some(root) => root,
        None => return Ok(ReadResult::RootNotFound),
    };
    loop {
        match current {
            Trie::Leaf {
                key: leaf_key,
                value,
            } => {
                if *key != leaf_key {
                    return Ok(ReadResult::NotFound);
                }
                let key = leaf_key;
                return Ok(ReadResult::Found(TrieMerkleProof::new(
                    key,
                    value,
                    proof_steps,
                )));
            }
            Trie::Node { pointer_block } => {
                let hole_index: usize = {
                    assert!(depth < path.len(), "depth must be < {}", path.len());
                    path[depth].into()
                };
                let pointer: Pointer = {
                    assert!(hole_index < RADIX, "key length must be < {}", RADIX);
                    match pointer_block[hole_index] {
                        Some(pointer) => pointer,
                        None => return Ok(ReadResult::NotFound),
                    }
                };
                let indexed_pointers_with_hole = pointer_block
                    .to_indexed_pointers()
                    .filter(|(index, _)| *index != hole_index)
                    .collect();
                let next = match store.get(txn, pointer.hash())? {
                    Some(next) => next,
                    None => {
                        panic!(
                            "No trie value at key: {:?} (reading from key: {:?})",
                            pointer.hash(),
                            key
                        );
                    }
                };
                depth += 1;
                current = next;
                proof_steps.push_front(TrieMerkleProofStep::node(
                    hole_index,
                    indexed_pointers_with_hole,
                ));
            }
            Trie::Extension { affix, pointer } => {
                let sub_path = &path[depth..depth + affix.len()];
                if sub_path != affix.as_slice() {
                    return Ok(ReadResult::NotFound);
                };

                let next = match store.get(txn, pointer.hash())? {
                    Some(next) => next,
                    None => {
                        panic!(
                            "No trie value at key: {:?} (reading from key: {:?})",
                            pointer.hash(),
                            key
                        );
                    }
                };
                depth += affix.len();
                current = next;
                proof_steps.push_front(TrieMerkleProofStep::extension(affix));
            }
        }
    }
}

struct TrieScan<K, V> {
    tip: Trie<K, V>,
    parents: Parents<K, V>,
}

impl<K, V> TrieScan<K, V> {
    fn new(tip: Trie<K, V>, parents: Parents<K, V>) -> Self {
        TrieScan { tip, parents }
    }
}

/// Returns a [`TrieScan`] from the given key at a given root in a given store.
/// A scan consists of the deepest trie variant found at that key, a.k.a. the
/// "tip", along the with the parents of that variant. Parents are ordered by
/// their depth from the root (shallow to deep).
fn scan<K, V, T, S, E>(
    _correlation_id: CorrelationId,
    txn: &T,
    store: &S,
    key_bytes: &[u8],
    root: &Trie<K, V>,
) -> Result<TrieScan<K, V>, E>
where
    K: ToBytes + FromBytes + Clone,
    V: ToBytes + FromBytes + Clone,
    T: Readable<Handle = S::Handle>,
    S: TrieStore<K, V>,
    S::Error: From<T::Error>,
    E: From<S::Error> + From<bytesrepr::Error>,
{
    let path = key_bytes;

    let mut current = root.to_owned();
    let mut depth: usize = 0;
    let mut acc: Parents<K, V> = Vec::new();

    loop {
        match current {
            leaf @ Trie::Leaf { .. } => {
                return Ok(TrieScan::new(leaf, acc));
            }
            Trie::Node { pointer_block } => {
                let index = {
                    assert!(depth < path.len(), "depth must be < {}", path.len());
                    path[depth]
                };
                let maybe_pointer: Option<Pointer> = {
                    let index: usize = index.into();
                    assert!(index < RADIX, "index must be < {}", RADIX);
                    pointer_block[index]
                };
                let pointer = match maybe_pointer {
                    Some(pointer) => pointer,
                    None => {
                        return Ok(TrieScan::new(Trie::Node { pointer_block }, acc));
                    }
                };
                match store.get(txn, pointer.hash())? {
                    Some(next) => {
                        current = next;
                        depth += 1;
                        acc.push((index, Trie::Node { pointer_block }))
                    }
                    None => {
                        panic!(
                            "No trie value at key: {:?} (reading from path: {:?})",
                            pointer.hash(),
                            path
                        );
                    }
                }
            }
            Trie::Extension { affix, pointer } => {
                let sub_path = &path[depth..depth + affix.len()];
                if sub_path != affix.as_slice() {
                    return Ok(TrieScan::new(Trie::Extension { affix, pointer }, acc));
                }
                match store.get(txn, pointer.hash())? {
                    Some(next) => {
                        let index = {
                            assert!(depth < path.len(), "depth must be < {}", path.len());
                            path[depth]
                        };
                        current = next;
                        depth += affix.len();
                        acc.push((index, Trie::Extension { affix, pointer }))
                    }
                    None => {
                        panic!(
                            "No trie value at key: {:?} (reading from path: {:?})",
                            pointer.hash(),
                            path
                        );
                    }
                }
            }
        }
    }
}

#[allow(clippy::type_complexity)]
fn rehash<K, V>(
    mut tip: Trie<K, V>,
    parents: Parents<K, V>,
) -> Result<Vec<(Blake2bHash, Trie<K, V>)>, bytesrepr::Error>
where
    K: ToBytes + Clone,
    V: ToBytes + Clone,
{
    let mut ret: Vec<(Blake2bHash, Trie<K, V>)> = Vec::new();
    let mut tip_hash = {
        let trie_bytes = tip.to_bytes()?;
        Blake2bHash::new(&trie_bytes)
    };
    ret.push((tip_hash, tip.to_owned()));

    for (index, parent) in parents.into_iter().rev() {
        match parent {
            Trie::Leaf { .. } => {
                panic!("parents should not contain any leaves");
            }
            Trie::Node { mut pointer_block } => {
                tip = {
                    let pointer = match tip {
                        Trie::Leaf { .. } => Pointer::LeafPointer(tip_hash),
                        Trie::Node { .. } => Pointer::NodePointer(tip_hash),
                        Trie::Extension { .. } => Pointer::NodePointer(tip_hash),
                    };
                    pointer_block[index.into()] = Some(pointer);
                    Trie::Node { pointer_block }
                };
                tip_hash = {
                    let node_bytes = tip.to_bytes()?;
                    Blake2bHash::new(&node_bytes)
                };
                ret.push((tip_hash, tip.to_owned()))
            }
            Trie::Extension { affix, pointer } => {
                tip = {
                    let pointer = pointer.update(tip_hash);
                    Trie::Extension { affix, pointer }
                };
                tip_hash = {
                    let extension_bytes = tip.to_bytes()?;
                    Blake2bHash::new(&extension_bytes)
                };
                ret.push((tip_hash, tip.to_owned()))
            }
        }
    }
    Ok(ret)
}

fn common_prefix<A: Eq + Clone>(ls: &[A], rs: &[A]) -> Vec<A> {
    ls.iter()
        .zip(rs.iter())
        .take_while(|(l, r)| l == r)
        .map(|(l, _)| l.to_owned())
        .collect()
}

fn get_parents_path<K, V>(parents: &[(u8, Trie<K, V>)]) -> Vec<u8> {
    let mut ret = Vec::new();
    for (index, element) in parents.iter() {
        if let Trie::Extension { affix, .. } = element {
            ret.extend(affix);
        } else {
            ret.push(index.to_owned());
        }
    }
    ret
}

/// Takes a path to a leaf, that leaf's parent node, and the parents of that
/// node, and adds the node to the parents.
///
/// This function will panic if the the path to the leaf and the path to its
/// parent node do not share a common prefix.
fn add_node_to_parents<K, V>(
    path_to_leaf: &[u8],
    new_parent_node: Trie<K, V>,
    mut parents: Parents<K, V>,
) -> Result<Parents<K, V>, bytesrepr::Error>
where
    K: ToBytes,
    V: ToBytes,
{
    // TODO: add is_node() method to Trie
    match new_parent_node {
        Trie::Node { .. } => (),
        _ => panic!("new_parent must be a node"),
    }
    // The current depth will be the length of the path to the new parent node.
    let depth: usize = {
        // Get the path to this node
        let path_to_node: Vec<u8> = get_parents_path(&parents);
        // Check that the path to the node is a prefix of the current path
        let current_path = common_prefix(&path_to_leaf, &path_to_node);
        assert_eq!(current_path, path_to_node);
        // Get the length
        path_to_node.len()
    };
    // Index path by current depth;
    let index = {
        assert!(
            depth < path_to_leaf.len(),
            "depth must be < {}",
            path_to_leaf.len()
        );
        path_to_leaf[depth]
    };
    // Add node to parents, along with index to modify
    parents.push((index, new_parent_node));
    Ok(parents)
}

/// Takes paths to a new leaf and an existing leaf that share a common prefix,
/// along with the parents of the existing leaf. Creates a new node (adding a
/// possible parent extension for it to parents) which contains the existing
/// leaf.  Returns the new node and parents, so that they can be used by
/// [`add_node_to_parents`].
#[allow(clippy::type_complexity)]
fn reparent_leaf<K, V>(
    new_leaf_path: &[u8],
    existing_leaf_path: &[u8],
    parents: Parents<K, V>,
) -> Result<(Trie<K, V>, Parents<K, V>), bytesrepr::Error>
where
    K: ToBytes,
    V: ToBytes,
{
    let mut parents = parents;
    let (child_index, parent) = parents.pop().expect("parents should not be empty");
    let pointer_block = match parent {
        Trie::Node { pointer_block } => pointer_block,
        _ => panic!("A leaf should have a node for its parent"),
    };
    // Get the path that the new leaf and existing leaf share
    let shared_path = common_prefix(&new_leaf_path, &existing_leaf_path);
    // Assemble a new node to hold the existing leaf. The new leaf will
    // be added later during the add_parent_node and rehash phase.
    let new_node = {
        let index: usize = existing_leaf_path[shared_path.len()].into();
        let existing_leaf_pointer =
            pointer_block[<usize>::from(child_index)].expect("parent has lost the existing leaf");
        Trie::node(&[(index, existing_leaf_pointer)])
    };
    // Re-add the parent node to parents
    parents.push((child_index, Trie::Node { pointer_block }));
    // Create an affix for a possible extension node
    let affix = {
        let parents_path = get_parents_path(&parents);
        &shared_path[parents_path.len()..]
    };
    // If the affix is non-empty, create an extension node and add it
    // to parents.
    if !affix.is_empty() {
        let new_node_bytes = new_node.to_bytes()?;
        let new_node_hash = Blake2bHash::new(&new_node_bytes);
        let new_extension = Trie::extension(affix.to_vec(), Pointer::NodePointer(new_node_hash));
        parents.push((child_index, new_extension));
    }
    Ok((new_node, parents))
}

struct SplitResult<K, V> {
    new_node: Trie<K, V>,
    parents: Parents<K, V>,
    maybe_hashed_child_extension: Option<(Blake2bHash, Trie<K, V>)>,
}

/// Takes a path to a new leaf, an existing extension that leaf collides with,
/// and the parents of that extension.  Creates a new node and possible parent
/// and child extensions.  The node pointer contained in the existing extension
/// is repositioned in the new node or the possible child extension.  The
/// possible parent extension is added to parents.  Returns the new node,
/// parents, and the the possible child extension (paired with its hash).
/// The new node and parents can be used by [`add_node_to_parents`], and the
/// new hashed child extension can be added to the list of new trie elements.
fn split_extension<K, V>(
    new_leaf_path: &[u8],
    existing_extension: Trie<K, V>,
    mut parents: Parents<K, V>,
) -> Result<SplitResult<K, V>, bytesrepr::Error>
where
    K: ToBytes + Clone,
    V: ToBytes + Clone,
{
    // TODO: add is_extension() method to Trie
    let (affix, pointer) = match existing_extension {
        Trie::Extension { affix, pointer } => (affix, pointer),
        _ => panic!("existing_extension must be an extension"),
    };
    let parents_path = get_parents_path(&parents);
    // Get the path to the existing extension node
    let existing_extension_path: Vec<u8> =
        parents_path.iter().chain(affix.iter()).cloned().collect();
    // Get the path that the new leaf and existing leaf share
    let shared_path = common_prefix(&new_leaf_path, &existing_extension_path);
    // Create an affix for a possible parent extension above the new
    // node.
    let parent_extension_affix = shared_path[parents_path.len()..].to_vec();
    // Create an affix for a possible child extension between the new
    // node and the node that the existing extension pointed to.
    let child_extension_affix = affix[parent_extension_affix.len() + 1..].to_vec();
    // Create a child extension (paired with its hash) if necessary
    let maybe_hashed_child_extension: Option<(Blake2bHash, Trie<K, V>)> =
        if child_extension_affix.is_empty() {
            None
        } else {
            let child_extension = Trie::extension(child_extension_affix.to_vec(), pointer);
            let child_extension_bytes = child_extension.to_bytes()?;
            let child_extension_hash = Blake2bHash::new(&child_extension_bytes);
            Some((child_extension_hash, child_extension))
        };
    // Assemble a new node.
    let new_node: Trie<K, V> = {
        let index: usize = existing_extension_path[shared_path.len()].into();
        let pointer = maybe_hashed_child_extension
            .to_owned()
            .map_or(pointer, |(hash, _)| Pointer::NodePointer(hash));
        Trie::node(&[(index, pointer)])
    };
    // Create a parent extension if necessary
    if !parent_extension_affix.is_empty() {
        let new_node_bytes = new_node.to_bytes()?;
        let new_node_hash = Blake2bHash::new(&new_node_bytes);
        let parent_extension = Trie::extension(
            parent_extension_affix.to_vec(),
            Pointer::NodePointer(new_node_hash),
        );
        parents.push((parent_extension_affix[0], parent_extension));
    }
    Ok(SplitResult {
        new_node,
        parents,
        maybe_hashed_child_extension,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum WriteResult {
    Written(Blake2bHash),
    AlreadyExists,
    RootNotFound,
}

pub fn write<K, V, T, S, E>(
    correlation_id: CorrelationId,
    txn: &mut T,
    store: &S,
    root: &Blake2bHash,
    key: &K,
    value: &V,
) -> Result<WriteResult, E>
where
    K: ToBytes + FromBytes + Clone + Eq + std::fmt::Debug,
    V: ToBytes + FromBytes + Clone + Eq,
    T: Readable<Handle = S::Handle> + Writable<Handle = S::Handle>,
    S: TrieStore<K, V>,
    S::Error: From<T::Error>,
    E: From<S::Error> + From<bytesrepr::Error>,
{
    match store.get(txn, root)? {
        None => Ok(WriteResult::RootNotFound),
        Some(current_root) => {
            let new_leaf = Trie::Leaf {
                key: key.to_owned(),
                value: value.to_owned(),
            };
            let path: Vec<u8> = key.to_bytes()?;
            let TrieScan { tip, parents } =
                scan::<K, V, T, S, E>(correlation_id, txn, store, &path, &current_root)?;
            let new_elements: Vec<(Blake2bHash, Trie<K, V>)> = match tip {
                // If the "tip" is the same as the new leaf, then the leaf
                // is already in the Trie.
                Trie::Leaf { .. } if new_leaf == tip => Vec::new(),
                // If the "tip" is an existing leaf with the same key as the
                // new leaf, but the existing leaf and new leaf have different
                // values, then we are in the situation where we are "updating"
                // an existing leaf.
                Trie::Leaf {
                    key: ref leaf_key,
                    value: ref leaf_value,
                } if key == leaf_key && value != leaf_value => rehash(new_leaf, parents)?,
                // If the "tip" is an existing leaf with a different key than
                // the new leaf, then we are in a situation where the new leaf
                // shares some common prefix with the existing leaf.
                Trie::Leaf {
                    key: ref existing_leaf_key,
                    ..
                } if key != existing_leaf_key => {
                    let existing_leaf_path = existing_leaf_key.to_bytes()?;
                    let (new_node, parents) = reparent_leaf(&path, &existing_leaf_path, parents)?;
                    let parents = add_node_to_parents(&path, new_node, parents)?;
                    rehash(new_leaf, parents)?
                }
                // This case is unreachable, but the compiler can't figure
                // that out.
                Trie::Leaf { .. } => unreachable!(),
                // If the "tip" is an existing node, then we can add a pointer
                // to the new leaf to the node's pointer block.
                node @ Trie::Node { .. } => {
                    let parents = add_node_to_parents(&path, node, parents)?;
                    rehash(new_leaf, parents)?
                }
                // If the "tip" is an extension node, then we must modify or
                // replace it, adding a node where necessary.
                extension @ Trie::Extension { .. } => {
                    let SplitResult {
                        new_node,
                        parents,
                        maybe_hashed_child_extension,
                    } = split_extension(&path, extension, parents)?;
                    let parents = add_node_to_parents(&path, new_node, parents)?;
                    if let Some(hashed_extension) = maybe_hashed_child_extension {
                        let mut ret = vec![hashed_extension];
                        ret.extend(rehash(new_leaf, parents)?);
                        ret
                    } else {
                        rehash(new_leaf, parents)?
                    }
                }
            };
            if new_elements.is_empty() {
                return Ok(WriteResult::AlreadyExists);
            }
            let mut root_hash = root.to_owned();
            for (hash, element) in new_elements.iter() {
                store.put(txn, hash, element)?;
                root_hash = *hash;
            }
            Ok(WriteResult::Written(root_hash))
        }
    }
}

enum KeysIteratorState<K, V, S: TrieStore<K, V>> {
    /// Iterate normally
    Ok,
    /// Return the error and stop iterating
    #[allow(dead_code)] // Return variant alone is used in testing.
    ReturnError(S::Error),
    /// Already failed, only return None
    Failed,
}

struct VisitedTrieNode<K, V> {
    trie: Trie<K, V>,
    maybe_index: Option<usize>,
    path: Vec<u8>,
}

pub struct KeysIterator<'a, 'b, K, V, T, S: TrieStore<K, V>> {
    initial_descend: VecDeque<u8>,
    visited: Vec<VisitedTrieNode<K, V>>,
    store: &'a S,
    txn: &'b T,
    state: KeysIteratorState<K, V, S>,
}

impl<'a, 'b, K, V, T, S> Iterator for KeysIterator<'a, 'b, K, V, T, S>
where
    K: ToBytes + FromBytes + Clone + Eq + std::fmt::Debug,
    V: ToBytes + FromBytes + Clone + Eq + std::fmt::Debug,
    T: Readable<Handle = S::Handle>,
    S: TrieStore<K, V>,
    S::Error: From<T::Error> + From<bytesrepr::Error>,
{
    type Item = Result<K, S::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        match mem::replace(&mut self.state, KeysIteratorState::Ok) {
            KeysIteratorState::Ok => (),
            KeysIteratorState::ReturnError(e) => {
                self.state = KeysIteratorState::Failed;
                return Some(Err(e));
            }
            KeysIteratorState::Failed => {
                return None;
            }
        }
        while let Some(VisitedTrieNode {
            trie,
            maybe_index,
            mut path,
        }) = self.visited.pop()
        {
            let mut maybe_next_trie: Option<Trie<K, V>> = None;

            match trie {
                Trie::Leaf { key, .. } => {
                    let key_bytes = match key.to_bytes() {
                        Ok(bytes) => bytes,
                        Err(e) => {
                            self.state = KeysIteratorState::Failed;
                            return Some(Err(e.into()));
                        }
                    };
                    debug_assert!(key_bytes.starts_with(&path));
                    // only return the leaf if it matches the initial descend path
                    path.extend(&self.initial_descend);
                    if key_bytes.starts_with(&path) {
                        return Some(Ok(key));
                    }
                }
                Trie::Node { ref pointer_block } => {
                    // if we are still initially descending (and initial_descend is not empty), take
                    // the first index we should descend to, otherwise take maybe_index from the
                    // visited stack
                    let mut index: usize = self
                        .initial_descend
                        .front()
                        .map(|i| *i as usize)
                        .or(maybe_index)
                        .unwrap_or_default();
                    while index < RADIX {
                        if let Some(ref pointer) = pointer_block[index] {
                            maybe_next_trie = match self.store.get(self.txn, pointer.hash()) {
                                Ok(trie) => trie,
                                Err(e) => {
                                    self.state = KeysIteratorState::Failed;
                                    return Some(Err(e));
                                }
                            };
                            debug_assert!(maybe_next_trie.is_some());
                            if self.initial_descend.pop_front().is_none() {
                                self.visited.push(VisitedTrieNode {
                                    trie,
                                    maybe_index: Some(index + 1),
                                    path: path.clone(),
                                });
                            }
                            path.push(index as u8);
                            break;
                        }
                        // only continue the loop if we are not initially descending;
                        // if we are descending and we land here, it means that there is no subtrie
                        // along the descend path and we will return no results
                        if !self.initial_descend.is_empty() {
                            break;
                        }
                        index += 1;
                    }
                }
                Trie::Extension { affix, pointer } => {
                    let descend_len = cmp::min(self.initial_descend.len(), affix.len());
                    let check_prefix = self
                        .initial_descend
                        .drain(..descend_len)
                        .collect::<Vec<_>>();
                    // if we are initially descending, we only want to continue if the affix
                    // matches the descend path
                    // if we are not, the check_prefix will be empty, so we will enter the if
                    // anyway
                    if affix.starts_with(&check_prefix) {
                        maybe_next_trie = match self.store.get(self.txn, pointer.hash()) {
                            Ok(trie) => trie,
                            Err(e) => {
                                self.state = KeysIteratorState::Failed;
                                return Some(Err(e));
                            }
                        };
                        debug_assert!({
                            match &maybe_next_trie {
                                Some(Trie::Node { .. }) => true,
                                _ => false,
                            }
                        });
                        path.extend(affix);
                    }
                }
            }

            if let Some(next_trie) = maybe_next_trie {
                self.visited.push(VisitedTrieNode {
                    trie: next_trie,
                    maybe_index: None,
                    path,
                });
            }
        }
        None
    }
}

/// Returns the iterator over the keys at a given root hash.
///
/// The root should be the apex of the trie.
#[cfg(test)]
pub fn keys<'a, 'b, K, V, T, S>(
    correlation_id: CorrelationId,
    txn: &'b T,
    store: &'a S,
    root: &Blake2bHash,
) -> KeysIterator<'a, 'b, K, V, T, S>
where
    K: ToBytes + FromBytes + Clone + Eq + std::fmt::Debug,
    V: ToBytes + FromBytes + Clone + Eq + std::fmt::Debug,
    T: Readable<Handle = S::Handle>,
    S: TrieStore<K, V>,
    S::Error: From<T::Error>,
{
    keys_with_prefix(correlation_id, txn, store, root, &[])
}

/// Returns the iterator over the keys in the subtrie matching `prefix`.
///
/// The root should be the apex of the trie.
#[cfg(test)]
pub fn keys_with_prefix<'a, 'b, K, V, T, S>(
    _correlation_id: CorrelationId,
    txn: &'b T,
    store: &'a S,
    root: &Blake2bHash,
    prefix: &[u8],
) -> KeysIterator<'a, 'b, K, V, T, S>
where
    K: ToBytes + FromBytes + Clone + Eq + std::fmt::Debug,
    V: ToBytes + FromBytes + Clone + Eq + std::fmt::Debug,
    T: Readable<Handle = S::Handle>,
    S: TrieStore<K, V>,
    S::Error: From<T::Error>,
{
    let (visited, init_state): (Vec<VisitedTrieNode<K, V>>, _) = match store.get(txn, root) {
        Ok(None) => (vec![], KeysIteratorState::Ok),
        Err(e) => (vec![], KeysIteratorState::ReturnError(e)),
        Ok(Some(current_root)) => (
            vec![VisitedTrieNode {
                trie: current_root,
                maybe_index: None,
                path: vec![],
            }],
            KeysIteratorState::Ok,
        ),
    };

    KeysIterator {
        initial_descend: prefix.iter().cloned().collect(),
        visited,
        store,
        txn,
        state: init_state,
    }
}
