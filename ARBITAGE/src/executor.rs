// src/executor.rs
use crate::telegram::{TelegramMsg, TelegramNotifier};
use crate::types::{
    ArbitrageRoute, BotMetrics, ExecutionStatus, TradeResult,
};
use alloy::{
    network::TransactionBuilder,
    primitives::{Address, U256},
    providers::{Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
    network::EthereumWallet,
    sol,
    sol_types::SolCall,
    rpc::types::TransactionRequest,
};
use anyhow::Result;
use chrono::Utc;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, warn};
use uuid::Uuid;

// ── Contract ABI ──────────────────────────────────────────────────────────────

sol! {
    #[derive(Debug)]
    interface IArbitrageExecutor {
        struct SwapStep {
            address pool;
            address tokenIn;
            address tokenOut;
            uint24  fee;
            bool    isV3;
        }

        function executeArbitrage(
            SwapStep[] calldata steps,
            uint256 amountIn,
            uint256 minProfit,
            uint256 deadline
        ) external returns (uint256 profit);
    }
}

// ── Execution Params ──────────────────────────────────────────────────────────

pub struct ExecutionParams {
    pub route:            ArbitrageRoute,
    pub min_profit_bps:   u64,
    pub gas_price_gwei:   f64,
    pub deadline_seconds: u64,
}

// ── TradeExecutor ─────────────────────────────────────────────────────────────

pub struct TradeExecutor {
    contract_address: Address,
    provider_url:     String,
    private_key:      String,
    simulation_mode:  bool,
    metrics:          Arc<Mutex<BotMetrics>>,
    tg:               Arc<TelegramNotifier>,
}

impl TradeExecutor {
    pub fn new(
        contract_address: Address,
        provider_url:     String,
        private_key:      String,
        simulation_mode:  bool,
        metrics:          Arc<Mutex<BotMetrics>>,
        tg:               Arc<TelegramNotifier>,
    ) -> Self {
        Self { contract_address, provider_url, private_key, simulation_mode, metrics, tg }
    }

    pub async fn execute(&self, params: ExecutionParams) -> TradeResult {
        let trade_id = Uuid::new_v4();
        let mode     = if self.simulation_mode { "[SIM]" } else { "[LIVE]" };

        info!(
            "{mode} Trade {trade_id}: {} hop | profit est. ${:.4}",
            params.route.hop_count(),
            params.route.net_profit_usd
        );

        let result = if self.simulation_mode {
            self.simulate_execution(&params, trade_id).await
        } else {
            self.live_execution(&params, trade_id).await
        };

        // Update metrics
        {
            let mut m = self.metrics.lock().await;
            m.total_trades_executed += 1;
            match &result.status {
                ExecutionStatus::Confirmed { actual_profit_usd, .. } => {
                    m.total_trades_successful += 1;
                    m.total_profit_usd        += actual_profit_usd;
                }
                ExecutionStatus::Reverted { .. } => {
                    m.total_trades_reverted += 1;
                }
                _ => {}
            }
            m.update_derived();
        }

        // Kirim notifikasi Telegram berdasarkan hasil
        self.notify_result(&result, mode).await;

        result
    }

    async fn notify_result(&self, result: &TradeResult, mode: &str) {
        match &result.status {
            ExecutionStatus::Confirmed { tx_hash, block, actual_profit_usd } => {
                let gas_cost = result.gas_used.unwrap_or(220_000) as f64
                    * result.gas_price_gwei.unwrap_or(50.0)
                    * 1e-9
                    * 0.80; // MATIC price approx
                self.tg.send_md(&TelegramMsg::trade_success(
                    tx_hash,
                    *actual_profit_usd,
                    gas_cost,
                    *block,
                    mode,
                )).await;
            }
            ExecutionStatus::Reverted { reason, .. } => {
                let input_usd = result.route.optimal_input.to::<u128>() as f64 / 1e6;
                self.tg.send_md(&TelegramMsg::trade_reverted(
                    reason, input_usd, mode
                )).await;
            }
            ExecutionStatus::Failed { reason } => {
                self.tg.send_md(&TelegramMsg::trade_failed(reason)).await;
            }
            _ => {}
        }
    }

