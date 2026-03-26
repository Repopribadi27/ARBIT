// src/graph/mod.rs
use crate::types::{ArbitrageRoute, DexType, FeeTier, PoolStateV2, RouteStep};
use crate::math::{optimal_input_two_hop, optimal_input_three_hop, simulate_multihop, u256_to_f64};
use alloy::primitives::{Address, U256};
use chrono::Utc;
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::trace;
use uuid::Uuid;

type NodeId = Address;

// ── PoolEdge ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PoolEdge {
    pub pool_address: Address,
    pub token_in:     Address,
    pub token_out:    Address,
    pub reserve_in:   U256,
    pub reserve_out:  U256,
    pub fee_tier:     FeeTier,
    pub dex_type:     DexType,
    pub log_weight:   f64,
}

impl PoolEdge {
    pub fn new(pool: &PoolStateV2, token_in: Address, token_out: Address) -> Option<Self> {
        let (reserve_in, reserve_out) = if token_in == pool.token0 {
            (pool.reserve0, pool.reserve1)
        } else if token_in == pool.token1 {
            (pool.reserve1, pool.reserve0)
        } else {
            return None;
        };

        if reserve_in == U256::ZERO || reserve_out == U256::ZERO {
            return None;
        }

        let rate = (u256_to_f64(reserve_out) * pool.fee_tier.multiplier())
            / u256_to_f64(reserve_in);

        if rate <= 0.0 { return None; }

        Some(PoolEdge {
            pool_address: pool.address,
            token_in,
            token_out,
            reserve_in,
            reserve_out,
            fee_tier:   pool.fee_tier,
            dex_type:   pool.dex,
            log_weight: -rate.ln(),
        })
    }
}

// ── ArbitrageGraph ────────────────────────────────────────────────────────────

pub struct ArbitrageGraph {
    pub edges:        DashMap<(NodeId, NodeId), Vec<PoolEdge>>,
    pub nodes:        DashMap<NodeId, usize>,
    pub base_tokens:  Vec<Address>,
    pub update_count: std::sync::atomic::AtomicU64,
}

impl ArbitrageGraph {
    pub fn new(base_tokens: Vec<Address>) -> Arc<Self> {
        Arc::new(Self {
            edges:        DashMap::new(),
            nodes:        DashMap::new(),
            base_tokens,
            update_count: std::sync::atomic::AtomicU64::new(0),
        })
    }

    pub fn update_pool(&self, pool: &PoolStateV2) {
        let nc = self.nodes.len();
        self.nodes.entry(pool.token0).or_insert(nc);
        let nc = self.nodes.len();
        self.nodes.entry(pool.token1).or_insert(nc);

        for (ti, to) in [(pool.token0, pool.token1), (pool.token1, pool.token0)] {
            if let Some(edge) = PoolEdge::new(pool, ti, to) {
                let key = (ti, to);
                let mut entry = self.edges.entry(key).or_insert_with(Vec::new);
                if let Some(pos) = entry.iter().position(|e| e.pool_address == pool.address) {
                    entry[pos] = edge;
                } else {
                    entry.push(edge);
                }
            }
        }

        self.update_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn all_edges_flat(&self) -> Vec<PoolEdge> {
        let mut result = Vec::new();
        for entry in self.edges.iter() {
            if let Some(best) = entry.value().iter()
                .min_by(|a, b| a.log_weight.partial_cmp(&b.log_weight).unwrap())
            {
                result.push(best.clone());
            }
        }
        result
    }

    pub fn all_nodes(&self) -> Vec<NodeId> {
        self.nodes.iter().map(|e| *e.key()).collect()
    }
}

// ── ArbitrageCycle ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ArbitrageCycle {
    pub edges:            Vec<PoolEdge>,
    pub total_log_weight: f64,
    pub source_token:     Address,
    pub block_number:     u64,
}

// ── Bellman-Ford Detector ─────────────────────────────────────────────────────

pub struct BellmanFordDetector {
    graph:    Arc<ArbitrageGraph>,
    max_hops: usize,
    min_threshold: f64,
}

impl BellmanFordDetector {
    pub fn new(
        graph:         Arc<ArbitrageGraph>,
        max_hops:      usize,
        min_threshold: f64,
    ) -> Self {
        Self { graph, max_hops, min_threshold }
    }

    pub fn find_arbitrage_cycles(
        &self,
        block_number: u64,
        max_results:  usize,
    ) -> Vec<ArbitrageCycle> {
        let edges = self.graph.all_edges_flat();
        let nodes = self.graph.all_nodes();

        if nodes.len() < 2 || edges.is_empty() {
            return vec![];
        }

        let mut all_cycles = Vec::new();

        for &source in &self.graph.base_tokens {
            if !self.graph.nodes.contains_key(&source) {
                continue;
            }
            let cycles = self.bellman_ford(source, &edges, &nodes, block_number);
            all_cycles.extend(cycles);
            if all_cycles.len() >= max_results * 2 { break; }
        }

        all_cycles.sort_by(|a, b| {
            a.total_log_weight.partial_cmp(&b.total_log_weight).unwrap()
        });
        all_cycles.into_iter().take(max_results).collect()
    }

