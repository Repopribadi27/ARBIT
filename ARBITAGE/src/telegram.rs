// src/telegram.rs
//! Notifikasi Telegram untuk setiap event penting bot.
//!
//! Menggunakan Telegram Bot API via HTTP (reqwest).
//! Setup:
//!   1. Buat bot baru via @BotFather -> dapatkan TELEGRAM_BOT_TOKEN
//!   2. Kirim /start ke bot kamu -> dapatkan TELEGRAM_CHAT_ID dari
//!      https://api.telegram.org/bot<TOKEN>/getUpdates
//!   3. Isi .env:
//!      TELEGRAM_BOT_TOKEN=1234567890:AAFxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
//!      TELEGRAM_CHAT_ID=123456789

use crate::types::{ExecutionStatus, TradeResult};
use anyhow::Result;
use reqwest::Client;
use serde_json::json;
use tracing::{debug, warn};

// ── TelegramConfig ────────────────────────────────────────────────────────────

/// Konfigurasi Telegram yang dibaca dari environment variables.
#[derive(Debug, Clone)]
pub struct TelegramConfig {
    pub token:   String,
    pub chat_id: String,
}

impl TelegramConfig {
    pub fn from_env() -> Self {
        let token   = std::env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default();
        let chat_id = std::env::var("TELEGRAM_CHAT_ID").unwrap_or_default();
        Self { token, chat_id }
    }
}

// ── TelegramMsg ───────────────────────────────────────────────────────────────

/// Builder pesan Telegram — semua method mengembalikan String yang siap dikirim.
pub struct TelegramMsg;

impl TelegramMsg {
    /// Bot baru saja dimulai.
    pub fn startup(mode: &str, min_profit: f64, max_input: f64) -> String {
        format!(
            "[BOT] <b>MEV Arb Bot STARTED</b>\nMode:       <code>{mode}</code>\nMin Profit: <b>${min_profit:.2}</b>\nMax Input:  <b>${max_input:.2}</b>",
        )
    }

    /// Trade berhasil dikonfirmasi on-chain.
    pub fn trade_success(
        tx_hash:    &str,
        profit_usd: f64,
        gas_cost:   f64,
        block:      u64,
        mode:       &str,
    ) -> String {
        let short_hash = if tx_hash.len() > 10 {
            format!("{}...", &tx_hash[..10])
        } else {
            tx_hash.to_string()
        };
        let net = profit_usd - gas_cost;
        format!(
            "[SUKSES] <b>Trade SUKSES {mode}</b>\nProfit:   <b>${profit_usd:.4}</b>\nGas Cost: <code>${gas_cost:.4}</code>\nNet PnL:  <b>${net:.4}</b>\nTx:       <code>{short_hash}</code>\nBlock:    <code>{block}</code>",
        )
    }

    /// Trade di-revert on-chain.
    pub fn trade_reverted(reason: &str, input_usd: f64, mode: &str) -> String {
        let short = if reason.len() > 80 { format!("{}...", &reason[..80]) } else { reason.to_string() };
        format!(
            "[REVERT] <b>Trade REVERT {mode}</b>\nInput:  <code>${input_usd:.4}</code>\nReason: <code>{short}</code>",
        )
    }

    /// Trade gagal sebelum dikirim ke chain.
    pub fn trade_failed(reason: &str) -> String {
        let short = if reason.len() > 80 { format!("{}...", &reason[..80]) } else { reason.to_string() };
        format!(
            "[GAGAL] <b>Trade FAILED</b>\nReason: <code>{short}</code>",
        )
    }

    /// Error kritis (disconnect, panic, dll).
    pub fn error(title: &str, detail: &str) -> String {
        let short = if detail.len() > 200 { format!("{}...", &detail[..200]) } else { detail.to_string() };
        format!(
            "[ERROR] <b>ERROR: {title}</b>\n<code>{short}</code>",
        )
    }

    /// Peluang arbitrase terdeteksi.
    pub fn opportunity_found(
        route:      &str,
        profit_est: f64,
        input_usd:  f64,
        hops:       usize,
        block:      u64,
    ) -> String {
        format!(
            "[PELUANG] <b>Peluang Terdeteksi!</b>\nRoute:      <code>{route}</code>\nInput Est:  <code>${input_usd:.4}</code>\nProfit Est: <b>${profit_est:.4}</b>\nHops:       <code>{hops}</code>\nBlock:      <code>{block}</code>",
        )
    }

