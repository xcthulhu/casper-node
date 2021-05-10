use std::collections::{BTreeMap, BTreeSet};

use proptest::{
    collection::vec,
    prelude::{any, prop_assert, proptest, Strategy},
};

use casper_types::{
    bytesrepr,
    bytesrepr::{FromBytes, ToBytes},
};

use crate::{
    shared::newtypes::CorrelationId,
    storage::{
        error::{in_memory::Error as InMemoryError, lmdb::Error as LmdbError},
        transaction_source::{Transaction, TransactionSource},
        trie::Trie,
        trie_store::{
            in_memory::InMemoryTrieStore,
            lmdb::LmdbTrieStore,
            operations::{
                keys_with_prefix,
                tests::{HashedTrie, InMemoryTestContext, LmdbTestContext},
                write, WriteResult,
            },
        },
    },
};

/// A variable length key; must be between 0 and 255 bytes in length
#[derive(Eq, PartialEq, Ord, PartialOrd, Debug, Clone)]
struct BadVariableLengthKey {
    key_bytes: Vec<u8>,
}

impl ToBytes for BadVariableLengthKey {
    fn to_bytes(&self) -> Result<Vec<u8>, bytesrepr::Error> {
        let mut result = bytesrepr::unchecked_allocate_buffer(self);
        result.push(0u8);
        result.push(self.key_bytes.len() as u8);
        result.extend_from_slice(&self.key_bytes);
        Ok(result)
    }

    fn serialized_length(&self) -> usize {
        1 + 1 + self.key_bytes.serialized_length()
    }
}

impl FromBytes for BadVariableLengthKey {
    fn from_bytes(bytes: &[u8]) -> Result<(Self, &[u8]), bytesrepr::Error> {
        let (_zero, mut remainder) = u8::from_bytes(bytes)?;
        let parsed = u8::from_bytes(bytes)?;
        let length = parsed.0 as usize;
        remainder = parsed.1;
        let mut key_bytes = Vec::with_capacity(length);
        for _ in (0..length) {
            let parsed = u8::from_bytes(remainder)?;
            key_bytes.push(parsed.0);
            remainder = parsed.1;
        }
        Ok((BadVariableLengthKey { key_bytes }, remainder))
    }
}

fn binary_arb() -> impl Strategy<Value = u8> {
    any::<u8>().prop_map(|u| u & 1)
}

fn prefixed_binary_variable_length_key_arb() -> impl Strategy<Value = BadVariableLengthKey> {
    (0usize..7)
        .prop_flat_map(|vec_length| vec(binary_arb(), vec_length))
        .prop_map(|key_bytes| BadVariableLengthKey { key_bytes })
}

fn variable_length_key_test_inputs_arb(
    size: usize,
) -> impl Strategy<Value = BTreeMap<BadVariableLengthKey, u64>> {
    vec(
        (prefixed_binary_variable_length_key_arb(), any::<u64>()),
        size,
    )
    .prop_map(|kv_pairs| {
        kv_pairs
            .into_iter()
            .collect::<BTreeMap<BadVariableLengthKey, u64>>()
    })
}

fn test_insert_variable_length_key_pairs(
    pairs_to_insert: BTreeMap<BadVariableLengthKey, u64>,
) -> anyhow::Result<()> {
    let correlation_id = CorrelationId::new();
    let empty_trie: HashedTrie<Vec<u8>, u64> = HashedTrie::new(Trie::node(&[]))?;
    let mut root = empty_trie.hash.clone();
    let context = InMemoryTestContext::new(&[empty_trie])?;
    {
        for (key, value) in pairs_to_insert.iter() {
            let mut txn = context.environment.create_read_write_txn()?;
            if let WriteResult::Written(new_root) =
                write::<BadVariableLengthKey, u64, _, InMemoryTrieStore, InMemoryError>(
                    correlation_id.clone(),
                    &mut txn,
                    &context.store,
                    &root,
                    key,
                    value,
                )?
            {
                root = new_root;
                txn.commit()?;
            } else {
                panic!("Could not write pair")
            }
        }
    }
    {
        let txn = context.environment.create_read_txn()?;
        let mut keys_found_in_input = pairs_to_insert
            .keys()
            .cloned()
            .collect::<Vec<BadVariableLengthKey>>();
        keys_found_in_input.sort();
        let mut keys_found_in_trie = keys_with_prefix::<
            BadVariableLengthKey,
            u64,
            _,
            InMemoryTrieStore,
        >(
            correlation_id.clone(), &txn, &context.store, &root, &[]
        )
        .filter_map(Result::ok)
        .collect::<Vec<BadVariableLengthKey>>();
        keys_found_in_trie.sort();
        assert_eq!(keys_found_in_trie, keys_found_in_input)
    }
    Ok(())
}

proptest! {
    #[test]
    fn prop_lmbd_succeeds(pairs_to_insert in variable_length_key_test_inputs_arb(100)) {
        prop_assert!(test_insert_variable_length_key_pairs(pairs_to_insert).is_ok());
    }
}
