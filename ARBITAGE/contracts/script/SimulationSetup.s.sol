// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Script, console2} from "forge-std/Script.sol";
import {StdCheats} from "forge-std/StdCheats.sol";
import {ArbitrageExecutor} from "../src/ArbitrageExecutor.sol";

/**
 * @title SimulationSetup
 * @notice Script Foundry untuk menyiapkan lingkungan simulasi lengkap.
 *
 * @dev Jalankan dengan:
 *
 *   # 1. Mulai Anvil dengan mainnet fork
 *   anvil --fork-url $ALCHEMY_HTTP_URL \
 *         --chain-id 137 \
 *         --block-time 2 \
 *         --port 8545
 *
 *   # 2. Deploy dan setup (di terminal lain)
 *   forge script contracts/script/SimulationSetup.s.sol \
 *     --rpc-url http://127.0.0.1:8545 \
 *     --broadcast \
 *     --private-key $ANVIL_DEFAULT_KEY
 *
 *   # 3. Atau semua-dalam-satu tanpa Anvil:
 *   forge script contracts/script/SimulationSetup.s.sol \
 *     --fork-url $ALCHEMY_HTTP_URL \
 *     --broadcast
 *
 * Script ini akan:
 *   1. Deploy ArbitrageExecutor
 *   2. Inject saldo USDC & MATIC fiktif ke bot wallet
 *   3. Approve tokens ke pools
 *   4. Log seluruh setup ke console
 *   5. Simpan deployment info ke JSON
 */

interface IERC20Minimal {
    function balanceOf(address) external view returns (uint256);
    function approve(address, uint256) external returns (bool);
    function transfer(address, uint256) external returns (bool);
    function symbol() external view returns (string memory);
    function decimals() external view returns (uint8);
}

interface IPoolMinimal {
    function getReserves() external view returns (uint112, uint112, uint32);
    function token0() external view returns (address);
    function token1() external view returns (address);
}