    /// Laporan berkala metrik bot.
    pub fn metrics_report(
        pools:        u64,
        events:       u64,
        opp_found:    u64,
        trades_total: u64,
        trades_ok:    u64,
        net_pnl:      f64,
        uptime_min:   u64,
    ) -> String {
        let success_rate = if trades_total > 0 {
            trades_ok as f64 / trades_total as f64 * 100.0
        } else { 0.0 };
        let pnl_emoji = if net_pnl >= 0.0 { "[NAIK]" } else { "[TURUN]" };
        format!(
            "[STATS] <b>Laporan Berkala</b>\nUptime:  <code>{uptime_min} menit</code>\nPools:   <code>{pools}</code>\nEvents:  <code>{events}</code>\nPeluang: <code>{opp_found}</code>\nTrades:  <code>{trades_ok}/{trades_total} ({success_rate:.1}%)</code>\n{pnl_emoji} Net PnL: <b>${net_pnl:.4}</b>",
        )
    }

    /// Bot shutdown / disconnect.
    pub fn shutdown(net_profit_usd: f64, total_trades: u64) -> String {
        format!(
            "[STOP] <b>Bot STOPPED</b>\nTotal Trades: <code>{total_trades}</code>\nNet PnL:      <b>${net_profit_usd:.4}</b>",
        )
    }
}

// ── TelegramNotifier ──────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct TelegramNotifier {
    client:  Client,
    token:   String,
    chat_id: String,
    enabled: bool,
}

impl TelegramNotifier {
    /// Buat notifier dari `TelegramConfig`.
    /// Jika token/chat_id kosong, notifikasi dinonaktifkan.
    pub fn new(cfg: TelegramConfig) -> Self {
        let enabled = !cfg.token.is_empty()
            && !cfg.chat_id.is_empty()
            && cfg.token != "disabled";
        if !enabled {
            warn!("Telegram notifier NONAKTIF (token/chat_id kosong di .env)");
        }
        Self {
            client:  Client::new(),
            token:   cfg.token,
            chat_id: cfg.chat_id,
            enabled,
        }
    }

    pub fn is_enabled(&self) -> bool { self.enabled }

    /// Kirim pesan teks ke Telegram (parse_mode: HTML).
    pub async fn send(&self, text: &str) -> Result<()> {
        if !self.enabled { return Ok(()); }

        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            self.token
        );

        let body = json!({
            "chat_id":    self.chat_id,
            "text":       text,
            "parse_mode": "HTML"
        });

        match self.client.post(&url).json(&body).send().await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    warn!("Telegram API error: {}", resp.status());
                } else {
                    debug!("Telegram notif terkirim");
                }
            }
            Err(e) => warn!("Gagal kirim Telegram: {e}"),
        }

        Ok(())
    }

    /// Alias dari `send` — kompatibel dengan pemanggil `send_md`.
    pub async fn send_md(&self, text: &str) -> Result<()> {
        self.send(text).await
    }

    // ── Notifikasi Spesifik ───────────────────────────────────────────────────

    pub async fn notify_startup(&self, mode: &str, min_profit: f64, max_input: f64) {
        let _ = self.send(&TelegramMsg::startup(mode, min_profit, max_input)).await;
    }

    pub async fn notify_opportunity(
        &self, route: &str, profit_est_usd: f64, input_usd: f64, hop_count: usize, block: u64,
    ) {
        let _ = self.send(
            &TelegramMsg::opportunity_found(route, profit_est_usd, input_usd, hop_count, block)
        ).await;
    }

    pub async fn notify_trade_result(&self, result: &TradeResult) {
        let mode = if result.simulation_mode { "[SIM]" } else { "[LIVE]" };
        let msg = match &result.status {
            ExecutionStatus::Confirmed { tx_hash, block, actual_profit_usd } => {
                let gas_cost = result.gas_used.unwrap_or(0) as f64
                    * result.gas_price_gwei.unwrap_or(50.0)
                    * 1e-9 * 0.80;
                TelegramMsg::trade_success(tx_hash, *actual_profit_usd, gas_cost, *block, mode)
            }
            ExecutionStatus::Reverted { reason, .. } => {
                let input_usd = result.route.optimal_input.to::<u128>() as f64 / 1e6;
                TelegramMsg::trade_reverted(reason, input_usd, mode)
            }
            ExecutionStatus::Failed { reason } => TelegramMsg::trade_failed(reason),
            _ => return,
        };
        let _ = self.send(&msg).await;
    }

    pub async fn notify_periodic_stats(
        &self, pools: u64, events: u64, opp_found: u64,
        trades_ok: u64, trades_total: u64, net_pnl: f64, uptime_min: u64,
    ) {
        let _ = self.send(
            &TelegramMsg::metrics_report(pools, events, opp_found, trades_total, trades_ok, net_pnl, uptime_min)
        ).await;
    }

    pub async fn notify_error(&self, title: &str, detail: &str) {
        let _ = self.send(&TelegramMsg::error(title, detail)).await;
    }

    pub async fn notify_shutdown(&self, net_profit_usd: f64, total_trades: u64) {
        let _ = self.send(&TelegramMsg::shutdown(net_profit_usd, total_trades)).await;
    }
}
