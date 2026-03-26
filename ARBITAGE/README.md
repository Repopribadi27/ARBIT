# 🤖 MEV Multi-Hop Arbitrage Bot

**Rust + Alloy + Solidity | Polygon Mainnet | Educational MEV System**

Bot arbitrase segitiga (triangular arbitrage) berperforma tinggi untuk jaringan EVM berbiaya rendah.
Dirancang dengan pendekatan simulasi-pertama sebelum live trading.

---

## 📐 Arsitektur Sistem

```
┌─────────────────────────────────────────────────────────────────────┐
│                     MEV ARBITRAGE BOT ARCHITECTURE                   │
│                                                                       │
│  DATA LAYER (WebSocket)           LOGIC LAYER              EXEC LAYER │
│  ─────────────────────            ─────────────            ──────────│
│                                                                       │
│  Alchemy WSS                                                          │
│  (Free Tier)                                                          │
│       │                                                               │
│       ▼ Sync Events                                                   │
│  ┌─────────────┐    update    ┌──────────────────┐                   │
│  │ DexMonitor  │ ──────────► │  PoolRegistry     │                   │
│  │ (Alloy WS)  │             │  (DashMap<Addr,   │                   │
│  └─────────────┘             │   PoolStateV2>)   │                   │
│                              └────────┬─────────┘                   │
│                                       │ update                        │
│                              ┌────────▼─────────┐                   │
│                              │  ArbitrageGraph   │                   │
│                              │  (Nodes=Tokens,   │                   │
│                              │   Edges=Pools,    │                   │
│                              │   w=-ln(rate*fee))│                   │
│                              └────────┬─────────┘                   │
│                                       │ detect                        │
│                              ┌────────▼─────────┐                   │
│                              │  BellmanFord      │                   │
│                              │  Detector         │                   │
│                              │  (negative cycles)│                   │
│                              └────────┬─────────┘                   │
│                                       │ cycles                        │
│                              ┌────────▼─────────┐                   │
│                              │  RouteEvaluator   │                   │
│                              │  · optimal_input  │                   │
│                              │  · simulate_hops  │                   │
│                              │  · profit > gas?  │                   │
│                              └────────┬─────────┘                   │
│                                       │ routes                        │
│                              ┌────────▼─────────┐    ┌────────────┐ │
│                              │  TradeExecutor    │───►│ Arbitrage  │ │
│                              │  · Simulation     │    │ Executor   │ │
│                              │  · Live           │    │ (Solidity) │ │
│                              └──────────────────┘    └────────────┘ │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 🧮 Matematika AMM

### Formula Dasar (Uniswap V2 / x·y = k)

```
amount_out = (amount_in × 997 × reserve_out)
           / (reserve_in × 1000 + amount_in × 997)
```

Fee 0.3% direpresentasikan sebagai: `fee_numerator = 997`, `fee_denominator = 1000`

### Optimal Input untuk 2-Hop Arbitrase

Untuk A→B→A melalui dua pool berbeda:
- Pool 1: (R_A1, R_B1) dengan fee f = 0.997
- Pool 2: (R_B2, R_A2) dengan fee f = 0.997

**Derivasi:**
```
Output hop 2:
  c(x) = f² × x × R_B1 × R_A2
        / (R_A1 × R_B2 + f×x × (R_B2 + f×R_B1))

Profit P(x) = c(x) - x → dP/dx = 0:

  (M + N×x)² = K×M

dimana:
  K = f² × R_B1 × R_A2
  M = R_A1 × R_B2  
  N = f × (R_B2 + f×R_B1)

Solusi: x* = (√(K×M) - M) / N
```

### Deteksi Arbitrase dengan Bellman-Ford

Transformasi log-harga untuk menggunakan Bellman-Ford:
```
weight(edge A→B) = -ln(price_A→B × fee_multiplier)

Siklus arbitrase = negative-weight cycle:
  Σ weights < 0
  ⟺ Π (price_i × fee_i) > 1
  ⟺ Ada profit setelah semua fee
