// src/types.rs
use alloy::primitives::{Address, B256, U256};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DexType { UniswapV2, UniswapV3 }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FeeTier { V2_30, V3_5, V3_30, V3_100 }

impl FeeTier {
    pub fn bps(&self) -> u64 {
        match self { Self::V2_30 => 30, Self::V3_5 => 5, Self::V3_30 => 30, Self::V3_100 => 100 }
    }
    pub fn fee_numerator(&self) -> u64 { 1000 - self.bps() / 10 }
    pub fn multiplier(&self) -> f64 { 1.0 - (self.bps() as f64 / 10_000.0) }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolStateV2 {
    pub address:      Address,
    pub token0:       Address,
    pub token1:       Address,
    pub reserve0:     U256,
    pub reserve1:     U256,
    pub fee_tier:     FeeTier,
    pub dex:          DexType,
    pub last_updated: DateTime<Utc>,
    pub block_number: u64,
}

impl PoolStateV2 {
    pub fn has_sufficient_liquidity(&self) -> bool {
        !self.reserve0.is_zero() && !self.reserve1.is_zero()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteStep {
    pub pool_address: Address,
    pub token_in:     Address,
    pub token_out:    Address,
    pub fee_tier:     FeeTier,
    pub dex_type:     DexType,
    pub reserve_in:   U256,
    pub reserve_out:  U256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbitrageRoute {
    pub id:              Uuid,
    pub start_token:     Address,
    pub steps:           Vec<RouteStep>,
    pub optimal_input:   U256,
    pub expected_output: U256,
    pub expected_profit: U256,
    pub profit_usd:      f64,
    pub gas_cost_usd:    f64,
    pub net_profit_usd:  f64,
    pub calculated_at:   DateTime<Utc>,
    pub block_number:    u64,
}

impl ArbitrageRoute {
    pub fn hop_count(&self) -> usize { self.steps.len() }
    pub fn is_profitable(&self) -> bool { self.net_profit_usd > 0.0 }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutionStatus {
    Pending,
    Submitted { tx_hash: String },
    Confirmed { tx_hash: String, block: u64, actual_profit_usd: f64 },
    Reverted  { tx_hash: String, reason: String },
    Failed    { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeResult {
    pub id:              Uuid,
    pub route:           ArbitrageRoute,
    pub status:          ExecutionStatus,
    pub gas_used:        Option<u64>,
    pub gas_price_gwei:  Option<f64>,
    pub executed_at:     DateTime<Utc>,
    pub simulation_mode: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct BotMetrics {
    pub total_opportunities_found: u64,
    pub total_trades_executed:     u64,
    pub total_trades_successful:   u64,
    pub total_trades_reverted:     u64,
    pub total_profit_usd:          f64,
    pub total_gas_cost_usd:        f64,
    pub net_profit_usd:            f64,
    pub avg_profit_per_trade_usd:  f64,
    pub success_rate:              f64,
    pub pools_monitored:           u64,
    pub blocks_processed:          u64,
    pub last_updated:              Option<DateTime<Utc>>,
}

impl BotMetrics {
    pub fn update_derived(&mut self) {
        self.net_profit_usd = self.total_profit_usd - self.total_gas_cost_usd;
        if self.total_trades_successful > 0 {
            self.avg_profit_per_trade_usd =
                self.net_profit_usd / self.total_trades_successful as f64;
        }
        if self.total_trades_executed > 0 {
            self.success_rate =
                self.total_trades_successful as f64 / self.total_trades_executed as f64 * 100.0;
        }
        self.last_updated = Some(Utc::now());
    }
}

#[derive(Debug, Clone)]
pub struct SyncEvent {
    pub pool_address: Address,
    pub reserve0:     U256,
    pub reserve1:     U256,
    pub block_number: u64,
    pub tx_hash:      B256,
    pub received_at:  DateTime<Utc>,
}