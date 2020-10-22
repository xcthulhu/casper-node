use futures::executor;
use jsonrpc_lite::{JsonRpc, Params};
use reqwest::Client;
use serde::Serialize;
use serde_json::{json, Map, Value};

use casper_execution_engine::{
    core::engine_state::ExecutableDeployItem, shared::stored_value::StoredValue,
    storage::trie::merkle_proof::TrieMerkleProof,
};
use casper_node::{
    crypto::{asymmetric_key::PublicKey, hash::Digest},
    rpcs::{
        account::{PutDeploy, PutDeployParams},
        chain::{GetBlock, GetBlockParams, GetStateRootHash, GetStateRootHashParams},
        info::{GetDeploy, GetDeployParams},
        state::{GetBalance, GetBalanceParams, GetItem, GetItemParams},
        RPC_API_PATH,
    },
    types::{BlockHash, Deploy, DeployHash},
};
use casper_types::{
    bytesrepr::{self, ToBytes},
    Key, RuntimeArgs, URef, U512,
};

use crate::{
    deploy::{DeployExt, DeployParams, ListDeploys, SendDeploy, Transfer},
    error::{Error, Result},
};

/// Target for a given transfer.
pub enum TransferTarget {
    /// Transfer to another purse within an account.
    OwnPurse(URef),
    /// Transfer to another account.
    Account(PublicKey),
}

/// Struct representing a single JSON-RPC call to the casper node.
#[derive(Debug, Default)]
pub struct RpcCall {
    // TODO - If/when https://github.com/AtsukiTak/warp-json-rpc/pull/1 is merged and published,
    //        change `rpc_id` to a `jsonrpc_lite::Id`.
    rpc_id: u32,
    node_address: String,
    verbose: bool,
}

// TODO: Make me real
// pub fn check_response(state_root_hash: Digest, key: Key, response: JsonRpc) -> Result<()> {
//     let hex_encoded_proofs: &str = response.get_result()?.get("proof")?.as_str()?;
//     let proofs: Vec<TrieMerkleProof<Key, StoredValue>> =
//         bytesrepr::deserialize(hex::decode(hex_encoded_proofs)?)?;
//     let value = response.get_result()?;
//     if !validate_proofs(&state_root_hash, &proofs, &key, &value)? {
//         panic!("uh oh")
//     }
//     Ok(())
// }

/// `RpcCall` encapsulates calls made to the casper node service via JSON-RPC.
impl RpcCall {
    /// Creates a new RPC instance.
    ///
    /// `rpc_id` is used for RPC-ID as required by the JSON-RPC specification, and is returned by
    /// the node in the corresponding response.
    ///
    /// `node_address` identifies the network address of the target node's HTTP server, e.g.
    /// `"http://127.0.0.1:7777"`.
    ///
    /// When `verbose` is `true`, the request will be printed to `stdout`.
    pub fn new(rpc_id: u32, node_address: String, verbose: bool) -> Self {
        Self {
            rpc_id,
            node_address,
            verbose,
        }
    }

    /// Gets a `Deploy` from the node.
    pub fn get_deploy(self, deploy_hash: DeployHash) -> Result<JsonRpc> {
        let params = GetDeployParams { deploy_hash };
        GetDeploy::request_with_map_params(self, params)
    }

    /// Queries the node for an item at `key`, given a `path` and a `state_root_hash`.
    ///
    /// - `state_root_hash` - Hash representing a pointer to the current state from which a value
    ///   can be extracted. See `get_state_root_hash`.
    /// - `key` - Key from which to get a value. See (Key)[casper_types::Key].
    /// - `path` - Path components starting with the `key`.
    pub fn get_item(self, state_root_hash: Digest, key: Key, path: Vec<String>) -> Result<JsonRpc> {
        let params = GetItemParams {
            state_root_hash,
            key: key.to_formatted_string(),
            path,
        };
        let response: JsonRpc = GetItem::request_with_map_params(self, params)?;

        let value = response.get_result().expect("should have result");
        let object = value.as_object().expect("should be an object");
        let proof = object.get("proof").expect("should have proof");
        let proof_str = proof.as_str().expect("should cast to a str");
        let proof_bytes = hex::decode(proof_str).expect("should decode proof");
        let proofs: Vec<TrieMerkleProof<Key, StoredValue>> =
            bytesrepr::deserialize(proof_bytes).expect("proof_bytes should deserialize");
        assert!(
            proofs.iter().all(|proof| {
                proof.compute_state_hash().expect("should compute") == state_root_hash.into()
            }),
            "merkle proof validation failed"
        );

        Ok(response)
    }

