use anyhow::Context;
use pathfinder_common::TransactionHash;
use starknet_gateway_types::pending::PendingData;

use crate::context::RpcContext;

#[derive(serde::Deserialize, Debug, PartialEq, Eq)]
pub struct GetGatewayTransactionInput {
    transaction_hash: TransactionHash,
}

crate::error::generate_rpc_error_subset!(GetGatewayTransactionError:);

pub async fn get_transaction_status(
    context: RpcContext,
    input: GetGatewayTransactionInput,
) -> Result<TransactionStatus, GetGatewayTransactionError> {
    // Check in pending block.
    if let Some(pending) = &context.pending_data {
        if is_pending_tx(pending, &input.transaction_hash).await {
            return Ok(TransactionStatus::Pending);
        }
    }

    // Check database.
    let span = tracing::Span::current();

    let db_status = tokio::task::spawn_blocking(move || {
        let _g = span.enter();

        let mut db = context
            .storage
            .connection()
            .context("Opening database connection")?;
        let db_tx = db.transaction().context("Creating database transaction")?;
        let block_hash = db_tx
            .transaction_block_hash(input.transaction_hash)
            .context("Fetching transaction block hash from database")?;

        let Some(block_hash) = block_hash else {
            return Ok(None);
        };

        let tx_status = db_tx
            .block_is_l1_accepted(block_hash.into())
            .context("Quering block's status")?;

        anyhow::Ok(Some(tx_status))
    })
    .await
    .context("Joining database task")??;

    match db_status {
        Some(true) => return Ok(TransactionStatus::AcceptedOnL1),
        Some(false) => return Ok(TransactionStatus::AcceptedOnL2),
        None => {}
    }

    // Check gateway for rejected transactions.
    use starknet_gateway_client::GatewayApi;
    context
        .sequencer
        .transaction(input.transaction_hash)
        .await
        .context("Fetching transaction from gateway")
        .map(|tx| tx.status.into())
        .map_err(GetGatewayTransactionError::Internal)
}

async fn is_pending_tx(pending: &PendingData, tx_hash: &TransactionHash) -> bool {
    pending
        .block()
        .await
        .map(|block| block.transactions.iter().any(|tx| &tx.hash() == tx_hash))
        .unwrap_or_default()
}

#[derive(Copy, Clone, Debug, serde::Serialize, PartialEq)]
pub enum TransactionStatus {
    #[serde(rename = "NOT_RECEIVED")]
    NotReceived,
    #[serde(rename = "RECEIVED")]
    Received,
    #[serde(rename = "PENDING")]
    Pending,
    #[serde(rename = "REJECTED")]
    Rejected,
    #[serde(rename = "ACCEPTED_ON_L1")]
    AcceptedOnL1,
    #[serde(rename = "ACCEPTED_ON_L2")]
    AcceptedOnL2,
    #[serde(rename = "REVERTED")]
    Reverted,
    #[serde(rename = "ABORTED")]
    Aborted,
}

impl From<starknet_gateway_types::reply::Status> for TransactionStatus {
    fn from(value: starknet_gateway_types::reply::Status) -> Self {
        use starknet_gateway_types::reply::Status;
        match value {
            Status::NotReceived => Self::NotReceived,
            Status::Received => Self::Received,
            Status::Pending => Self::Pending,
            Status::Rejected => Self::Rejected,
            Status::AcceptedOnL1 => Self::AcceptedOnL1,
            Status::AcceptedOnL2 => Self::AcceptedOnL2,
            Status::Reverted => Self::Reverted,
            Status::Aborted => Self::Aborted,
        }
    }
}

#[cfg(test)]
mod tests {
    use pathfinder_common::{felt, felt_bytes};

    use super::*;

    #[tokio::test]
    async fn l1_accepted() {
        let context = RpcContext::for_tests();
        // This transaction is in block 0 which is L1 accepted.
        let tx_hash = TransactionHash(felt_bytes!(b"txn 0"));
        let input = GetGatewayTransactionInput {
            transaction_hash: tx_hash,
        };
        let status = get_transaction_status(context, input).await.unwrap();

        assert_eq!(status, TransactionStatus::AcceptedOnL1);
    }

    #[tokio::test]
    async fn l2_accepted() {
        let context = RpcContext::for_tests();
        // This transaction is in block 1 which is not L1 accepted.
        let tx_hash = TransactionHash(felt_bytes!(b"txn 1"));
        let input = GetGatewayTransactionInput {
            transaction_hash: tx_hash,
        };
        let status = get_transaction_status(context, input).await.unwrap();

        assert_eq!(status, TransactionStatus::AcceptedOnL2);
    }

    #[tokio::test]
    async fn pending() {
        let context = RpcContext::for_tests_with_pending().await;
        let tx_hash = TransactionHash(felt_bytes!(b"pending tx hash 0"));
        let input = GetGatewayTransactionInput {
            transaction_hash: tx_hash,
        };
        let status = get_transaction_status(context, input).await.unwrap();

        assert_eq!(status, TransactionStatus::Pending);
    }

    #[tokio::test]
    async fn rejected() {
        let input = GetGatewayTransactionInput {
            transaction_hash: TransactionHash(felt!(
                // Transaction hash known to be rejected by the testnet gateway.
                "0x07c64b747bdb0831e7045925625bfa6309c422fded9527bacca91199a1c8d212"
            )),
        };
        let context = RpcContext::for_tests();
        let status = get_transaction_status(context, input).await.unwrap();

        assert_eq!(status, TransactionStatus::Rejected);
    }
}
