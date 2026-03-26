// src/main.rs
mod config;
mod executor;
mod graph;
mod math;
mod monitor;
mod types;

use config::BotConfig;
use executor::{TradeExecutor, TradeLogger};
use graph::{ArbitrageGraph, BellmanFordDetector};
use math::to_raw_amount;
use monitor::{DexMonitor, PoolRegistry};
use types::{BotMetrics, DexType, FeeTier, SyncEvent};

use alloy::primitives::Address;
use anyhow::Result;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{Duration, interval};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

fn init_logging() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}

fn token_names(cfg: &BotConfig) -> HashMap<Address, String> {
    let mut m = HashMap::new();
    m.insert(cfg.tokens.wmatic, "WMATIC".into());
    m.insert(cfg.tokens.weth,   "WETH".into());
    m.insert(cfg.tokens.usdc,   "USDC".into());
    m.insert(cfg.tokens.usdt,   "USDT".into());
    m.insert(cfg.tokens.dai,    "DAI".into());
    m.insert(cfg.tokens.wbtc,   "WBTC".into());
    m
}

fn token_decimals(cfg: &BotConfig) -> HashMap<Address, u8> {
    let mut m = HashMap::new();
    m.insert(cfg.tokens.wmatic, 18u8);
    m.insert(cfg.tokens.weth,   18u8);
    m.insert(cfg.tokens.usdc,    6u8);
    m.insert(cfg.tokens.usdt,    6u8);
    m.insert(cfg.tokens.dai,    18u8);
    m.insert(cfg.tokens.wbtc,    8u8);
    m
}

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();

    let cfg = BotConfig::from_env()?;
    print_banner(&cfg);

    let ws_url = if cfg.simulation.is_simulation {
        cfg.simulation.anvil_ws_url.clone()
    } else {
        cfg.network.ws_url.clone()
    };

    let http_url = if cfg.simulation.is_simulation {
        cfg.simulation.anvil_rpc_url.clone()
    } else {
        cfg.network.http_url.clone()
    };

    // ── Shared State ──────────────────────────────────────────────────────────
    let metrics = Arc::new(Mutex::new(BotMetrics {
        last_updated: Some(Utc::now()),
        ..Default::default()
    }));

    let pool_registry = PoolRegistry::new();
    let base_tokens   = cfg.tokens.base_tokens();
    let graph         = ArbitrageGraph::new(base_tokens.clone());
    let (event_tx, mut event_rx) = mpsc::channel::<SyncEvent>(10_000);

    let _names    = token_names(&cfg);
    let _decimals = token_decimals(&cfg);

 // ── Executor ──────────────────────────────────────────────────────────────
    let _executor = Arc::new(TradeExecutor::new(
        cfg.network.contract_addr,
        http_url.clone(),
        cfg.network.private_key.clone(),
        cfg.simulation.is_simulation,
        metrics.clone(),
    ));

    let logger = Arc::new(TradeLogger::new("logs/trades.jsonl".to_string()));

    // ── Factories ─────────────────────────────────────────────────────────────
    let factories = vec![
        (cfg.dex.quickswap_v2_factory, DexType::UniswapV2, FeeTier::V2_30),
        (cfg.dex.sushiswap_factory,    DexType::UniswapV2, FeeTier::V2_30),
    ];

    // ── Monitor ───────────────────────────────────────────────────────────────
    let monitor = Arc::new(DexMonitor::new(
        ws_url,
        http_url,
        pool_registry.clone(),
        graph.clone(),
        event_tx,
        factories,
        base_tokens.clone(),
    ));

    // ── Detector ──────────────────────────────────────────────────────────────
    let detector = Arc::new(BellmanFordDetector::new(
        graph.clone(),
        cfg.strategy.max_hops,
        0.001,
    ));

    let max_input_raw = to_raw_amount(cfg.strategy.max_input_usd, 6);

    // ── Tasks ─────────────────────────────────────────────────────────────────

    let monitor_task = {
        let monitor = monitor.clone();
        tokio::spawn(async move {
            if let Err(e) = monitor.run().await {
                error!("Monitor error: {e}");
            }
        })
    };

    let scan_ms  = cfg.strategy.scan_interval_ms;
    let min_pnl  = cfg.strategy.min_profit_usd;
    let detect_task = {
        let detector  = detector.clone();
        let metrics   = metrics.clone();
        tokio::spawn(async move {
            let _ticker = interval(Duration::from_millis(scan_ms));
            let mut block: u64   = 0;
            let mut last_scan    = std::time::Instant::now();

            while let Some(_ev) = event_rx.recv().await {
                let now = std::time::Instant::now();
                if now.duration_since(last_scan).as_millis() < scan_ms as u128 {
                    continue;
                }
                last_scan = now;

                let cycles = detector.find_arbitrage_cycles(block, 10);
                if cycles.is_empty() { block += 1; continue; }

                info!("{} siklus terdeteksi di block {}", cycles.len(), block);

                for cycle in &cycles {
                    let profit_pct = (-cycle.total_log_weight).exp() - 1.0;
                    let profit_est = profit_pct * max_input_raw.to::<u128>() as f64 / 1e6 * 0.10;

                    if profit_est < min_pnl { continue; }

                    let mut m = metrics.lock().await;
                    m.total_opportunities_found += 1;
                    drop(m);

                    info!(
                        "Peluang: profit est ${:.4} | log_w {:.6}",
                        profit_est, cycle.total_log_weight
                    );
                }
                block += 1;
            }
        })
    };

    let metrics_task = {
        let metrics       = metrics.clone();
        let pool_registry = pool_registry.clone();
        let logger        = logger.clone();
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(30));
            loop {
                ticker.tick().await;
                let mut m = metrics.lock().await;
                m.pools_monitored = pool_registry.pool_count() as u64;
                m.update_derived();
                info!(
                    "Metrics | Pools:{} | Events:{} | Opp:{} | PnL:${:.2}",
                    m.pools_monitored,
                    pool_registry.events_count(),
                    m.total_opportunities_found,
                    m.net_profit_usd
                );
                drop(m);

                if let Ok(stats) = logger.compute_stats().await {
                    if stats.total_trades > 0 {
                        info!(
                            "Trades: {} total | {} ok ({:.1}%) | ${:.2}",
                            stats.total_trades,
                            stats.successful,
                            stats.success_rate,
                            stats.total_profit_usd
                        );
                    }
                }
            }
        })
    };

    if cfg.simulation.is_simulation {
        info!("Mode SIMULASI | MATIC:{:.0} | USDC:{:.0}",
            cfg.simulation.simulation_matic_balance,
            cfg.simulation.simulation_usdc_balance);
    }

    tokio::select! {
        _ = monitor_task   => warn!("Monitor selesai"),
        _ = detect_task    => warn!("Detector selesai"),
        _ = metrics_task   => warn!("Metrics selesai"),
        _ = tokio::signal::ctrl_c() => info!("Ctrl+C - shutdown"),
    }

    info!("Bot berhenti.");
    Ok(())
}

fn print_banner(cfg: &BotConfig) {
    let mode = if cfg.simulation.is_simulation { "SIMULASI" } else { "LIVE" };
    println!("==============================================");
    println!("  MEV Multi-Hop Arbitrage Bot v0.1.0");
    println!("  Rust + Alloy | Polygon Mainnet");
    println!("----------------------------------------------");
    println!("  Mode:       {}", mode);
    println!("  Min Profit: ${:.2}", cfg.strategy.min_profit_usd);
    println!("  Max Input:  ${:.2}", cfg.strategy.max_input_usd);
    println!("  Max Hops:   {}", cfg.strategy.max_hops);
    println!("==============================================");
}