    /// Queries the node for the most recent state root hash or as of a given block hash if
    /// `maybe_block_hash` is provided.
    pub fn get_state_root_hash(self, maybe_block_hash: Option<BlockHash>) -> Result<JsonRpc> {
        match maybe_block_hash {
            Some(block_hash) => {
                let params = GetStateRootHashParams { block_hash };
                GetStateRootHash::request_with_map_params(self, params)
            }
            None => GetStateRootHash::request(self),
        }
    }

    /// Gets the balance from a purse.
    ///
    /// - `state_root_hash` - Hash representing a pointer to the current state from which a balance
    ///   can be extracted. See `get_state_root_hash`.
    /// - `purse_uref` - The purse `URef` that can obtained with `get_item`.
    pub fn get_balance(self, state_root_hash: Digest, purse_uref: URef) -> Result<JsonRpc> {
        let params = GetBalanceParams {
            state_root_hash,
            purse_uref: purse_uref.to_formatted_string(),
        };
        GetBalance::request_with_map_params(self, params)
    }

    /// Transfers an amount between purses.
    ///
    /// - `amount` - Specifies the amount to be transferred.
    /// - `source_purse` - Source purse in the sender's account that this amount will be drawn from,
    ///   if it is `None`, it will default to the account's main purse.
    /// - `target` - Target for this transfer - see (TransferTarget)[TransferTarget].
    pub fn transfer(
        self,
        amount: U512,
        source_purse: Option<URef>,
        target: TransferTarget,
        deploy_params: DeployParams,
        payment: ExecutableDeployItem,
    ) -> Result<JsonRpc> {
        const TRANSFER_ARG_AMOUNT: &str = "amount";
        const TRANSFER_ARG_SOURCE: &str = "source";
        const TRANSFER_ARG_TARGET: &str = "target";

        let mut transfer_args = RuntimeArgs::new();
        transfer_args.insert(TRANSFER_ARG_AMOUNT, amount);
        if let Some(source_purse) = source_purse {
            transfer_args.insert(TRANSFER_ARG_SOURCE, source_purse);
        }
        match target {
            TransferTarget::Account(target_account) => {
                let target_account_hash = target_account.to_account_hash().value();
                transfer_args.insert(TRANSFER_ARG_TARGET, target_account_hash);
            }
            TransferTarget::OwnPurse(target_purse) => {
                transfer_args.insert(TRANSFER_ARG_TARGET, target_purse);
            }
        }
        let session = ExecutableDeployItem::Transfer {
            args: transfer_args.to_bytes()?,
        };
        let deploy = Deploy::with_payment_and_session(deploy_params, payment, session);
        let params = PutDeployParams { deploy };
        Transfer::request_with_map_params(self, params)
    }

    /// Reads a previously-saved `Deploy` from file, and sends that to the node.
    pub fn send_deploy_file(self, input_path: &str) -> Result<JsonRpc> {
        let deploy = Deploy::read_deploy(input_path)?;
        let params = PutDeployParams { deploy };
        SendDeploy::request_with_map_params(self, params)
    }

    /// Puts a `Deploy` to the node.
    pub fn put_deploy(self, deploy: Deploy) -> Result<JsonRpc> {
        let params = PutDeployParams { deploy };
        PutDeploy::request_with_map_params(self, params)
    }

