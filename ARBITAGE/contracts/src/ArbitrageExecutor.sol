// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/**
 * @title ArbitrageExecutor
 * @author MEV Bot (Educational Purpose)
 * @notice Kontrak eksekusi arbitrase multi-hop yang atomic.
 *
 * CHANGELOG v2:
 *   BUG FIX #4 — uniswapV3SwapCallback tidak memvalidasi msg.sender.
 *     Siapapun bisa memanggil callback dan menguras token dari contract.
 *     Fix: tambah state variable `_activeV3Pool` yang di-set sebelum
 *     pool.swap() dipanggil, dan di-reset sesudahnya. Callback hanya
 *     boleh dipanggil oleh pool yang sedang aktif.
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

    // BUG FIX #4: Simpan alamat V3 pool yang sedang aktif.
    // Di-set tepat sebelum pool.swap() dipanggil, di-reset sesudahnya.
    // Callback hanya boleh dipanggil oleh alamat ini.
    address private _activeV3Pool;

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
        address pool;
        address tokenIn;
        address tokenOut;
        uint24  fee;
        bool    isV3;
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

        address startToken    = steps[0].tokenIn;
        uint256 balanceBefore = IERC20(startToken).balanceOf(address(this));

        if (balanceBefore < amountIn) {
            uint256 needed = amountIn - balanceBefore;
            require(
                IERC20(startToken).transferFrom(msg.sender, address(this), needed),
                "Transfer from caller failed"
            );
        }

        uint256 gasStart    = gasleft();
        uint256 currentAmount = amountIn;

        for (uint256 i = 0; i < steps.length; i++) {
            SwapStep calldata step = steps[i];

            if (step.isV3) {
                currentAmount = _swapV3(step, currentAmount);
            } else {
                currentAmount = _swapV2(step, currentAmount);
            }

            require(currentAmount > 0, "Zero amount after swap step");
        }

        uint256 balanceAfter  = IERC20(startToken).balanceOf(address(this));
        uint256 totalReceived = balanceAfter > balanceBefore
            ? balanceAfter - balanceBefore
            : 0;

        require(
            totalReceived >= amountIn + minProfit,
            "Insufficient profit: trade not profitable enough"
        );

        profit = totalReceived - amountIn;

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

    function _swapV2(
        SwapStep calldata step,
        uint256 amountIn
    ) internal returns (uint256 amountOut) {
        IUniswapV2Pair pair = IUniswapV2Pair(step.pool);

        address token0     = pair.token0();
        bool    zeroForOne = (step.tokenIn == token0);

        (uint112 reserve0, uint112 reserve1,) = pair.getReserves();
        (uint256 reserveIn, uint256 reserveOut) = zeroForOne
            ? (uint256(reserve0), uint256(reserve1))
            : (uint256(reserve1), uint256(reserve0));

        amountOut = _getAmountOutV2(amountIn, reserveIn, reserveOut);
        require(amountOut > 0, "V2: insufficient output amount");

        require(
            IERC20(step.tokenIn).transfer(step.pool, amountIn),
            "V2: token transfer to pool failed"
        );

        (uint256 amount0Out, uint256 amount1Out) = zeroForOne
            ? (uint256(0), amountOut)
            : (amountOut, uint256(0));

        pair.swap(amount0Out, amount1Out, address(this), "");
    }

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

    function _swapV3(
        SwapStep calldata step,
        uint256 amountIn
    ) internal returns (uint256 amountOut) {
        IUniswapV3Pool pool = IUniswapV3Pool(step.pool);

        bool zeroForOne = step.tokenIn < step.tokenOut;

        uint160 sqrtPriceLimitX96 = zeroForOne
            ? 4295128739 + 1
            : 1461446703485210103287273052203988822378723970342 - 1;

        bytes memory callbackData = abi.encode(V3CallbackData({
            tokenIn: step.tokenIn,
            payer:   address(this)
        }));

        // BUG FIX #4: Catat pool yang sedang aktif SEBELUM memanggil swap.
        // Ini memastikan callback hanya bisa dipanggil oleh pool ini,
        // bukan oleh address lain yang mencoba menguras token contract.
        _activeV3Pool = step.pool;

        (int256 amount0Delta, int256 amount1Delta) = pool.swap(
            address(this),
            zeroForOne,
            int256(amountIn),
            sqrtPriceLimitX96,
            callbackData
        );

        // Reset setelah swap selesai untuk mencegah replay attack.
        _activeV3Pool = address(0);

        amountOut = zeroForOne
            ? uint256(-amount1Delta)
            : uint256(-amount0Delta);
    }

    /**
     * @dev Callback dari Uniswap V3.
     *
     * BUG FIX #4: Tambah validasi msg.sender == _activeV3Pool.
     * Tanpa validasi ini, siapapun bisa memanggil fungsi ini secara
     * langsung dan memerintahkan contract untuk transfer token ke
     * alamat sembarang — ini adalah vulnerability drain klasik di
     * kontrak DEX callback.
     */
    function uniswapV3SwapCallback(
        int256 amount0Delta,
        int256 amount1Delta,
        bytes calldata data
    ) external override {
        // SECURITY: Hanya pool yang sedang aktif yang boleh memanggil callback ini.
        require(
            msg.sender == _activeV3Pool,
            "V3 Callback: unauthorized caller"
        );
        require(_activeV3Pool != address(0), "V3 Callback: no active pool");

        V3CallbackData memory cbData = abi.decode(data, (V3CallbackData));

        uint256 amountToPay = amount0Delta > 0
            ? uint256(amount0Delta)
            : uint256(amount1Delta);

        require(amountToPay > 0, "V3 Callback: nothing to pay");

        require(
            IERC20(cbData.tokenIn).transfer(msg.sender, amountToPay),
            "V3 Callback: transfer failed"
        );
    }

    // ── Admin Functions ────────────────────────────────────────────────────────

    function withdrawToken(address token, uint256 amount) external onlyOwner {
        if (amount == 0) {
            amount = IERC20(token).balanceOf(address(this));
        }
        require(
            IERC20(token).transfer(owner, amount),
            "Withdraw failed"
        );
    }

    function withdrawMatic() external onlyOwner {
        uint256 balance = address(this).balance;
        require(balance > 0, "No MATIC to withdraw");
        (bool success, ) = owner.call{value: balance}("");
        require(success, "Transfer MATIC gagal");
    }

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
        }

        estimatedProfit = currentAmount > amountIn
            ? currentAmount - amountIn
            : 0;
    }

    // ── Receive ────────────────────────────────────────────────────────────────

    receive() external payable {}
}
