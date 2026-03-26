// src/main.rs
mod config;
mod executor;
mod graph;
mod math;
mod monitor;
mod telegram;
mod types;

use config::BotConfig;
use executor::{ExecutionParams, TradeExecutor, TradeLogger};
use graph::{ArbitrageGraph, BellmanFordDetector};
use math::to_raw_amount;
use monitor::{DexMonitor, PoolRegistry};
use telegram::{TelegramConfig, TelegramMsg, TelegramNotifier};
use types::{BotMetrics, DexType, FeeTier, SyncEvent};

use alloy::primitives::Address;
use anyhow::Result;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
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

fn token_names(cfg: &BotConfig) -> HashMap<Address, &'static str> {
    let mut m = HashMap::new();
    m.insert(cfg.tokens.wmatic, "WMATIC");
    m.insert(cfg.tokens.weth,   "WETH");
    m.insert(cfg.tokens.usdc,   "USDC");
    m.insert(cfg.tokens.usdt,   "USDT");
    m.insert(cfg.tokens.dai,    "DAI");
    m.insert(cfg.tokens.wbtc,   "WBTC");
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

    // Buat direktori logs jika belum ada
    tokio::fs::create_dir_all("logs").await.ok();

    let cfg      = BotConfig::from_env()?;
    let tg_cfg   = TelegramConfig::from_env();
    let tg       = Arc::new(TelegramNotifier::new(tg_cfg));

    print_banner(&cfg);

    // Kirim notifikasi startup
    let mode_str = if cfg.simulation.is_simulation { "SIMULASI" } else { "LIVE" };
    tg.send_md(&TelegramMsg::startup(
        mode_str,
        cfg.strategy.min_profit_usd,
        cfg.strategy.max_input_usd,
    )).await;

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
    let start_time = Instant::now();
    let metrics = Arc::new(Mutex::new(BotMetrics {
        last_updated: Some(Utc::now()),
        ..Default::default()
    }));

    let pool_registry = PoolRegistry::new();
    let base_tokens   = cfg.tokens.base_tokens();
    let graph         = ArbitrageGraph::new(base_tokens.clone());
    let (event_tx, mut event_rx) = mpsc::channel::<SyncEvent>(10_000);

    let names    = token_names(&cfg);
    let decimals = token_decimals(&cfg);

    // ── Executor ──────────────────────────────────────────────────────────────
    let executor = Arc::new(TradeExecutor::new(
        cfg.network.contract_addr,
        http_url.clone(),
        cfg.network.private_key.clone(),
        cfg.simulation.is_simulation,
        metrics.clone(),
        tg.clone(),
    ));

    let logger = Arc::new(TradeLogger::new("logs/trades.jsonl".to_string()));

    // ── Factories ─────────────────────────────────────────────────────────────
    let factories = vec![
        (cfg.dex.quickswap_v2_factory, DexType::UniswapV2, FeeTier::V2_30),
        (cfg.dex.sushiswap_factory,    DexType::UniswapV2, FeeTier::V2_30),
    ];

    // ── Monitor ───────────────────────────────────────────────────────────────
    let monitor = Arc::new(DexMonitor::new(
        ws_url.clone(),
        http_url.clone(),
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

    // ── TASK 1: WebSocket Monitor ─────────────────────────────────────────────
    let monitor_task = {
        let monitor = monitor.clone();
        let tg      = tg.clone();
        let ws_url2 = ws_url.clone();
        tokio::spawn(async move {
            let mut retry   = 0u32;
            let mut backoff = 2u64;

            loop {
                info!("Monitor attempt #{}", retry + 1);
                match monitor.run().await {
                    Ok(_) => {
                        warn!("Monitor selesai normal");
                        break;
                    }
                    Err(e) => {
                        retry += 1;
                        error!("Monitor error: {e}");
                        tg.send_md(&TelegramMsg::error(
                            "WebSocket Disconnect",
                            &format!("{e} | Retry #{retry} dalam {backoff}s")
                        )).await;

                        if retry >= 10 {
                            error!("Terlalu banyak retry, berhenti.");
                            break;
                        }

                        tokio::time::sleep(Duration::from_secs(backoff)).await;
                        backoff = (backoff * 2).min(60);
                    }
                }
            }
        })
    };

    // ── TASK 2: Arbitrage Detection + Execution ───────────────────────────────
    let scan_ms  = cfg.strategy.scan_interval_ms;
    let min_pnl  = cfg.strategy.min_profit_usd;
    let max_hops = cfg.strategy.max_hops;
    let is_sim   = cfg.simulation.is_simulation;

    let detect_task = {
        let detector  = detector.clone();
        let executor  = executor.clone();
        let logger    = logger.clone();
        let metrics   = metrics.clone();
        let tg        = tg.clone();
        // Clone names untuk dipakai di dalam closure
        let names_arc: Arc<HashMap<Address, &'static str>> = Arc::new(names);

        tokio::spawn(async move {
            let mut block: u64        = 0;
            let mut last_scan         = Instant::now();
            let mut last_exec         = Instant::now();
            // Anti-spam: jangan eksekusi lebih dari 1 trade per 5 detik
            let exec_cooldown = Duration::from_secs(5);

            while let Some(_ev) = event_rx.recv().await {
                let now = Instant::now();
                if now.duration_since(last_scan).as_millis() < scan_ms as u128 {
                    continue;
                }
                last_scan = now;

                let cycles = detector.find_arbitrage_cycles(block, 10);
                if cycles.is_empty() { block += 1; continue; }

                info!("{} siklus terdeteksi di block {}", cycles.len(), block);

                for cycle in &cycles {
                    let profit_pct = (-cycle.total_log_weight).exp() - 1.0;
                    // Estimasi profit dalam USD berdasarkan persentase dari max input
                    let profit_est = profit_pct
                        * max_input_raw.to::<u128>() as f64
                        / 1e6  // USDC 6 decimals
                        * 0.10; // gunakan 10% max input sebagai input awal estimasi

                    if profit_est < min_pnl { continue; }

                    // Update opportunities counter
                    {
                        let mut m = metrics.lock().await;
                        m.total_opportunities_found += 1;
                    }

                    info!(
                        "Peluang: profit est ${:.4} | log_w {:.6} | hops {}",
                        profit_est, cycle.total_log_weight, cycle.edges.len()
                    );

                    // Build route string dari cycle edges untuk Telegram
                    let route_str: String = {
                        let mut parts = Vec::new();
                        if let Some(first) = cycle.edges.first() {
                            let name = names_arc.get(&first.token_in)
                                .copied().unwrap_or("???");
                            parts.push(name);
                        }
                        for edge in &cycle.edges {
                            let name = names_arc.get(&edge.token_out)
                                .copied().unwrap_or("???");
                            parts.push(name);
                        }
                        parts.join(" -> ")
                    };

                    // Kirim notifikasi peluang ke Telegram
                    tg.send_md(&TelegramMsg::opportunity_found(
                        &route_str,
                        profit_est,
                        max_input_raw.to::<u128>() as f64 / 1e6 * 0.10,
                        cycle.edges.len(),
                        block,
                    )).await;

                    // ── EKSEKUSI TRADE ─────────────────────────────────────────
                    // Cek cooldown
                    if now.duration_since(last_exec) < exec_cooldown {
                        info!("Cooldown aktif, skip eksekusi");
                        continue;
                    }

                    // Build RouteSteps dari cycle edges
                    use types::RouteStep;
                    let steps: Vec<RouteStep> = cycle.edges.iter()
                        .map(|e| RouteStep {
                            pool_address: e.pool_address,
                            token_in:     e.token_in,
                            token_out:    e.token_out,
                            fee_tier:     e.fee_tier,
                            dex_type:     e.dex_type,
                            reserve_in:   e.reserve_in,
                            reserve_out:  e.reserve_out,
                        })
                        .collect();

                    if steps.len() < 2 || steps.len() > max_hops {
                        continue;
                    }

                    // Build ArbitrageRoute minimal
                    use types::ArbitrageRoute;
                    use uuid::Uuid;
                    use alloy::primitives::U256;

                    let start_token = steps[0].token_in;
                    let input_raw   = max_input_raw
                        * U256::from(10u64)
                        / U256::from(100u64); // 10% dari max input

                    let route = ArbitrageRoute {
                        id:              Uuid::new_v4(),
                        start_token,
                        steps,
                        optimal_input:   input_raw,
                        expected_output: input_raw, // akan divalidasi di contract
                        expected_profit: U256::ZERO,
                        profit_usd:      profit_est,
                        gas_cost_usd:    0.009,
                        net_profit_usd:  profit_est - 0.009,
                        calculated_at:   Utc::now(),
                        block_number:    block,
                    };

                    let params = ExecutionParams {
                        route,
                        min_profit_bps:   10, // 0.1% minimum profit
                        gas_price_gwei:   50.0,
                        deadline_seconds: 30,
                    };

                    let result = executor.execute(params).await;

                    // Log trade ke file
                    if let Err(e) = logger.log_trade(&result).await {
                        warn!("Gagal log trade: {e}");
                    }

                    last_exec = Instant::now();
                    break; // Hanya eksekusi satu trade per scan cycle
                }

                block += 1;
            }
        })
    };

    // ── TASK 3: Metrics Reporter ──────────────────────────────────────────────
    let metrics_task = {
        let metrics       = metrics.clone();
        let pool_registry = pool_registry.clone();
        let logger        = logger.clone();
        let tg            = tg.clone();

        tokio::spawn(async move {
            let mut ticker          = interval(Duration::from_secs(30));
            let mut tg_report_count = 0u64;

            loop {
                ticker.tick().await;

                let mut m = metrics.lock().await;
                m.pools_monitored = pool_registry.pool_count() as u64;
                m.update_derived();

                info!(
                    "Metrics | Pools:{} | Events:{} | Opp:{} | Trades:{} | PnL:${:.4}",
                    m.pools_monitored,
                    pool_registry.events_count(),
                    m.total_opportunities_found,
                    m.total_trades_executed,
                    m.net_profit_usd
                );

                let snap = m.clone();
                drop(m);

                // Kirim laporan ke Telegram setiap 10 menit (20 x 30s)
                tg_report_count += 1;
                if tg_report_count % 20 == 0 {
                    let uptime_m = start_time.elapsed().as_secs() / 60;
                    tg.send_md(&TelegramMsg::metrics_report(
                        snap.pools_monitored,
                        pool_registry.events_count(),
                        snap.total_opportunities_found,
                        snap.total_trades_executed,
                        snap.total_trades_successful,
                        snap.net_profit_usd,
                        uptime_m,
                    )).await;
                }

                if let Ok(stats) = logger.compute_stats().await {
                    if stats.total_trades > 0 {
                        info!(
                            "Trades: {} total | {} ok ({:.1}%) | ${:.4}",
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

    // ── Simulasi Info ─────────────────────────────────────────────────────────
    if cfg.simulation.is_simulation {
        info!(
            "Mode SIMULASI | MATIC:{:.0} | USDC:{:.0}",
            cfg.simulation.simulation_matic_balance,
            cfg.simulation.simulation_usdc_balance
        );
        info!("Jalankan Anvil: anvil --fork-url {} --chain-id 137",
            cfg.network.http_url
        );
    }

    // ── Wait ──────────────────────────────────────────────────────────────────
    tokio::select! {
        _ = monitor_task  => warn!("Monitor task selesai"),
        _ = detect_task   => warn!("Detector task selesai"),
        _ = metrics_task  => warn!("Metrics task selesai"),
        _ = tokio::signal::ctrl_c() => info!("Ctrl+C - shutdown"),
    }

    // Kirim notifikasi shutdown
    let final_metrics = metrics.lock().await;
    tg.send_md(&TelegramMsg::shutdown(
        final_metrics.net_profit_usd,
        final_metrics.total_trades_executed,
    )).await;

    info!("Bot berhenti.");
    Ok(())
}

fn print_banner(cfg: &BotConfig) {
    let mode = if cfg.simulation.is_simulation { "SIMULASI" } else { "LIVE" };
    println!("==============================================");
    println!("  MEV Multi-Hop Arbitrage Bot v0.1.0");
    println!("  Rust + Alloy | Polygon Mainnet");
    println!("----------------------------------------------");
    println!("  Mode:        {}", mode);
    println!("  Min Profit:  ${:.2}", cfg.strategy.min_profit_usd);
    println!("  Max Input:   ${:.2}", cfg.strategy.max_input_usd);
    println!("  Max Hops:    {}", cfg.strategy.max_hops);
    println!("==============================================");
}
