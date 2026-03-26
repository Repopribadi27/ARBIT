// src/config.rs
use alloy::primitives::Address;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotConfig {
    pub network:    NetworkConfig,
    pub strategy:   StrategyConfig,
    pub tokens:     TokenConfig,
    pub dex:        DexConfig,
    pub simulation: SimulationConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub ws_url:        String,
    pub http_url:      String,
    pub chain_id:      u64,
    pub private_key:   String,
    pub bot_address:   Address,
    pub contract_addr: Address,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyConfig {
    pub min_profit_usd:   f64,
    pub max_input_usd:    f64,
    pub max_hops:         usize,
    pub slippage_bps:     u64,
    pub gas_multiplier:   f64,
    pub scan_interval_ms: u64,
}

// ── TokenConfig ───────────────────────────────────────────────────────────────
// REMOVED: `dai`  — wide spreads + 18-decimal dust errors at $10 capital
// REMOVED: `wbtc` — illiquid at micro-scale, high slippage on small DEXes
// ADDED:   `link` — deep Polygon liquidity; tight spread on QS V2 + Uniswap V3
// ADDED:   `maticx` — Stader liquid staking; active arb vs WMATIC on QS + Dfyn
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenConfig {
    pub wmatic:  Address,
    pub weth:    Address,
    pub usdc:    Address,
    pub usdt:    Address,
    pub link:    Address,   // ← NEW (replaces wbtc)
    pub maticx:  Address,   // ← NEW (replaces dai)
}

impl TokenConfig {
    /// Returns only the six high-liquidity tokens used for graph construction.
    /// Order is intentional: stable pairs first so Bellman-Ford seeds quickly.
    pub fn base_tokens(&self) -> Vec<Address> {
        vec![
            self.wmatic,   // native gas token — present in every pair
            self.usdc,     // deepest stable pool on Polygon
            self.usdt,     // second-deepest stable; tight USDC↔USDT arb
            self.weth,     // ETH bridge token — high-volume V2 + V3
            self.link,     // oracle token; cross-DEX price divergence common
            self.maticx,   // liquid staking premium creates WMATIC↔MATICx arb
        ]
    }
}

// ── DexConfig ─────────────────────────────────────────────────────────────────
// NEW fields added below the original three:
//   quickswap_v3_factory — Algebra concentrated liquidity (Polygon-native)
//   apeswap_factory      — V2-compatible; good WMATIC/USDT TVL
//   dfyn_factory         — V2-compatible; active MATICx pairs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DexConfig {
    pub quickswap_v2_factory:  Address,
    pub sushiswap_factory:     Address,
    pub uniswap_v3_factory:    Address,  // existed before; now wired into factories vec
    pub quickswap_v3_factory:  Address,  // ← NEW
    pub apeswap_factory:       Address,  // ← NEW
    pub dfyn_factory:          Address,  // ← NEW
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationConfig {
    pub anvil_rpc_url:            String,
    pub anvil_ws_url:             String,
    pub simulation_matic_balance: f64,
    pub simulation_usdc_balance:  f64,
    pub is_simulation:            bool,
}

impl BotConfig {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();

        let cfg = BotConfig {
            network: NetworkConfig {
                ws_url:       env_str("ALCHEMY_WS_URL")?,
                http_url:     env_str("ALCHEMY_HTTP_URL")?,
                chain_id:     137,
                private_key:  env_str("PRIVATE_KEY")?,
                bot_address:  parse_addr("BOT_ADDRESS")
                    .unwrap_or(Address::ZERO),
                contract_addr: parse_addr("ARBITRAGE_CONTRACT")
                    .unwrap_or(Address::ZERO),
            },
            strategy: StrategyConfig {
                // Micro-capital defaults mirror .env values; override via env
                min_profit_usd:   env_f64("MIN_PROFIT_USD").unwrap_or(0.03),
                max_input_usd:    env_f64("MAX_INPUT_USD").unwrap_or(10.0),
                max_hops:         env_usize("MAX_HOPS").unwrap_or(3),
                slippage_bps:     env_u64("SLIPPAGE_BPS").unwrap_or(10),
                gas_multiplier:   env_f64("GAS_MULTIPLIER").unwrap_or(1.15),
                scan_interval_ms: 100,
            },
            tokens: TokenConfig {
                wmatic: parse_addr("WMATIC")
                    .unwrap_or_else(|_| "0x0d500B1d8E8eF31E21C99d1Db9A6444d3ADf1270".parse().unwrap()),
                weth:   parse_addr("WETH")
                    .unwrap_or_else(|_| "0x7ceB23fD6bC0adD59E62ac25578270cFf1b9f619".parse().unwrap()),
                usdc:   parse_addr("USDC")
                    .unwrap_or_else(|_| "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174".parse().unwrap()),
                usdt:   parse_addr("USDT")
                    .unwrap_or_else(|_| "0xc2132D05D31c914a87C6611C10748AEb04B58e8F".parse().unwrap()),
                // LINK — Chainlink on Polygon PoS (18 decimals)
                link:   parse_addr("LINK")
                    .unwrap_or_else(|_| "0x53E0bca35eC356BD5ddDFebbD1Fc0fD03FaBad39".parse().unwrap()),
                // MATICx — Stader liquid staking token (18 decimals)
                maticx: parse_addr("MATICX")
                    .unwrap_or_else(|_| "0xfa68FB4628DFF1028CFEc22b4162FCcd0d45efb6".parse().unwrap()),
            },
            dex: DexConfig {
                quickswap_v2_factory: parse_addr("QUICKSWAP_V2_FACTORY")
                    .unwrap_or_else(|_| "0x5757371414417b8C6CAad45bAeF941aBc7d3Ab32".parse().unwrap()),
                sushiswap_factory: parse_addr("SUSHISWAP_FACTORY")
                    .unwrap_or_else(|_| "0xc35DADB65012eC5796536bD9864eD8773aBc74C4".parse().unwrap()),
                uniswap_v3_factory: parse_addr("UNISWAP_V3_FACTORY")
                    .unwrap_or_else(|_| "0x1F98431c8aD98523631AE4a59f267346ea31F984".parse().unwrap()),
                // QuickSwap V3 (Algebra protocol) — Polygon canonical address
                quickswap_v3_factory: parse_addr("QUICKSWAP_V3_FACTORY")
                    .unwrap_or_else(|_| "0x411b0fAcC3489691f28ad58c47006AF5E3Ab3A28".parse().unwrap()),
                // ApeSwap — V2-compatible Polygon factory
                apeswap_factory: parse_addr("APESWAP_FACTORY")
                    .unwrap_or_else(|_| "0xCf083Be4164828f00cAE704EC15a36D711491284".parse().unwrap()),
                // Dfyn — V2-compatible; good MATICx/WMATIC pool
                dfyn_factory: parse_addr("DFYN_FACTORY")
                    .unwrap_or_else(|_| "0xE7Fb3e833eFE5F9c441105EB65Ef8b261266423B".parse().unwrap()),
            },
            simulation: SimulationConfig {
                anvil_rpc_url: env_str("ANVIL_RPC_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:8545".to_string()),
                anvil_ws_url: env_str("ANVIL_WS_URL")
                    .unwrap_or_else(|_| "ws://127.0.0.1:8545".to_string()),
                simulation_matic_balance: env_f64("SIMULATION_MATIC_BALANCE").unwrap_or(10_000.0),
                simulation_usdc_balance:  env_f64("SIMULATION_USDC_BALANCE").unwrap_or(50_000.0),
                is_simulation: std::env::var("SIMULATION_MODE")
                    .map(|v| v == "true" || v == "1")
                    .unwrap_or(false),
            },
        };

        Ok(cfg)
    }
}

fn env_str(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("Env var '{key}' tidak ditemukan"))
}

fn parse_addr(key: &str) -> Result<Address> {
    let val = env_str(key)?;
    Address::from_str(&val)
        .with_context(|| format!("Gagal parse address dari '{key}'"))
}

fn env_f64(key: &str) -> Result<f64> {
    env_str(key)?.parse::<f64>()
        .with_context(|| format!("Gagal parse f64 dari '{key}'"))
}

fn env_u64(key: &str) -> Result<u64> {
    env_str(key)?.parse::<u64>()
        .with_context(|| format!("Gagal parse u64 dari '{key}'"))
}

fn env_usize(key: &str) -> Result<usize> {
    env_str(key)?.parse::<usize>()
        .with_context(|| format!("Gagal parse usize dari '{key}'"))
}