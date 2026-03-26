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

use crate::types::{ArbitrageCycle, ExecutionStatus, TradeResult};
use anyhow::Result;
use reqwest::Client;
use serde_json::json;
use tracing::{debug, warn};

#[derive(Clone)]
pub struct TelegramNotifier {
    client:   Client,
    token:    String,
    chat_id:  String,
    enabled:  bool,
}

impl TelegramNotifier {
    /// Buat notifier baru. Jika token/chat_id kosong, notifikasi dinonaktifkan.
    pub fn new(token: String, chat_id: String) -> Self {
        let enabled = !token.is_empty() && !chat_id.is_empty()
            && token != "disabled";
        if !enabled {
            warn!("Telegram notifier NONAKTIF (token/chat_id kosong di .env)");
        }
        Self {
            client: Client::new(),
            token,
            chat_id,
            enabled,
        }
    }

    pub fn from_env() -> Self {
        let token   = std::env::var("8631512709:AAFcnXoNcT5yu20AHRrlKIZV9pGnWnAKT78").unwrap_or_default();
        let chat_id = std::env::var("1570042022").unwrap_or_default();
        Self::new(token, chat_id)
    }

    pub fn is_enabled(&self) -> bool { self.enabled }

    /// Kirim pesan teks biasa ke Telegram
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

    // ── Notifikasi Spesifik ───────────────────────────────────────────────────

    /// Bot baru saja dimulai
    pub async fn notify_startup(
        &self,
        mode:       &str,
        min_profit: f64,
        max_input:  f64,
    ) {
        let msg = format!(
            "[BOT] <b>MEV Arb Bot STARTED</b>\n\
             Mode: <code>{mode}</code>\n\
             Min Profit: <b>${min_profit:.2}</b>\n\
             Max Input:  <b>${max_input:.2}</b>",
        );
        let _ = self.send(&msg).await;
    }

    /// Peluang arbitrase terdeteksi
    pub async fn notify_opportunity(
        &self,
        profit_est_usd: f64,
        log_weight:     f64,
        hop_count:      usize,
        block:          u64,
    ) {
        let profit_pct = (-log_weight).exp() - 1.0;
        let msg = format!(
            "[PELUANG] <b>Peluang Terdeteksi!</b>\n\
             Profit Est: <b>${profit_est_usd:.4}</b>\n\
             Profit %:   <code>{:.3}%</code>\n\
             Hops:       <code>{hop_count}</code>\n\
             Block:      <code>{block}</code>",
            profit_pct * 100.0,
        );
        let _ = self.send(&msg).await;
    }

    /// Trade berhasil dieksekusi
    pub async fn notify_trade_result(&self, result: &TradeResult) {
        let mode = if result.simulation_mode { "[SIM]" } else { "[LIVE]" };

        let msg = match &result.status {
            ExecutionStatus::Confirmed { tx_hash, block, actual_profit_usd } => {
                let gas = result.gas_used.unwrap_or(0);
                let short_hash = if tx_hash.len() > 10 {
                    format!("{}...", &tx_hash[..10])
                } else {
                    tx_hash.clone()
                };
                format!(
                    "[SUKSES] <b>Trade SUKSES {mode}</b>\n\
                     Profit:  <b>${actual_profit_usd:.4}</b>\n\
                     Tx Hash: <code>{short_hash}</code>\n\
                     Block:   <code>{block}</code>\n\
                     Gas:     <code>{gas}</code>",
                )
            }
            ExecutionStatus::Reverted { tx_hash, reason } => {
                let short_hash = if tx_hash.len() > 10 {
                    format!("{}...", &tx_hash[..10])
                } else {
                    tx_hash.clone()
                };
                let short_reason = if reason.len() > 80 {
                    format!("{}...", &reason[..80])
                } else {
                    reason.clone()
                };
                format!(
                    "[REVERT] <b>Trade REVERT {mode}</b>\n\
                     Tx:     <code>{short_hash}</code>\n\
                     Reason: <code>{short_reason}</code>",
                )
            }
            ExecutionStatus::Failed { reason } => {
                let short = if reason.len() > 80 {
                    format!("{}...", &reason[..80])
                } else {
                    reason.clone()
                };
                format!(
                    "[GAGAL] <b>Trade FAILED {mode}</b>\n\
                     Reason: <code>{short}</code>",
                )
            }
            _ => return,
        };

        let _ = self.send(&msg).await;
    }

    /// Laporan berkala (setiap 30 menit)
    pub async fn notify_periodic_stats(
        &self,
        pools:        u64,
        events:       u64,
        opp_found:    u64,
        trades_ok:    u64,
        trades_total: u64,
        net_pnl:      f64,
        uptime_min:   u64,
    ) {
        let success_rate = if trades_total > 0 {
            trades_ok as f64 / trades_total as f64 * 100.0
        } else { 0.0 };

        let pnl_emoji = if net_pnl >= 0.0 { "[NAIK]" } else { "[TURUN]" };

        let msg = format!(
            "[STATS] <b>Laporan Berkala</b>\n\
             Uptime:    <code>{uptime_min} menit</code>\n\
             Pools:     <code>{pools}</code>\n\
             Events:    <code>{events}</code>\n\
             Peluang:   <code>{opp_found}</code>\n\
             Trades:    <code>{trades_ok}/{trades_total} ({success_rate:.1}%)</code>\n\
             {pnl_emoji} Net PnL: <b>${net_pnl:.4}</b>",
        );
        let _ = self.send(&msg).await;
    }

    /// Error kritis (disconnect, panic, dll)
    pub async fn notify_error(&self, err: &str) {
        let short = if err.len() > 200 {
            format!("{}...", &err[..200])
        } else {
            err.to_string()
        };
        let msg = format!("[ERROR] <b>ERROR KRITIS</b>\n<code>{short}</code>");
        let _ = self.send(&msg).await;
    }

    /// Bot shutdown / disconnect
    pub async fn notify_shutdown(&self, reason: &str) {
        let msg = format!("[STOP] <b>Bot STOPPED</b>\nAlasan: <code>{reason}</code>");
        let _ = self.send(&msg).await;
    }
}