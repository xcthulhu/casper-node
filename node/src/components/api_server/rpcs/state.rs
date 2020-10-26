//! RPCs related to the state.

use std::{convert::TryFrom, str};

use futures::{future::BoxFuture, FutureExt};
use http::Response;
use hyper::Body;
use semver::Version;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};
use warp_json_rpc::Builder;

use casper_execution_engine::{
    core::engine_state::{BalanceResult, QueryResult},
    storage::protocol_data::ProtocolData,
};
use casper_types::{bytesrepr::ToBytes, Key, ProtocolVersion, URef, U512};

use super::{ApiRequest, Error, ErrorCode, ReactorEventT, RpcWithParams, RpcWithParamsExt};
use crate::{
    components::api_server::CLIENT_API_VERSION,
    crypto::hash::Digest,
    effect::EffectBuilder,
    reactor::QueueKind,
    types::{
        json_compatibility::{AuctionState, StoredValue},
        Block,
    },
};

/// Params for "state_get_item" RPC request.
#[derive(Serialize, Deserialize, Debug)]
pub struct GetItemParams {
    /// Hash of the state root.
    pub state_root_hash: Digest,
    /// `casper_types::Key` as formatted string.
    pub key: String,
    /// The path components starting from the key as base.
    pub path: Vec<String>,
}

/// Result for "state_get_item" RPC response.
#[derive(Serialize, Deserialize, Debug)]
pub struct GetItemResult {
    /// The RPC API version.
    pub api_version: Version,
    /// The stored value.
    pub stored_value: StoredValue,
    /// The merkle proof.
    pub merkle_proof: String,
}

/// "state_get_item" RPC.
pub struct GetItem {}

impl RpcWithParams for GetItem {
    const METHOD: &'static str = "state_get_item";
    type RequestParams = GetItemParams;
    type ResponseResult = GetItemResult;
}

impl RpcWithParamsExt for GetItem {
    fn handle_request<REv: ReactorEventT>(
        effect_builder: EffectBuilder<REv>,
        response_builder: Builder,
        params: Self::RequestParams,
    ) -> BoxFuture<'static, Result<Response<Body>, Error>> {
        async move {
            // Try to parse a `casper_types::Key` from the params.
            let base_key = match Key::from_formatted_str(&params.key)
                .map_err(|error| format!("failed to parse key: {:?}", error))
            {
                Ok(key) => key,
                Err(error_msg) => {
                    info!("{}", error_msg);
                    return Ok(response_builder.error(warp_json_rpc::Error::custom(
                        ErrorCode::ParseQueryKey as i64,
                        error_msg,
                    ))?);
                }
            };

            // Run the query.
            let query_result = effect_builder
                .make_request(
                    |responder| ApiRequest::QueryGlobalState {
                        state_root_hash: params.state_root_hash,
                        base_key,
                        path: params.path,
                        responder,
                    },
                    QueueKind::Api,
                )
                .await;

            // Extract the EE `(StoredValue, Vec<TrieMerkleProof<Key, StoredValue>>)` from the
            // result.
            let (value, proof) = match query_result {
                Ok(QueryResult::Success { value, proofs }) => (value, proofs),
                Ok(query_result) => {
                    let error_msg = format!("state query failed: {:?}", query_result);
                    info!("{}", error_msg);
                    return Ok(response_builder.error(warp_json_rpc::Error::custom(
                        ErrorCode::QueryFailed as i64,
                        error_msg,
                    ))?);
                }
                Err(error) => {
                    let error_msg = format!("state query failed to execute: {:?}", error);
                    info!("{}", error_msg);
                    return Ok(response_builder.error(warp_json_rpc::Error::custom(
                        ErrorCode::QueryFailedToExecute as i64,
                        error_msg,
                    ))?);
                }
            };

            let value_compat = match StoredValue::try_from(&*value) {
                Ok(value_compat) => value_compat,
                Err(error) => {
                    info!("failed to encode stored value: {}", error);
                    return Ok(response_builder.error(warp_json_rpc::Error::INTERNAL_ERROR)?);
                }
            };

            let proof_bytes = match proof.to_bytes() {
                Ok(proof_bytes) => proof_bytes,
                Err(error) => {
                    info!("failed to encode stored value: {}", error);
                    return Ok(response_builder.error(warp_json_rpc::Error::INTERNAL_ERROR)?);
                }
            };

            let result = Self::ResponseResult {
                api_version: CLIENT_API_VERSION.clone(),
                stored_value: value_compat,
                merkle_proof: hex::encode(proof_bytes),
            };

            Ok(response_builder.success(result)?)
        }
        .boxed()
    }
}

/// Params for "state_get_balance" RPC request.
#[derive(Serialize, Deserialize, Debug)]
pub struct GetBalanceParams {
    /// The hash of state root.
    pub state_root_hash: Digest,
    /// Formatted URef.
    pub purse_uref: String,
}

/// Result for "state_get_balance" RPC response.
#[derive(Serialize, Deserialize, Debug)]
pub struct GetBalanceResult {
    /// The RPC API version.
    pub api_version: Version,
    /// The balance value.
    pub balance_value: U512,
    /// The merkle proof.
    pub merkle_proof: String,
}

/// "state_get_balance" RPC.
pub struct GetBalance {}

impl RpcWithParams for GetBalance {
    const METHOD: &'static str = "state_get_balance";
    type RequestParams = GetBalanceParams;
    type ResponseResult = GetBalanceResult;
}

