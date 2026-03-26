// src/monitor.rs
//! Monitor WebSocket real-time untuk event Sync DEX V2.

use crate::graph::ArbitrageGraph;
use crate::types::{DexType, FeeTier, PoolStateV2, SyncEvent};
use alloy::{
    primitives::{Address, B256, U256, b256},
    providers::{Provider, ProviderBuilder, WsConnect},
    rpc::types::Filter,
    sol,
};
use anyhow::Result;
use chrono::Utc;
use dashmap::DashMap;
use futures_util::StreamExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

// Sync event topic: keccak256("Sync(uint112,uint112)")
const SYNC_TOPIC: B256 =
    b256!("1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1");

sol! {
    #[sol(rpc)]
    interface IUniswapV2Pair {
        function token0() external view returns (address);
        function token1() external view returns (address);
        function getReserves() external view returns (
            uint112 reserve0,
            uint112 reserve1,
            uint32  blockTimestampLast
        );
    }

    #[sol(rpc)]
    interface IUniswapV2Factory {
        function getPair(address tokenA, address tokenB)
            external view returns (address pair);
        function allPairsLength() external view returns (uint256);
    }
}

// ── Pool Registry ─────────────────────────────────────────────────────────────

pub struct PoolRegistry {
    pub pools:            DashMap<Address, PoolStateV2>,
    pub by_pair:          DashMap<(Address, Address), Vec<Address>>,
    pub events_processed: AtomicU64,
}

impl PoolRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            pools:            DashMap::new(),
            by_pair:          DashMap::new(),
            events_processed: AtomicU64::new(0),
        })
    }

    pub fn register_pool(&self, pool: PoolStateV2) {
        let addr = pool.address;
        let t0   = pool.token0;
        let t1   = pool.token1;
        let key  = if t0 < t1 { (t0, t1) } else { (t1, t0) };
        self.by_pair.entry(key).or_insert_with(Vec::new).push(addr);
        self.pools.insert(addr, pool);
        debug!("Pool terdaftar: {addr}");
    }

    pub fn update_reserves(
        &self,
        pool_address: Address,
        reserve0:     U256,
        reserve1:     U256,
        block_number: u64,
    ) -> bool {
        if let Some(mut pool) = self.pools.get_mut(&pool_address) {
            // FIX: Skip update jika reserve tidak berubah sama sekali
            // (menghindari graph update yang tidak perlu)
            if pool.reserve0 == reserve0 && pool.reserve1 == reserve1 {
                return false;
            }
            pool.reserve0     = reserve0;
            pool.reserve1     = reserve1;
            pool.block_number = block_number;
            pool.last_updated = Utc::now();
            self.events_processed.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        false
    }

    pub fn pool_count(&self) -> usize {
        self.pools.len()
    }

    pub fn events_count(&self) -> u64 {
        self.events_processed.load(Ordering::Relaxed)
    }
}

// ── DexMonitor ────────────────────────────────────────────────────────────────

pub struct DexMonitor {
    pub ws_url:        String,
    pub http_url:      String,
    pub pool_registry: Arc<PoolRegistry>,
    pub graph:         Arc<ArbitrageGraph>,
    pub event_tx:      mpsc::Sender<SyncEvent>,
    pub factories:     Vec<(Address, DexType, FeeTier)>,
    pub base_tokens:   Vec<Address>,
}

impl DexMonitor {
    pub fn new(
        ws_url:        String,
        http_url:      String,
        pool_registry: Arc<PoolRegistry>,
        graph:         Arc<ArbitrageGraph>,
        event_tx:      mpsc::Sender<SyncEvent>,
        factories:     Vec<(Address, DexType, FeeTier)>,
        base_tokens:   Vec<Address>,
    ) -> Self {
        Self { ws_url, http_url, pool_registry, graph, event_tx, factories, base_tokens }
    }

    pub async fn run(&self) -> Result<()> {
        info!("Menghubungkan ke WebSocket: {}", self.ws_url);

        let ws       = WsConnect::new(&self.ws_url);
        let provider = ProviderBuilder::new().on_ws(ws).await?;
        let provider = Arc::new(provider);

        info!("WebSocket terhubung");

        let http_provider = ProviderBuilder::new()
            .on_builtin(&self.http_url)
            .await?;
        let http_provider = Arc::new(http_provider);

        info!("Menemukan pools dari {} factory...", self.factories.len());
        self.discover_pools(http_provider).await?;

        let pool_count = self.pool_registry.pool_count();
        info!("Total {} pools ditemukan", pool_count);

        // FIX: Jangan lanjut subscribe jika tidak ada pool yang ditemukan
        if pool_count == 0 {
            return Err(anyhow::anyhow!(
                "Tidak ada pool yang ditemukan! Periksa factory address dan koneksi RPC."
            ));
        }

        info!("Subscribe ke Sync events...");
        self.subscribe_sync_events(provider).await?;

        Ok(())
    }