```

---

## 📁 Struktur Project

```
mev-arb-bot/
├── Cargo.toml                    # Dependencies Rust
├── .env.example                  # Template konfigurasi
│
├── src/
│   ├── main.rs                   # Entry point & orchestration
│   ├── config.rs                 # Load environment & validate
│   ├── types.rs                  # Core types (PoolState, Route, etc.)
│   ├── math/
│   │   └── mod.rs               # AMM math (integer precision)
│   ├── graph/
│   │   └── mod.rs               # Graph + Bellman-Ford detector
│   ├── monitor/
│   │   └── mod.rs               # WebSocket monitor (Alloy)
│   └── executor/
│       └── mod.rs               # Trade execution + logging
│
└── contracts/
    ├── foundry.toml              # Foundry config
    ├── src/
    │   └── ArbitrageExecutor.sol # Smart contract atomic execution
    ├── test/
    │   └── ArbitrageExecutor.t.sol # Test suite (mainnet fork)
    └── script/
        └── SimulationSetup.s.sol # Deploy & inject fake capital
```

---

## 🚀 Cara Menjalankan

### Prerequisites

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install Foundry
curl -L https://foundry.paradigm.xyz | bash
foundryup

# Clone & setup
cp .env.example .env
# Edit .env dengan API key Alchemy Anda
```

### Mode Simulasi (Rekomendasi untuk Belajar)

```bash
# Terminal 1: Jalankan Anvil dengan mainnet fork
anvil \
  --fork-url $ALCHEMY_HTTP_URL \
  --chain-id 137 \
  --block-time 2 \
  --port 8545

# Terminal 2: Deploy contract & inject modal fiktif
cd contracts
forge install foundry-rs/forge-std
forge script script/SimulationSetup.s.sol \
  --rpc-url http://127.0.0.1:8545 \
  --broadcast \
  --private-key $PRIVATE_KEY

# Terminal 3: Jalankan bot dalam mode simulasi
SIMULATION_MODE=true cargo run --release

# Terminal 4 (optional): Jalankan test suite lengkap
forge test --fork-url $ALCHEMY_HTTP_URL -vvv
```

### Mode Live (Setelah Simulasi Berhasil)

```bash
# Deploy contract ke Polygon Mainnet
forge script script/SimulationSetup.s.sol \
  --rpc-url $ALCHEMY_HTTP_URL \
  --broadcast \
  --verify \
  --etherscan-api-key $POLYGONSCAN_API_KEY \
  --private-key $PRIVATE_KEY

# Jalankan bot live
SIMULATION_MODE=false cargo run --release
```

---

## 🧪 Test Suite Foundry

```bash
cd contracts

# Install dependencies
forge install

# Jalankan semua test dengan mainnet fork
forge test --fork-url $ALCHEMY_HTTP_URL -vvv

# Test spesifik
forge test --fork-url $ALCHEMY_HTTP_URL \
  --match-test test_SimulateArbitrage_USDC_WMATIC_WETH_USDC -vvvv

# Gas profiling
forge test --fork-url $ALCHEMY_HTTP_URL \
  --match-test test_GasProfiling \
  --gas-report

# Coverage report
forge coverage --fork-url $ALCHEMY_HTTP_URL
```

### Hasil Test yang Diharapkan

```
[PASS] test_Deployment
[PASS] test_InjectFakeBalance
  → USDC:  50,000 USDC (fiktif)
  → MATIC: 10,000 MATIC (fiktif)
[INFO] test_SimulateArbitrage_USDC_WMATIC_WETH_USDC
  → Log tabel profit per input amount
[INFO] test_ExecuteArbitrage_WithFakeCapital
  → Eksekusi trade di pool mainnet dengan modal fiktif
[PASS] test_DeadlineProtection
[PASS] test_OnlyOwnerWithdraw
[PASS] test_ReentrancyProtection
```

---

## 📊 Interpretasi Log Bot

