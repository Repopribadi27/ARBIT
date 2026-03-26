// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/**
 * @title ArbitrageExecutor
 * @author MEV Bot (Educational Purpose)
 * @notice Kontrak eksekusi arbitrase multi-hop yang atomic.
 *
 * @dev Fitur utama:
 *   1. Eksekusi multi-hop swap (V2 dan V3) dalam satu transaksi
 *   2. Automatic revert jika profit < minProfit (kapital aman)
 *   3. Deadline protection untuk mencegah frontrun
 *   4. Re-entrancy guard
 *   5. Owner-only withdrawal
 *
 * Alur eksekusi:
 *   1. Transfer tokenIn dari owner ke contract
 *   2. Execute setiap SwapStep secara berurutan
 *   3. Hitung profit akhir
 *   4. Jika profit < minProfit → REVERT seluruh transaksi
 *   5. Jika OK → transfer profit ke owner
 */

interface IERC20 {
    function transfer(address to, uint256 amount) external returns (bool);
    function transferFrom(address from, address to, uint256 amount) external returns (bool);
    function approve(address spender, uint256 amount) external returns (bool);
    function balanceOf(address account) external view returns (uint256);
}

interface IUniswapV2Pair {
    function swap(
        uint256 amount0Out,
        uint256 amount1Out,
        address to,
        bytes calldata data
    ) external;
    function getReserves() external view returns (
        uint112 reserve0,
        uint112 reserve1,
        uint32 blockTimestampLast
    );
    function token0() external view returns (address);
    function token1() external view returns (address);
}

interface IUniswapV3Pool {
    function swap(
        address recipient,
        bool zeroForOne,
        int256 amountSpecified,
        uint160 sqrtPriceLimitX96,
        bytes calldata data
    ) external returns (int256 amount0, int256 amount1);
}

interface IUniswapV3SwapCallback {
    function uniswapV3SwapCallback(
        int256 amount0Delta,
        int256 amount1Delta,
        bytes calldata data
    ) external;
}