    async fn discover_pools(
        &self,
        provider: Arc<impl Provider + Clone + 'static>,
    ) -> Result<()> {
        let token_pairs = self.generate_token_pairs();
        info!("Mengecek {} pasangan di {} DEX...",
            token_pairs.len(), self.factories.len());

        for (factory_addr, dex_type, fee_tier) in &self.factories {
            let factory = IUniswapV2Factory::new(*factory_addr, provider.clone());

            for (token_a, token_b) in &token_pairs {
                match factory.getPair(*token_a, *token_b).call().await {
                    Ok(ret) => {
                        let pair_addr = ret.pair;
                        if pair_addr == Address::ZERO {
                            continue;
                        }
                        match self.fetch_pool_state(&provider, pair_addr, *dex_type, *fee_tier).await {
                            Ok(pool) => {
                                // FIX: Hanya register pool jika punya liquiditas cukup
                                if pool.reserve0 > U256::ZERO && pool.reserve1 > U256::ZERO {
                                    self.graph.update_pool(&pool);
                                    self.pool_registry.register_pool(pool);
                                } else {
                                    debug!("Pool {pair_addr} dilewati: reserve kosong");
                                }
                            }
                            Err(e) => debug!("fetch_pool_state error {pair_addr}: {e}"),
                        }
                    }
                    Err(e) => debug!("getPair error: {e}"),
                }
            }
        }

        Ok(())
    }

    async fn fetch_pool_state(
        &self,
        provider:  &Arc<impl Provider + Clone + 'static>,
        pool_addr: Address,
        dex_type:  DexType,
        fee_tier:  FeeTier,
    ) -> Result<PoolStateV2> {
        let pair = IUniswapV2Pair::new(pool_addr, provider.clone());

        let token0_ret   = pair.token0().call().await?;
        let token1_ret   = pair.token1().call().await?;
        let reserves_ret = pair.getReserves().call().await?;

        let block = provider.get_block_number().await.unwrap_or(0);

        Ok(PoolStateV2 {
            address:      pool_addr,
            token0:       token0_ret._0,
            token1:       token1_ret._0,
            reserve0:     U256::from(reserves_ret.reserve0),
            reserve1:     U256::from(reserves_ret.reserve1),
            fee_tier,
            dex:          dex_type,
            last_updated: Utc::now(),
            block_number: block,
        })
    }

    async fn subscribe_sync_events(
        &self,
        provider: Arc<alloy::providers::RootProvider<alloy::pubsub::PubSubFrontend>>,
    ) -> Result<()> {
        let pool_addresses: Vec<Address> = self.pool_registry
            .pools
            .iter()
            .map(|e| *e.key())
            .collect();

        if pool_addresses.is_empty() {
            warn!("Tidak ada pool yang bisa dimonitor!");
            return Ok(());
        }

        info!("Subscribe Sync events untuk {} pools", pool_addresses.len());

        let filter = Filter::new()
            .event_signature(SYNC_TOPIC)
            .address(pool_addresses);

        let sub        = provider.subscribe_logs(&filter).await?;
        let mut stream = sub.into_stream();

        while let Some(log) = stream.next().await {
            let pool_address = log.address();
            let block_number = log.block_number.unwrap_or(0);
            let tx_hash      = log.transaction_hash.unwrap_or_default();
            let data         = log.data();

            // FIX: Sync event Uniswap V2 = 2 × uint112 = 2 × 32 bytes ABI-encoded
            if data.data.len() < 64 {
                debug!("Data log terlalu pendek ({} bytes) dari {pool_address}", data.data.len());
                continue;
            }

            let reserve0 = U256::from_be_slice(&data.data[0..32]);
            let reserve1 = U256::from_be_slice(&data.data[32..64]);

            // FIX: Skip jika salah satu reserve = 0 (pool dikeringkan / tidak valid)
            if reserve0.is_zero() || reserve1.is_zero() {
                debug!("Reserve nol dari {pool_address}, skip");
                continue;
            }

            let updated = self.pool_registry
                .update_reserves(pool_address, reserve0, reserve1, block_number);

            if updated {
                if let Some(pool) = self.pool_registry.pools.get(&pool_address) {
                    self.graph.update_pool(&pool);
                }

                let event = SyncEvent {
                    pool_address,
                    reserve0,
                    reserve1,
                    block_number,
                    tx_hash,
                    received_at: Utc::now(),
                };

                if self.event_tx.try_send(event).is_err() {
                    debug!("Event channel penuh, skip {pool_address}");
                }
            }
        }

        warn!("WebSocket stream berakhir");
        Ok(())
    }

    fn generate_token_pairs(&self) -> Vec<(Address, Address)> {
        let mut pairs = Vec::new();
        let t = &self.base_tokens;
        for i in 0..t.len() {
            for j in (i + 1)..t.len() {
                pairs.push((t[i], t[j]));
            }
        }
        pairs
    }
}