impl RpcWithParamsExt for GetBalance {
    fn handle_request<REv: ReactorEventT>(
        effect_builder: EffectBuilder<REv>,
        response_builder: Builder,
        params: Self::RequestParams,
    ) -> BoxFuture<'static, Result<Response<Body>, Error>> {
        async move {
            // Try to parse the purse's URef from the params.
            let purse_uref = match URef::from_formatted_str(&params.purse_uref)
                .map_err(|error| format!("failed to parse purse_uref: {:?}", error))
            {
                Ok(uref) => uref,
                Err(error_msg) => {
                    info!("{}", error_msg);
                    return Ok(response_builder.error(warp_json_rpc::Error::custom(
                        ErrorCode::ParseGetBalanceURef as i64,
                        error_msg,
                    ))?);
                }
            };

            // Get the balance.
            let balance_result = effect_builder
                .make_request(
                    |responder| ApiRequest::GetBalance {
                        state_root_hash: params.state_root_hash,
                        purse_uref,
                        responder,
                    },
                    QueueKind::Api,
                )
                .await;

            let (balance_value, main_purse_proof, balance_proof) = match balance_result {
                Ok(BalanceResult::Success {
                    motes,
                    main_purse_proof,
                    balance_proof,
                }) => (motes, main_purse_proof, balance_proof),
                Ok(balance_result) => {
                    let error_msg = format!("get-balance failed: {:?}", balance_result);
                    info!("{}", error_msg);
                    return Ok(response_builder.error(warp_json_rpc::Error::custom(
                        ErrorCode::GetBalanceFailed as i64,
                        error_msg,
                    ))?);
                }
                Err(error) => {
                    let error_msg = format!("get-balance failed to execute: {}", error);
                    info!("{}", error_msg);
                    return Ok(response_builder.error(warp_json_rpc::Error::custom(
                        ErrorCode::GetBalanceFailedToExecute as i64,
                        error_msg,
                    ))?);
                }
            };

            let proof_bytes = match (*main_purse_proof, *balance_proof).to_bytes() {
                Ok(proof_bytes) => proof_bytes,
                Err(error) => {
                    info!("failed to encode stored value: {}", error);
                    return Ok(response_builder.error(warp_json_rpc::Error::INTERNAL_ERROR)?);
                }
            };

            let merkle_proof = hex::encode(proof_bytes);

            // Return the result.
            let result = Self::ResponseResult {
                api_version: CLIENT_API_VERSION.clone(),
                balance_value,
                merkle_proof,
            };
            Ok(response_builder.success(result)?)
        }
        .boxed()
    }
}

// auction info

/// Params for "state_get_auction_info" RPC request.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct GetAuctionInfoParams {}

/// Result for "state_get_auction_info" RPC response.
#[derive(Serialize, Deserialize, Debug)]
pub struct GetAuctionInfoResult {
    /// The RPC API version.
    pub api_version: Version,
    /// The auction state.
    pub auction_state: AuctionState,
}

/// "state_get_auction_info" RPC.
pub struct GetAuctionInfo {}

impl RpcWithParams for GetAuctionInfo {
    const METHOD: &'static str = "state_get_auction_info";
    type RequestParams = GetAuctionInfoParams;
    type ResponseResult = GetAuctionInfoResult;
}

impl RpcWithParamsExt for GetAuctionInfo {
    fn handle_request<REv: ReactorEventT>(
        effect_builder: EffectBuilder<REv>,
        response_builder: Builder,
        _params: Self::RequestParams,
    ) -> BoxFuture<'static, Result<Response<Body>, Error>> {
        async move {
            let block: Block = {
                let maybe_block = effect_builder
                    .make_request(
                        |responder| ApiRequest::GetBlock {
                            maybe_hash: None,
                            responder,
                        },
                        QueueKind::Api,
                    )
                    .await;

                match maybe_block {
                    None => {
                        let error_msg =
                            "get-auction-info failed to get last added block".to_string();
                        info!("{}", error_msg);
                        return Ok(response_builder.error(warp_json_rpc::Error::custom(
                            ErrorCode::NoSuchBlock as i64,
                            error_msg,
                        ))?);
                    }
                    Some(block) => block,
                }
            };

            let protocol_version = ProtocolVersion::V1_0_0;
            let protocol_version_result = effect_builder
                .make_request(
                    |responder| ApiRequest::QueryProtocolData {
                        protocol_version,
                        responder,
                    },
                    QueueKind::Api,
                )
                .await;

            let protocol_data = {
                if let Ok(Some(protocol_data)) = protocol_version_result {
                    protocol_data
                } else {
                    Box::new(ProtocolData::default())
                }
            };

            // auction contract key
            let base_key = protocol_data.auction().into();
            // bids named key in auction contract
            let path = vec![casper_types::auction::BIDS_KEY.to_string()];
            // the global state hash of the last block
            let state_root_hash = *block.header().state_root_hash();
            // the era of the last block
            let era_id = block.header().era_id().0;

            let query_result = effect_builder
                .make_request(
                    |responder| ApiRequest::QueryGlobalState {
                        state_root_hash,
                        base_key,
                        path,
                        responder,
                    },
                    QueueKind::Api,
                )
                .await;

            let bids = if let Ok(QueryResult::Success { value, .. }) = query_result {
                value
                    .as_cl_value()
                    .and_then(|cl_value| cl_value.to_owned().into_t().ok())
            } else {
                None
            };

            let era_validators_result = effect_builder
                .make_request(
                    |responder| ApiRequest::QueryEraValidators {
                        state_root_hash,
                        era_id,
                        protocol_version,
                        responder,
                    },
                    QueueKind::Api,
                )
                .await;

            let validator_weights = era_validators_result.ok().flatten();

            let auction_state = AuctionState::new(state_root_hash, era_id, bids, validator_weights);
            debug!("responding to client with: {:?}", auction_state);
            Ok(response_builder.success(auction_state)?)
        }
        .boxed()
    }
}
