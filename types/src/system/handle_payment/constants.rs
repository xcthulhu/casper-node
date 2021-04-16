/// Named constant for `purse`.
pub const ARG_PURSE: &str = "purse";
/// Named constant for `amount`.
pub const ARG_AMOUNT: &str = "amount";
/// Named constant for `source`.
pub const ARG_ACCOUNT: &str = "account";
/// Named constant for `target`.
pub const ARG_TARGET: &str = "target";

/// Named constant for method `get_payment_purse`.
pub const METHOD_GET_PAYMENT_PURSE: &str = "get_payment_purse";
/// Named constant for method `set_refund_purse`.
pub const METHOD_SET_REFUND_PURSE: &str = "set_refund_purse";
/// Named constant for method `finalize_payment`.
pub const METHOD_FINALIZE_PAYMENT: &str = "finalize_payment";

/// Storage for handle payment contract hash.
pub const CONTRACT_HASH_KEY: &str = "contract_hash";

/// Storage for handle payment access key.
pub const CONTRACT_ACCESS_KEY: &str = "access_key";

/// The uref name where the Handle Payment accepts payment for computation on behalf of validators.
pub const PAYMENT_PURSE_KEY: &str = "payment_purse";

/// The uref name where the Handle Payment will refund unused payment back to the user. The uref
/// this name corresponds to is set by the user.
pub const REFUND_PURSE_KEY: &str = "refund_purse";