    async fn simulate_execution(
        &self,
        params:   &ExecutionParams,
        trade_id: Uuid,
    ) -> TradeResult {
        let calldata = self.encode_calldata(params);

        let provider = match ProviderBuilder::new()
            .on_builtin(&self.provider_url).await
        {
            Ok(p)  => p,
            Err(e) => return self.failed_result(trade_id, params, format!("Provider: {e}")),
        };

        let tx = TransactionRequest::default()
            .with_to(self.contract_address)
            .with_input(calldata)
            .with_from(Address::ZERO);

        match provider.call(&tx).await {
            Ok(output) => {
                let profit = if output.len() >= 32 {
                    U256::from_be_slice(&output[..32]).to::<u128>() as f64 / 1e6
                } else { 0.0 };

                info!("[SIM] Trade {trade_id} SUKSES profit ${:.4}", profit);
                TradeResult {
                    id: trade_id,
                    route: params.route.clone(),
                    status: ExecutionStatus::Confirmed {
                        tx_hash: format!("sim-{trade_id}"),
                        block: 0,
                        actual_profit_usd: profit,
                    },
                    gas_used:        Some(220_000),
                    gas_price_gwei:  Some(params.gas_price_gwei),
                    executed_at:     Utc::now(),
                    simulation_mode: true,
                }
            }
            Err(e) => {
                warn!("[SIM] Trade {trade_id} REVERT: {e}");
                TradeResult {
                    id: trade_id,
                    route: params.route.clone(),
                    status: ExecutionStatus::Reverted {
                        tx_hash: format!("sim-{trade_id}"),
                        reason:  e.to_string(),
                    },
                    gas_used:        Some(30_000),
                    gas_price_gwei:  Some(params.gas_price_gwei),
                    executed_at:     Utc::now(),
                    simulation_mode: true,
                }
            }
        }
    }

    async fn live_execution(
        &self,
        params:   &ExecutionParams,
        trade_id: Uuid,
    ) -> TradeResult {
        let signer: PrivateKeySigner = match self.private_key.parse() {
            Ok(s)  => s,
            Err(e) => return self.failed_result(trade_id, params, format!("Bad key: {e}")),
        };

        let wallet   = EthereumWallet::from(signer);
        let provider = match ProviderBuilder::new()
            .wallet(wallet)
            .on_builtin(&self.provider_url).await
        {
            Ok(p)  => p,
            Err(e) => return self.failed_result(trade_id, params, format!("Provider: {e}")),
        };

        let gas_price_wei = (params.gas_price_gwei * 1e9) as u128;
        let calldata      = self.encode_calldata(params);

        let tx = TransactionRequest::default()
            .with_to(self.contract_address)
            .with_input(calldata)
            .with_gas_price(gas_price_wei);

        match provider.send_transaction(tx).await {
            Ok(pending) => {
                let tx_hash = pending.tx_hash().to_string();
                info!("[LIVE] Tx terkirim: {tx_hash}");

                match pending.get_receipt().await {
                    Ok(receipt) => {
                        let block    = receipt.block_number.unwrap_or(0);
                        let gas_used = Some(receipt.gas_used as u64);

                        if receipt.status() {
                            info!("[LIVE] Confirmed di block {block}");
                            TradeResult {
                                id: trade_id,
                                route: params.route.clone(),
                                status: ExecutionStatus::Confirmed {
                                    tx_hash,
                                    block,
                                    actual_profit_usd: params.route.net_profit_usd,
                                },
                                gas_used,
                                gas_price_gwei: Some(params.gas_price_gwei),
                                executed_at:    Utc::now(),
                                simulation_mode: false,
                            }
                        } else {
                            warn!("[LIVE] Trade {trade_id} REVERTED!");
                            TradeResult {
                                id: trade_id,
                                route: params.route.clone(),
                                status: ExecutionStatus::Reverted {
                                    tx_hash,
                                    reason: "Reverted on-chain".to_string(),
                                },
                                gas_used,
                                gas_price_gwei: Some(params.gas_price_gwei),
                                executed_at:    Utc::now(),
                                simulation_mode: false,
                            }
                        }
                    }
                    Err(e) => self.failed_result(trade_id, params, format!("Receipt: {e}")),
                }
            }
            Err(e) => self.failed_result(trade_id, params, format!("SendTx: {e}")),
        }
    }