    fn bellman_ford(
        &self,
        source:       NodeId,
        edges:        &[PoolEdge],
        nodes:        &[NodeId],
        block_number: u64,
    ) -> Vec<ArbitrageCycle> {
        let n = nodes.len();
        let node_idx: HashMap<NodeId, usize> = nodes.iter()
            .enumerate().map(|(i, &a)| (a, i)).collect();

        let src_idx = match node_idx.get(&source) {
            Some(&i) => i,
            None     => return vec![],
        };

        let mut dist: Vec<f64>             = vec![f64::INFINITY; n];
        let mut pred: Vec<Option<(usize, usize)>> = vec![None; n]; // (prev_node_idx, edge_idx)
        dist[src_idx] = 0.0;

        let max_iters = self.max_hops.min(n.saturating_sub(1));

        for _ in 0..max_iters {
            let mut updated = false;
            for (ei, edge) in edges.iter().enumerate() {
                let u = match node_idx.get(&edge.token_in)  { Some(&i) => i, None => continue };
                let v = match node_idx.get(&edge.token_out) { Some(&i) => i, None => continue };
                if dist[u] == f64::INFINITY { continue; }
                let nd = dist[u] + edge.log_weight;
                if nd < dist[v] - 1e-12 {
                    dist[v] = nd;
                    pred[v] = Some((u, ei));
                    updated = true;
                }
            }
            if !updated { break; }
        }

        // Deteksi negative cycle yang kembali ke source
        let mut cycles = Vec::new();
        for (ei, edge) in edges.iter().enumerate() {
            let u = match node_idx.get(&edge.token_in)  { Some(&i) => i, None => continue };
            let v = match node_idx.get(&edge.token_out) { Some(&i) => i, None => continue };
            if v != src_idx || dist[u] == f64::INFINITY { continue; }

            let cycle_w = dist[u] + edge.log_weight;
            if cycle_w < -self.min_threshold {
                if let Some(path_edges) = self.reconstruct_path(
                    src_idx, u, ei, edges, &pred, n
                ) {
                    cycles.push(ArbitrageCycle {
                        edges: path_edges,
                        total_log_weight: cycle_w,
                        source_token: source,
                        block_number,
                    });
                }
            }
        }
        cycles
    }

    fn reconstruct_path(
        &self,
        src_idx:   usize,
        u_idx:     usize,
        final_ei:  usize,
        edges:     &[PoolEdge],
        pred:      &[Option<(usize, usize)>],
        n:         usize,
    ) -> Option<Vec<PoolEdge>> {
        let mut path = vec![edges[final_ei].clone()];
        let mut cur  = u_idx;
        let mut seen = vec![false; n];
        seen[cur] = true;

        loop {
            if cur == src_idx { break; }
            match pred[cur] {
                Some((prev, ei)) => {
                    if seen[prev] && prev != src_idx { return None; }
                    seen[prev] = true;
                    path.push(edges[ei].clone());
                    cur = prev;
                }
                None => return None,
            }
            if path.len() > self.max_hops { return None; }
        }
        path.reverse();
        Some(path)
    }
}

// ── RouteEvaluator ────────────────────────────────────────────────────────────

pub struct RouteEvaluator {
    pub max_input_raw: U256,
}

impl RouteEvaluator {
    pub fn new(max_input_raw: U256) -> Self { Self { max_input_raw } }

    pub fn evaluate_route(
        &self,
        steps:           Vec<RouteStep>,
        start_token:     Address,
        block_number:    u64,
        token_price_usd: f64,
        token_decimals:  &HashMap<Address, u8>,
    ) -> Option<ArbitrageRoute> {
        if steps.is_empty() || steps.len() > 3 { return None; }

        let reserves: Vec<(U256, U256)> = steps.iter()
            .map(|s| (s.reserve_in, s.reserve_out))
            .collect();

        let optimal_input = if steps.len() == 2 {
            optimal_input_two_hop(
                steps[0].reserve_in,  steps[0].reserve_out,
                steps[1].reserve_in,  steps[1].reserve_out,
            )?
        } else {
            optimal_input_three_hop(&reserves, self.max_input_raw)?
        };

        let clamped = optimal_input.min(self.max_input_raw);
        if clamped.is_zero() { return None; }

        let expected_output = simulate_multihop(clamped, &reserves).ok()?;
        if expected_output <= clamped { return None; }

        let expected_profit = expected_output - clamped;
        let dec = *token_decimals.get(&start_token).unwrap_or(&18) as i32;
        let profit_human = u256_to_f64(expected_profit) / 10f64.powi(dec);
        let profit_usd   = profit_human * token_price_usd;

        let gas_cost_usd = 225_000f64 * 50.0 * 1e-9 * 0.80; // ~$0.009

        Some(ArbitrageRoute {
            id:              Uuid::new_v4(),
            start_token,
            steps,
            optimal_input:   clamped,
            expected_output,
            expected_profit,
            profit_usd,
            gas_cost_usd,
            net_profit_usd:  profit_usd - gas_cost_usd,
            calculated_at:   Utc::now(),
            block_number,
        })
    }
}