    /// List `Deploy`s included in the specified `Block`.
    ///
    /// If `maybe_block_hash` is `Some`, that specific block is used, otherwise the most
    /// recently-added block is used.
    pub fn list_deploys(self, maybe_block_hash: Option<BlockHash>) -> Result<JsonRpc> {
        match maybe_block_hash {
            Some(block_hash) => {
                let params = GetBlockParams { block_hash };
                ListDeploys::request_with_map_params(self, params)
            }
            None => ListDeploys::request(self),
        }
    }

    /// Gets a `Block` from the node.
    ///
    /// If `maybe_block_hash` is `Some`, that specific `Block` is retrieved, otherwise the most
    /// recently-added `Block` is retrieved.
    pub fn get_block(self, maybe_block_hash: Option<BlockHash>) -> Result<JsonRpc> {
        match maybe_block_hash {
            Some(block_hash) => {
                let params = GetBlockParams { block_hash };
                GetBlock::request_with_map_params(self, params)
            }
            None => GetBlock::request(self),
        }
    }

    async fn request(self, method: &str, params: Params) -> Result<JsonRpc> {
        let url = format!("{}/{}", self.node_address, RPC_API_PATH);
        let rpc_req = JsonRpc::request_with_params(self.rpc_id as i64, method, params);

        if self.verbose {
            println!(
                "{}",
                serde_json::to_string_pretty(&rpc_req).expect("should encode to JSON")
            );
        }

        let client = Client::new();
        let response = client
            .post(&url)
            .json(&rpc_req)
            .send()
            .await
            .map_err(Error::FailedToGetResponse)?;

        if let Err(error) = response.error_for_status_ref() {
            if self.verbose {
                println!("Failed Sending {}", error);
            }
            return Err(Error::FailedSending(rpc_req));
        }

        let rpc_response = response.json().await.map_err(Error::FailedToParseResponse);

        if let Err(error) = rpc_response {
            if self.verbose {
                println!("Failed parsing as a JSON-RPC response: {}", error);
            }
            return Err(error);
        }

        let rpc_response: JsonRpc = rpc_response?;

        if rpc_response.get_result().is_some() {
            if self.verbose {
                println!("Received successful response:");
            }
            return Ok(rpc_response);
        }

        if let Some(error) = rpc_response.get_error() {
            if self.verbose {
                println!("Response returned an error");
            }
            return Err(Error::ResponseIsError(error.clone()));
        }

        if self.verbose {
            println!("Invalid response returned");
        }
        Err(Error::InvalidResponse(rpc_response))
    }
}

/// General purpose client trait for making requests to casper node's HTTP endpoints.
pub(crate) trait RpcClient {
    const RPC_METHOD: &'static str;

    /// Calls a casper node's JSON-RPC endpoint.
    fn request(rpc_call: RpcCall) -> Result<JsonRpc> {
        executor::block_on(async { rpc_call.request(Self::RPC_METHOD, Params::None(())).await })
    }

    /// Calls a casper node's JSON-RPC endpoint with parameters.
    fn request_with_map_params<T: IntoJsonMap>(rpc_call: RpcCall, params: T) -> Result<JsonRpc> {
        executor::block_on(async {
            rpc_call
                .request(Self::RPC_METHOD, Params::from(params.into_json_map()))
                .await
        })
    }
}

pub(crate) trait IntoJsonMap: Serialize {
    fn into_json_map(self) -> Map<String, Value>
    where
        Self: Sized,
    {
        json!(self)
            .as_object()
            .unwrap_or_else(|| panic!("should be a JSON object"))
            .clone()
    }
}

impl IntoJsonMap for PutDeployParams {}
impl IntoJsonMap for GetBlockParams {}
impl IntoJsonMap for GetStateRootHashParams {}
impl IntoJsonMap for GetDeployParams {}
impl IntoJsonMap for GetBalanceParams {}
impl IntoJsonMap for GetItemParams {}