contract ArbitrageExecutor is IUniswapV3SwapCallback {

    // ── State Variables ────────────────────────────────────────────────────────

    address public immutable owner;
    bool    private _locked;

    // ── Events ─────────────────────────────────────────────────────────────────

    event ArbitrageExecuted(
        address indexed tokenIn,
        uint256 amountIn,
        uint256 profit,
        uint256 gasUsed,
        uint256 blockNumber
    );

    event ArbitrageFailed(
        address indexed tokenIn,
        uint256 amountIn,
        uint256 amountOut,
        uint256 minProfit
    );

    // ── Structs ────────────────────────────────────────────────────────────────

    struct SwapStep {
        address pool;      // Alamat pool (V2 pair atau V3 pool)
        address tokenIn;   // Token yang masuk ke pool
        address tokenOut;  // Token yang keluar dari pool
        uint24  fee;       // Fee tier (hanya relevan untuk V3: 500, 3000, 10000)
        bool    isV3;      // true = UniswapV3, false = UniswapV2
    }

    struct V3CallbackData {
        address tokenIn;
        address payer;
    }

    // ── Modifiers ──────────────────────────────────────────────────────────────

    modifier onlyOwner() {
        require(msg.sender == owner, "ArbitrageExecutor: not owner");
        _;
    }

    modifier nonReentrant() {
        require(!_locked, "ArbitrageExecutor: reentrant call");
        _locked = true;
        _;
        _locked = false;
    }

    modifier checkDeadline(uint256 deadline) {
        require(block.timestamp <= deadline, "ArbitrageExecutor: deadline exceeded");
        _;
    }

    // ── Constructor ────────────────────────────────────────────────────────────

    constructor() {
        owner = msg.sender;
    }

    // ── Main Execution ─────────────────────────────────────────────────────────

    /**
     * @notice Eksekusi arbitrase multi-hop secara atomic.
     *
     * @dev Jika profit akhir < minProfit, seluruh transaksi REVERT.
     *      Ini menjamin bahwa modal tidak pernah berkurang dari eksekusi
     *      yang tidak menguntungkan.
     *
     * @param steps     Array langkah swap yang membentuk siklus arbitrase
     * @param amountIn  Jumlah token awal yang diinput
     * @param minProfit Minimum profit yang harus diperoleh (dalam token awal)
     * @param deadline  Unix timestamp batas waktu eksekusi
     *
     * @return profit   Jumlah profit yang diperoleh (token awal)
     */
    function executeArbitrage(
        SwapStep[] calldata steps,
        uint256 amountIn,
        uint256 minProfit,
        uint256 deadline
    )
        external
        nonReentrant
        checkDeadline(deadline)
        returns (uint256 profit)
    {
        require(steps.length >= 2 && steps.length <= 4, "Invalid hop count");
        require(amountIn > 0, "amountIn must be > 0");
        require(
            steps[0].tokenIn == steps[steps.length - 1].tokenOut,
            "Must return to start token (not a cycle)"
        );

        // Catat saldo awal sebelum eksekusi
        address startToken      = steps[0].tokenIn;
        uint256 balanceBefore   = IERC20(startToken).balanceOf(address(this));

        // Transfer token dari caller ke contract jika perlu
        if (balanceBefore < amountIn) {
            uint256 needed = amountIn - balanceBefore;
            require(
                IERC20(startToken).transferFrom(msg.sender, address(this), needed),
                "Transfer from caller failed"
            );
        }

        uint256 gasStart = gasleft();

        // ── Execute semua swap steps ──────────────────────────────────────────
        uint256 currentAmount = amountIn;

        for (uint256 i = 0; i < steps.length; i++) {
            SwapStep calldata step = steps[i];

            if (step.isV3) {
                currentAmount = _swapV3(step, currentAmount);
            } else {
                currentAmount = _swapV2(step, currentAmount);
            }

            // Safety check: amount tidak boleh nol setelah setiap hop
            require(currentAmount > 0, "Zero amount after swap step");
        }

        // ── Profit Check ──────────────────────────────────────────────────────

        uint256 balanceAfter = IERC20(startToken).balanceOf(address(this));

        // Hitung profit berdasarkan perubahan saldo (lebih akurat dari kalkulasi)
        uint256 totalReceived = balanceAfter > balanceBefore
            ? balanceAfter - balanceBefore
            : 0;

        // Jika profit kurang dari minimum yang diharapkan → REVERT
        // Ini melindungi modal dari eksekusi yang tidak menguntungkan
        require(
            totalReceived >= amountIn + minProfit,
            "Insufficient profit: trade not profitable enough"
        );

        profit = totalReceived - amountIn;

        // ── Transfer profit ke owner ───────────────────────────────────────────
        require(
            IERC20(startToken).transfer(owner, amountIn + profit),
            "Transfer to owner failed"
        );

        uint256 gasUsed = gasStart - gasleft();

        emit ArbitrageExecuted(
            startToken,
            amountIn,
            profit,
            gasUsed,
            block.number
        );
    }

    // ── V2 Swap ────────────────────────────────────────────────────────────────

    /**
     * @dev Execute satu swap di Uniswap V2 compatible pool.
     *
     * Formula output: amountOut = (amountIn * 997 * reserveOut)
     *                           / (reserveIn * 1000 + amountIn * 997)
     */
    function _swapV2(
        SwapStep calldata step,
        uint256 amountIn
    ) internal returns (uint256 amountOut) {
        IUniswapV2Pair pair = IUniswapV2Pair(step.pool);

        // Tentukan urutan token (token0 atau token1)
        address token0 = pair.token0();
        bool    zeroForOne = (step.tokenIn == token0);

        // Fetch reserves
        (uint112 reserve0, uint112 reserve1,) = pair.getReserves();
        (uint256 reserveIn, uint256 reserveOut) = zeroForOne
            ? (uint256(reserve0), uint256(reserve1))
            : (uint256(reserve1), uint256(reserve0));

        // Hitung output dengan formula AMM
        amountOut = _getAmountOutV2(amountIn, reserveIn, reserveOut);
        require(amountOut > 0, "V2: insufficient output amount");

        // Transfer token ke pool
        require(
            IERC20(step.tokenIn).transfer(step.pool, amountIn),
            "V2: token transfer to pool failed"
        );

        // Execute swap: set amount0Out atau amount1Out
        (uint256 amount0Out, uint256 amount1Out) = zeroForOne
            ? (uint256(0), amountOut)
            : (amountOut, uint256(0));

        pair.swap(amount0Out, amount1Out, address(this), "");
    }

    /**
     * @dev Implementasi formula AMM V2: x * y = k
     */
    function _getAmountOutV2(
        uint256 amountIn,
        uint256 reserveIn,
        uint256 reserveOut
    ) internal pure returns (uint256 amountOut) {
        require(amountIn > 0,    "V2 Math: insufficient input amount");
        require(reserveIn > 0 && reserveOut > 0, "V2 Math: insufficient liquidity");

        uint256 amountInWithFee = amountIn * 997;
        uint256 numerator       = amountInWithFee * reserveOut;
        uint256 denominator     = (reserveIn * 1000) + amountInWithFee;

        amountOut = numerator / denominator;
    }

    // ── V3 Swap ────────────────────────────────────────────────────────────────

    /**
     * @dev Execute satu swap di Uniswap V3 pool.
     *
     * Menggunakan sqrtPriceLimitX96 = 0 (no price limit, tapi ada slippage
     * protection dari minProfit check di akhir).
     */
    function _swapV3(
        SwapStep calldata step,
        uint256 amountIn
    ) internal returns (uint256 amountOut) {
        IUniswapV3Pool pool = IUniswapV3Pool(step.pool);

        // Tentukan arah swap
        bool zeroForOne = step.tokenIn < step.tokenOut;

        // sqrtPriceLimitX96: batas harga
        // MIN_SQRT_RATIO + 1 untuk zeroForOne, MAX_SQRT_RATIO - 1 untuk sebaliknya
        uint160 sqrtPriceLimitX96 = zeroForOne
            ? 4295128739 + 1           // MIN_SQRT_RATIO + 1
            : 1461446703485210103287273052203988822378723970342 - 1; // MAX_SQRT_RATIO - 1

        bytes memory callbackData = abi.encode(V3CallbackData({
            tokenIn: step.tokenIn,
            payer:   address(this)
        }));

        (int256 amount0Delta, int256 amount1Delta) = pool.swap(
            address(this),      // recipient
            zeroForOne,
            int256(amountIn),   // positive = exact input
            sqrtPriceLimitX96,
            callbackData
        );

        // Output adalah nilai negatif dari delta yang berlawanan
        amountOut = zeroForOne
            ? uint256(-amount1Delta)
            : uint256(-amount0Delta);
    }

    /**
     * @dev Callback dari Uniswap V3 untuk transfer token saat swap.
     *
     * V3 menggunakan "pull" pattern: pool memanggil callback,
     * dan kita harus transfer token di sini.
     */
    function uniswapV3SwapCallback(
        int256 amount0Delta,
        int256 amount1Delta,
        bytes calldata data
    ) external override {
        // Decode callback data
        V3CallbackData memory cbData = abi.decode(data, (V3CallbackData));

        // Tentukan berapa yang harus kita bayar
        uint256 amountToPay = amount0Delta > 0
            ? uint256(amount0Delta)
            : uint256(amount1Delta);

        require(amountToPay > 0, "V3 Callback: nothing to pay");

        // Transfer token ke pool (yang sedang melakukan callback)
        require(
            IERC20(cbData.tokenIn).transfer(msg.sender, amountToPay),
            "V3 Callback: transfer failed"
        );
    }

    // ── Admin Functions ────────────────────────────────────────────────────────

    /**
     * @notice Withdraw token dari contract ke owner.
     * @dev Hanya dipanggil dalam kondisi darurat atau setelah profit terkumpul.
     */
    function withdrawToken(address token, uint256 amount) external onlyOwner {
        if (amount == 0) {
            amount = IERC20(token).balanceOf(address(this));
        }
        require(
            IERC20(token).transfer(owner, amount),
            "Withdraw failed"
        );
    }

    /**
     * @notice Withdraw native MATIC dari contract.
     */
    function withdrawMatic() external onlyOwner {
        uint256 balance = address(this).balance;
        require(balance > 0, "No MATIC to withdraw");
        (bool success, ) = owner.call{value: balance}("");
        require(success, "Transfer MATIC gagal");
    }

    /**
     * @notice Batch approve tokens untuk pools (gas optimization).
     * @dev Approve max uint256 untuk semua token × pools yang akan digunakan.
     *      Dipanggil sekali setelah deploy, bukan per transaksi.
     */
    function approveTokens(
        address[] calldata tokens,
        address[] calldata spenders
    ) external onlyOwner {
        for (uint256 i = 0; i < tokens.length; i++) {
            for (uint256 j = 0; j < spenders.length; j++) {
                IERC20(tokens[i]).approve(spenders[j], type(uint256).max);
            }
        }
    }

    // ── View Functions ─────────────────────────────────────────────────────────

    /**
     * @notice Simulasikan arbitrase tanpa state change (eth_call only).
     * @dev Berguna untuk off-chain profit check sebelum submit tx.
     */
    function simulateArbitrage(
        SwapStep[] calldata steps,
        uint256 amountIn
    ) external view returns (uint256 estimatedProfit) {
        uint256 currentAmount = amountIn;

        for (uint256 i = 0; i < steps.length; i++) {
            if (!steps[i].isV3) {
                IUniswapV2Pair pair = IUniswapV2Pair(steps[i].pool);
                address token0 = pair.token0();
                bool zeroForOne = (steps[i].tokenIn == token0);

                (uint112 r0, uint112 r1,) = pair.getReserves();
                (uint256 rIn, uint256 rOut) = zeroForOne
                    ? (uint256(r0), uint256(r1))
                    : (uint256(r1), uint256(r0));

                currentAmount = _getAmountOutV2(currentAmount, rIn, rOut);
            }
            // V3 simulation lebih kompleks - skip untuk view function
        }

        estimatedProfit = currentAmount > amountIn
            ? currentAmount - amountIn
            : 0;
    }

    // ── Receive ────────────────────────────────────────────────────────────────

    receive() external payable {}
}