contract SimulationSetup is Script, StdCheats {

    // ── Polygon Token Addresses ────────────────────────────────────────────────
    address constant WMATIC = 0x0d500B1d8E8eF31E21C99d1Db9A6444d3ADf1270;
    address constant USDC   = 0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174;
    address constant WETH   = 0x7ceB23fD6bC0adD59E62ac25578270cFf1b9f619;
    address constant USDT   = 0xc2132D05D31c914a87C6611C10748AEb04B58e8F;
    address constant DAI    = 0x8f3Cf7ad23Cd3CaDbD9735AFf958023239c6A063;
    address constant WBTC   = 0x1BFD67037B42Cf73acF2047067bd4F2C47D9BfD6;

    // ── Pool Addresses ─────────────────────────────────────────────────────────
    address constant QS_USDC_WMATIC = 0x6e7a5FAFcec6BB1e78bAE2A1F0B612012BF14827;
    address constant QS_WMATIC_WETH = 0xadbF1854e5883eB8aa7BAf50705338739e558E5b;
    address constant QS_WETH_USDC   = 0x853Ee4b2A13f8a742d64C8F088bE7bA2131f670d;
    address constant QS_USDC_USDT   = 0x2cF7252e74036d1Da831d11089D326296e64a728;
    address constant SS_USDC_WMATIC = 0xcd353F79d9FADe311fC3119B841e1f456b54e858;
    address constant SS_WMATIC_WETH = 0xc4e595acDD7d12feC385E5dA5D43160e8A0bAC0E;

    // ── Simulation Parameters ──────────────────────────────────────────────────

    // Modal fiktif yang disuntikkan ke bot wallet
    uint256 constant INJECT_USDC  = 50_000 * 1e6;    // 50,000 USDC
    uint256 constant INJECT_WMATIC = 10_000 * 1e18;  // 10,000 MATIC
    uint256 constant INJECT_WETH  = 5 * 1e18;        // 5 WETH
    uint256 constant INJECT_NATIVE = 100 ether;      // 100 MATIC native

    // ── Main Script ────────────────────────────────────────────────────────────

    function run() external {
        uint256 deployerKey = vm.envUint("PRIVATE_KEY");
        address deployer    = vm.addr(deployerKey);

        console2.log("=====================================================");
        console2.log("     MEV Arb Bot - Simulation Environment Setup      ");
        console2.log("=====================================================");
        console2.log("");
        console2.log("Deployer:", deployer);
        console2.log("Block:   ", block.number);
        console2.log("Chain:   ", block.chainid);
        console2.log("");

        vm.startBroadcast(deployerKey);

        // ── Step 1: Deploy Contract ────────────────────────────────────────────
        console2.log("=== Step 1: Deploy ArbitrageExecutor ===");
        ArbitrageExecutor executor = new ArbitrageExecutor();
        console2.log("Contract deployed at:", address(executor));
        console2.log("");

        // ── Step 2: Inject Fake Balances ──────────────────────────────────────
        console2.log("=== Step 2: Inject Fake Balances (Foundry Cheat) ===");
        console2.log("CATATAN: Ini hanya berfungsi di Anvil/test environment");
        console2.log("         Pool state tetap REAL dari mainnet fork");
        console2.log("");

        // Inject native MATIC
        vm.deal(deployer, INJECT_NATIVE);
        console2.log("MATIC injected:", INJECT_NATIVE / 1e18);

        // Inject ERC20 tokens menggunakan vm.store (modifikasi storage)
        _injectERC20Balance(deployer, USDC,   INJECT_USDC,   "USDC");
        _injectERC20Balance(deployer, WMATIC, INJECT_WMATIC, "WMATIC");
        _injectERC20Balance(deployer, WETH,   INJECT_WETH,   "WETH");

        // Transfer sebagian ke contract sebagai working capital
        uint256 contractCapital = 10_000 * 1e6; // 10,000 USDC untuk contract
        deal(USDC, address(executor), contractCapital);
        console2.log("Contract capital: 10,000 USDC");
        console2.log("");

        // ── Step 3: Approve tokens ────────────────────────────────────────────
        console2.log("=== Step 3: Approve Tokens ke Pools ===");

        address[] memory tokens = new address[](6);
        tokens[0] = USDC;
        tokens[1] = WMATIC;
        tokens[2] = WETH;
        tokens[3] = USDT;
        tokens[4] = DAI;
        tokens[5] = WBTC;

        address[] memory pools = new address[](6);
        pools[0] = QS_USDC_WMATIC;
        pools[1] = QS_WMATIC_WETH;
        pools[2] = QS_WETH_USDC;
        pools[3] = QS_USDC_USDT;
        pools[4] = SS_USDC_WMATIC;
        pools[5] = SS_WMATIC_WETH;

        executor.approveTokens(tokens, pools);
        console2.log("Approvals selesai untuk tokens:", tokens.length);
        console2.log("Ke dalam jumlah pools:", pools.length);

        vm.stopBroadcast();

        // ── Step 4: Log Pool States (read-only) ───────────────────────────────
        console2.log("=== Step 4: Real Mainnet Pool States ===");
        console2.log("(Data murni dari mainnet - tidak dimanipulasi)");
        console2.log("");

        _logPoolState("QS USDC/WMATIC", QS_USDC_WMATIC, USDC,   WMATIC);
        _logPoolState("QS WMATIC/WETH", QS_WMATIC_WETH, WMATIC, WETH);
        _logPoolState("QS WETH/USDC",   QS_WETH_USDC,   WETH,   USDC);
        _logPoolState("SS USDC/WMATIC", SS_USDC_WMATIC, USDC,   WMATIC);
        _logPoolState("SS WMATIC/WETH", SS_WMATIC_WETH, WMATIC, WETH);
        console2.log("");

        // ── Step 5: Price Analysis ────────────────────────────────────────────
        console2.log("=== Step 5: Price Difference Analysis ===");
        _analyzePriceDifference(
            "USDC->WMATIC",
            QS_USDC_WMATIC, USDC, WMATIC,
            SS_USDC_WMATIC, USDC, WMATIC
        );

        // ── Step 6: Save Deployment Info ──────────────────────────────────────
        console2.log("");
        console2.log("=== Step 6: Deployment Summary ===");
        console2.log("------------------------------------------------------");
        console2.log("  SIMULATION ENVIRONMENT READY                        ");
        console2.log("------------------------------------------------------");
        console2.log("  Contract:  ", address(executor));
        console2.log("  Capital:   50,000 USDC + 10,000 MATIC (FIKTIF)     ");
        console2.log("  Pools:     6 pools dari QuickSwap & SushiSwap       ");
        console2.log("  Mode:      Mainnet Fork (data live, capital fiktif) ");
        console2.log("------------------------------------------------------");
        console2.log("");
        console2.log("Tambahkan ke .env:");
        console2.log("  ARBITRAGE_CONTRACT=", address(executor));
        console2.log("  SIMULATION_MODE=true");
    }

    // ── Helper: Inject ERC20 Balance ──────────────────────────────────────────

    /**
     * @dev Inject balance ERC20 menggunakan Foundry's deal() atau manual
     *      storage manipulation.
     *
     * deal() secara otomatis mencari storage slot balance dan memodifikasinya.
     * Ini adalah "cheat code" Foundry yang hanya berfungsi di test/script env.
     */
    function _injectERC20Balance(
        address to,
        address token,
        uint256 amount,
        string memory symbol
    ) internal {
        // Gunakan deal() dari forge-std
        deal(token, to, amount);

        uint256 actual = IERC20Minimal(token).balanceOf(to);
        console2.log(
            string.concat(symbol, " injected: "),
            actual / (10 ** IERC20Minimal(token).decimals())
        );
    }

    // ── Helper: Log Pool State ─────────────────────────────────────────────────

    function _logPoolState(
        string memory name,
        address pool,
        address tokenA,
        address tokenB
    ) internal view {
        (uint112 r0, uint112 r1,) = IPoolMinimal(pool).getReserves();
        address t0 = IPoolMinimal(pool).token0();

        uint256 rA = (t0 == tokenA) ? uint256(r0) : uint256(r1);
        uint256 rB = (t0 == tokenA) ? uint256(r1) : uint256(r0);

        uint8 decA = IERC20Minimal(tokenA).decimals();
        uint8 decB = IERC20Minimal(tokenB).decimals();
        string memory symA = IERC20Minimal(tokenA).symbol();
        string memory symB = IERC20Minimal(tokenB).symbol();

        // Spot price (scaled untuk integer display)
        uint256 spotPrice = (rB * 1e6) / (rA > 0 ? rA : 1);

        console2.log(
            string.concat(
                name, " | ",
                symA, ": ", vm.toString(rA / (10 ** decA)),
                " | ",
                symB, ": ", vm.toString(rB / (10 ** decB)),
                " | Price(", symB, "/", symA, "): x10^-6 = ", vm.toString(spotPrice)
            )
        );
    }

    // ── Helper: Analyze Price Difference ─────────────────────────────────────

    function _analyzePriceDifference(
        string memory pairName,
        address pool1,  address t1in, address t1out,
        address pool2,  address t2in, address t2out
    ) internal view {
        (uint112 p1r0, uint112 p1r1,) = IPoolMinimal(pool1).getReserves();
        address p1t0 = IPoolMinimal(pool1).token0();
        uint256 p1in  = (p1t0 == t1in) ? uint256(p1r0) : uint256(p1r1);
        uint256 p1out = (p1t0 == t1in) ? uint256(p1r1) : uint256(p1r0);

        (uint112 p2r0, uint112 p2r1,) = IPoolMinimal(pool2).getReserves();
        address p2t0 = IPoolMinimal(pool2).token0();
        uint256 p2in  = (p2t0 == t2in) ? uint256(p2r0) : uint256(p2r1);
        uint256 p2out = (p2t0 == t2in) ? uint256(p2r1) : uint256(p2r0);

        // Price dalam unit 1e12
        uint256 price1 = (p1out * 1e12) / (p1in > 0 ? p1in : 1);
        uint256 price2 = (p2out * 1e12) / (p2in > 0 ? p2in : 1);

        uint256 spreadBps;
        string memory direction;
        if (price1 > price2) {
            spreadBps = ((price1 - price2) * 10000) / price2;
            direction = "Pool1 > Pool2 (Buy Pool2, Sell Pool1)";
        } else {
            spreadBps = ((price2 - price1) * 10000) / price1;
            direction = "Pool2 > Pool1 (Buy Pool1, Sell Pool2)";
        }

        console2.log(
            string.concat(
                pairName,
                " | Spread: ", vm.toString(spreadBps), " bps",
                " | ", direction,
                spreadBps > 60 ? " *** POTENTIAL ARB ***" : " (efficient)"
            )
        );
    }
}