    fn encode_calldata(&self, params: &ExecutionParams) -> Vec<u8> {
        // Konversi fee dari bps ke Uniswap fee unit (bps * 10 = fee ppm)
        // V2: 30 bps = 300 fee units (0.3%)
        // V3: 5 bps = 500, 30 bps = 3000, 100 bps = 10000
        let steps: Vec<IArbitrageExecutor::SwapStep> = params.route.steps.iter()
            .map(|s| {
                let fee_val = match s.fee_tier {
                    crate::types::FeeTier::V2_30  => 3000u32,  // 0.3%
                    crate::types::FeeTier::V3_5   => 500u32,   // 0.05%
                    crate::types::FeeTier::V3_30  => 3000u32,  // 0.3%
                    crate::types::FeeTier::V3_100 => 10000u32, // 1%
                };
                IArbitrageExecutor::SwapStep {
                    pool:     s.pool_address,
                    tokenIn:  s.token_in,
                    tokenOut: s.token_out,
                    fee:      alloy::primitives::Uint::<24, 1>::from(fee_val),
                    isV3:     matches!(s.dex_type, crate::types::DexType::UniswapV3),
                }
            })
            .collect();

        let min_profit = params.route.optimal_input
            * U256::from(params.min_profit_bps)
            / U256::from(10_000u64);

        let deadline = U256::from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                + params.deadline_seconds
        );

        IArbitrageExecutor::executeArbitrageCall {
            steps,
            amountIn:  params.route.optimal_input,
            minProfit: min_profit,
            deadline,
        }.abi_encode()
    }

    fn failed_result(&self, id: Uuid, params: &ExecutionParams, reason: String) -> TradeResult {
        error!("Trade {id} FAILED: {reason}");
        TradeResult {
            id,
            route: params.route.clone(),
            status: ExecutionStatus::Failed { reason },
            gas_used: None,
            gas_price_gwei: None,
            executed_at: Utc::now(),
            simulation_mode: self.simulation_mode,
        }
    }
}

// ── Trade Logger ──────────────────────────────────────────────────────────────

pub struct TradeLogger {
    log_path: String,
}

impl TradeLogger {
    pub fn new(log_path: String) -> Self { Self { log_path } }

    pub async fn log_trade(&self, result: &TradeResult) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        // Buat parent dir jika belum ada
        if let Some(parent) = std::path::Path::new(&self.log_path).parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true).append(true)
            .open(&self.log_path).await?;
        file.write_all(serde_json::to_string(result)?.as_bytes()).await?;
        file.write_all(b"\n").await?;
        Ok(())
    }

    pub async fn compute_stats(&self) -> Result<TradeStats> {
        let content = tokio::fs::read_to_string(&self.log_path).await
            .unwrap_or_default();
        let mut stats = TradeStats::default();
        for line in content.lines().filter(|l| !l.is_empty()) {
            if let Ok(r) = serde_json::from_str::<TradeResult>(line) {
                stats.total_trades += 1;
                if let ExecutionStatus::Confirmed { actual_profit_usd, .. } = &r.status {
                    stats.successful       += 1;
                    stats.total_profit_usd += actual_profit_usd;
                } else if matches!(r.status, ExecutionStatus::Reverted { .. }) {
                    stats.reverted += 1;
                }
            }
        }
        if stats.total_trades > 0 {
            stats.success_rate = stats.successful as f64 / stats.total_trades as f64 * 100.0;
        }
        Ok(stats)
    }
}

#[derive(Debug, Default)]
pub struct TradeStats {
    pub total_trades:     u64,
    pub successful:       u64,
    pub reverted:         u64,
    pub total_profit_usd: f64,
    pub success_rate:     f64,
}