```log
🤖 MEV Multi-Hop Arbitrage Bot v0.1.0
   Mode: 🧪 SIMULASI
   Min Profit: $0.50
   
🔌 Menghubungkan ke WebSocket: ws://127.0.0.1:8545
✅ WebSocket terhubung
🔍 Menemukan pools dari 2 factory...
📊 Total 15 pools ditemukan  ← Pool dengan liquiditas cukup
📡 Subscribe Sync events untuk 15 pools

📈 Metrics | Pools: 15 | Events: 1,234 | Opportunities: 3 | Net P&L: $0.00
💡 2 siklus terdeteksi di block 48291234
🎯 Peluang: profit est $1.23 | Log weight: -0.008123
   ← Negative log weight = profitable cycle
```

### Frekuensi Peluang (Ekspektasi Realistis)

| Kondisi Pasar | Frekuensi Peluang | Profit per Trade |
|---------------|-------------------|-----------------|
| Volatile (dump/pump besar) | 5-15x / jam | $2-50 |
| Normal | 0-3x / jam | $0.5-5 |
| Sangat Efisien | Sangat Jarang | < $0.5 |

> **Catatan:** Polygon adalah jaringan yang cukup efisien. Peluang arbitrase
> muncul lebih banyak saat ada volatilitas atau ketika pool baru mendapat
> liquidity besar secara tiba-tiba.

---

## 🔒 Keamanan Smart Contract

### Mekanisme Perlindungan Modal

```solidity
// Revert otomatis jika profit < minProfit
require(
    totalReceived >= amountIn + minProfit,
    "Insufficient profit: trade not profitable enough"
);
```

Ini berarti:
- **Jika tidak profitable** → Transaksi REVERT, gas saja yang hilang (~$0.001 di Polygon)
- **Modal tidak pernah berkurang** karena kalah arbitrase
- **Deadline protection** mencegah eksekusi transaksi yang sudah basi

### Security Checklist

- [x] Reentrancy guard (`nonReentrant` modifier)
- [x] Owner-only admin functions
- [x] Deadline protection untuk setiap trade
- [x] Minimum profit enforcement (profit-or-revert)
- [x] Integer math (tidak ada floating point di Solidity)
- [ ] Audit independen (WAJIB sebelum live trading!)
- [ ] Formal verification (opsional tapi direkomendasikan)

---

## ⚡ Performa & Optimasi

### Rust Bot
- **Async tokio**: Semua operasi I/O non-blocking
- **DashMap**: Concurrent hashmap untuk pool state tanpa mutex bottleneck
- **Channel buffering**: 10,000 event buffer untuk burst traffic
- **Ternary search**: O(log n) optimal input calculation

### Solidity Contract
- **`optimizer_runs = 1_000_000`**: Optimasi untuk runtime (bukan deployment)
- **Batch approvals**: Approve semua token sekali, bukan per trade
- **Minimal storage reads**: Hindari sload berulang dalam satu transaksi

### Gas Estimate (Polygon)
```
2-hop trade:  ~180,000 gas × 50 Gwei = 0.009 MATIC ≈ $0.008
3-hop trade:  ~250,000 gas × 50 Gwei = 0.0125 MATIC ≈ $0.011
```
Gas sangat murah di Polygon → threshold profit bisa sangat rendah!

---

## 📚 Referensi & Bahan Belajar

- [Uniswap V2 Whitepaper](https://uniswap.org/whitepaper.pdf)
- [Alloy Book](https://alloy.rs/book/)
- [Foundry Book](https://book.getfoundry.sh/)
- [MEV Explore](https://explore.flashbots.net/)
- "Triangular Arbitrage in Cryptocurrency" - Academic Paper
- [Bellman-Ford Algorithm](https://en.wikipedia.org/wiki/Bellman%E2%80%93Ford_algorithm)

---

## ⚠️ Disclaimer

Bot ini dibuat untuk tujuan **edukasi dan penelitian**.
- Tidak ada jaminan profit dalam live trading
- Selalu mulai dengan simulasi sebelum live
- Gunakan modal yang siap hilang
- Lakukan audit keamanan sebelum deploy ke mainnet
- Pahami risiko MEV dan front-running oleh bot lain
