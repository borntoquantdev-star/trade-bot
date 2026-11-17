"""The risk gate: the one function that decides whether an order may exist.

Contract - do not break it without discussing first:

* **Pure.** No network, no file, no clock, no randomness, no LLM.  ``now`` is
  passed in.  The same inputs always produce the same output, which is why it
  can be unit tested exhaustively and reused unchanged by a backtest.
* **Synchronous.** Never ``async``.  A gate that can await is a gate that can
  be raced.
* **Sizing never reads "confidence".**  Position size comes from the risk
  budget and the stop distance, nothing else.  A strategy cannot talk its way
  into a bigger position.
* **Most serious reason wins.**  Checks run from the most to the least
  serious, so the log explains what actually stopped the bot.
"""

from __future__ import annotations

import hashlib
from datetime import datetime, timedelta, timezone
from decimal import Decimal

from .money import ONE, ZERO
from .types import (
    InstrumentSpec,
    PortfolioState,
    Reject,
    RejectCode,
    RiskLimits,
    Side,
    Signal,
    SizedOrder,
)

Decision = SizedOrder | Reject

#: How many decimal places a money figure keeps when it appears in a message.
#: Display only: nothing computed here is ever rounded.
_SHOWN_PLACES = Decimal("0.0001")


def show(value: Decimal) -> str:
    """Format a money figure for a human, without the division tail.

    ``equity * max_daily_loss`` is an exact Decimal with as many digits as the
    two operands imply, which for a 3% limit on a 9,800 balance is twenty-odd
    of them. The reader needs four.
    """
    if not value.is_finite():
        return str(value)
    if value == value.to_integral_value():
        return str(value.quantize(Decimal("1")))
    rounded = value.quantize(_SHOWN_PLACES)
    if rounded == 0:
        # Never print a non-zero amount as 0: "would risk 0" reads as a bug
        # report about the bot rather than a fact about the number.
        return format(value.normalize(), "f")
    return str(rounded.normalize())


def client_order_id(signal: Signal) -> str:
    """A deterministic idempotency key for *signal*.

    The same signal always produces the same key, so a retry after a timeout
    cannot turn into a second position.  The key covers everything that makes
    the order distinct - symbol, side, the prices it was built from, and the
    window it is valid for.

    The result is 32 hex characters, which is exactly the 128-bit client order
    id ("cloid") Hyperliquid accepts, so the exchange enforces the same
    uniqueness we do and will refuse a duplicate itself.

    >>> s = Signal.build("BTC", "buy", 100, 99, valid_for_seconds=60)
    >>> client_order_id(s) == client_order_id(s)
    True
    >>> len(client_order_id(s))
    32
    """
    payload = "|".join(
        [
            signal.symbol,
            signal.side.value,
            format(signal.ref_price, "f"),
            format(signal.stop_price, "f"),
            signal.valid_until.isoformat(),
        ]
    )
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()[:32]


def risk_gate(
    signal: Signal,
    state: PortfolioState,
    limits: RiskLimits,
    spec: InstrumentSpec,
    mark_price: Decimal,
    now: datetime,
) -> Decision:
    """Approve *signal* as a :class:`SizedOrder`, or refuse it.

    Args:
        signal: What the strategy wants to do.
        state: Account snapshot assembled by the engine.
        limits: The ceilings from the config file.
        spec: Exchange rules for the symbol.
        mark_price: The price right now, to measure drift since the signal.
        now: Current time, timezone-aware.  Passed in so the gate stays pure.

    Returns:
        A :class:`SizedOrder` that is safe to send, or a :class:`Reject`
        carrying the most serious reason it was refused.
    """
    # 0. Inputs must describe the same instrument.
    if spec.symbol != signal.symbol:
        return Reject(
            RejectCode.BAD_INSTRUMENT,
            f"instrument spec is for {spec.symbol} but the signal is for {signal.symbol}",
        )
    if mark_price <= ZERO:
        return Reject(
            RejectCode.BAD_INSTRUMENT,
            f"mark price must be positive, got {mark_price}",
        )
    if now.tzinfo is None:
        raise ValueError("now must be timezone-aware (use UTC)")

    # 1. Halted - a human pulled the lever, only a human may push it back.
    if state.halted:
        return Reject(
            RejectCode.HALTED,
            f"trading is halted: {state.halt_reason or 'no reason recorded'}",
        )

    # 2. Max drawdown - the kill switch, measured from peak equity.
    floor_equity = state.peak_equity * (ONE - limits.max_drawdown)
    if state.equity <= floor_equity:
        return Reject(
            RejectCode.MAX_DRAWDOWN,
            f"equity {show(state.equity)} is at or below the drawdown floor "
            f"{show(floor_equity)} (peak {show(state.peak_equity)}, "
            f"limit {show(limits.max_drawdown * 100)}%)",
        )

    # 3. Daily loss limit - realised losses booked today.
    daily_budget = state.equity * limits.max_daily_loss
    if state.realized_pnl_today <= -daily_budget:
        return Reject(
            RejectCode.DAILY_LOSS_LIMIT,
            f"realised loss on {trading_day(now)} {show(state.realized_pnl_today)} has reached "
            f"the daily budget of -{show(daily_budget)}",
        )

    # 4. Expired - never open a position on a stale signal.
    if now > signal.valid_until:
        return Reject(
            RejectCode.EXPIRED,
            f"signal expired at {signal.valid_until.isoformat()}, now {now.isoformat()}",
        )

    # 5. Direction allowed?
    if signal.side is Side.SELL and not limits.allow_short:
        return Reject(
            RejectCode.SHORT_NOT_ALLOWED,
            "this strategy is configured long-only (risk.allow_short: false)",
        )

    # 6. Stop placement must be real and on the correct side of entry.
    if signal.side is Side.BUY and signal.stop_price >= signal.ref_price:
        return Reject(
            RejectCode.INVALID_STOP,
            f"a long needs its stop below entry: stop {signal.stop_price} "
            f">= entry {signal.ref_price}",
        )
    if signal.side is Side.SELL and signal.stop_price <= signal.ref_price:
        return Reject(
            RejectCode.INVALID_STOP,
            f"a short needs its stop above entry: stop {signal.stop_price} "
            f"<= entry {signal.ref_price}",
        )
    if signal.stop_distance < spec.tick_size:
        return Reject(
            RejectCode.INVALID_STOP,
            f"stop distance {signal.stop_distance} is smaller than one tick "
            f"({spec.tick_size}); it would be filled by noise alone",
        )
    if signal.take_profit is not None:
        tp_wrong_way = (signal.side is Side.BUY and signal.take_profit <= signal.ref_price) or (
            signal.side is Side.SELL and signal.take_profit >= signal.ref_price
        )
        if tp_wrong_way:
            return Reject(
                RejectCode.INVALID_STOP,
                f"take profit {signal.take_profit} is on the losing side of "
                f"entry {signal.ref_price} for a {signal.side.value}",
            )

    # 7. Slippage - has the market already run away from the signal?
    drift = (mark_price - signal.ref_price) / signal.ref_price * signal.side.sign
    if drift > limits.max_slippage:
        return Reject(
            RejectCode.SLIPPAGE_EXCEEDED,
            f"price moved {drift * 100:.4f}% against the signal since it was built, "
            f"over the {limits.max_slippage * 100}% allowed "
            f"(signal {signal.ref_price}, now {mark_price})",
        )

    # 8. One position per symbol: never stack onto an open trade.
    if signal.symbol in state.positions:
        held = state.positions[signal.symbol]
        return Reject(
            RejectCode.ALREADY_IN_POSITION,
            f"already holding {held.qty} {held.symbol} ({held.side.value}); "
            "close it before opening another",
        )

    # 9. Concurrent position ceiling.
    if len(state.positions) >= limits.max_positions:
        return Reject(
            RejectCode.TOO_MANY_POSITIONS,
            f"{len(state.positions)} positions open, ceiling is {limits.max_positions}",
        )

    # --- Sizing -----------------------------------------------------------
    #
    # Being stopped out costs the stop distance *plus* the fees on both legs,
    # and those fees depend on the quantity we are still solving for.  Rather
    # than padding a guess, solve the equation:
    #
    #     risk = qty * stop_distance + qty * (entry + stop) * fee_rate
    #     qty  = risk / (stop_distance + (entry + stop) * fee_rate)
    #
    risk_budget = state.equity * limits.risk_per_trade
    fee_leg = (signal.ref_price + signal.stop_price) * limits.fee_rate
    cost_per_unit = signal.stop_distance + fee_leg
    qty = spec.round_qty(risk_budget / cost_per_unit)

    # 10. Too little capital for this stop distance.
    if qty <= ZERO:
        smallest = spec.lot_step * cost_per_unit
        return Reject(
            RejectCode.QTY_ROUNDS_TO_ZERO,
            f"risk budget {show(risk_budget)} buys less than one lot step "
            f"({spec.lot_step}); the smallest possible order would risk {show(smallest)}",
        )

    # The limit price caps slippage inside the order itself: the exchange is
    # not allowed to fill us worse than this, however far price travels while
    # the order is in flight.
    if signal.side is Side.BUY:
        limit_price = spec.round_price(
            signal.ref_price * (ONE + limits.max_slippage), favour="down"
        )
    else:
        limit_price = spec.round_price(signal.ref_price * (ONE - limits.max_slippage), favour="up")
    if limit_price <= ZERO:
        return Reject(
            RejectCode.BAD_INSTRUMENT,
            f"limit price rounded to {limit_price} with tick size {spec.tick_size} "
            f"and {spec.price_sig_figs} significant figures",
        )

    # 11. Exchange minimum order size, counted in the base asset.
    #     GRVT publishes min_size; venues without the rule leave it at zero.
    if spec.min_qty > ZERO and qty < spec.min_qty:
        return Reject(
            RejectCode.BELOW_MIN_SIZE,
            f"order size {qty} is below the exchange minimum {spec.min_qty} " f"for {spec.symbol}",
        )

    notional = qty * limit_price

    # 12. Exchange minimum order value.
    if notional < spec.min_notional:
        return Reject(
            RejectCode.BELOW_MIN_NOTIONAL,
            f"order value {show(notional)} is below the exchange minimum "
            f"{show(spec.min_notional)} for {spec.symbol}",
        )

    # 13. Total exposure ceiling (the only place leverage is enforced).
    leverage_cap = min(limits.max_leverage, spec.max_leverage)
    exposure_cap = state.equity * leverage_cap
    new_exposure = state.gross_exposure + notional
    if new_exposure > exposure_cap:
        return Reject(
            RejectCode.EXCEEDS_EXPOSURE,
            f"total exposure would be {show(new_exposure)}, over the cap "
            f"{show(exposure_cap)} (equity {show(state.equity)} x {leverage_cap})",
        )

    return SizedOrder(
        client_order_id=client_order_id(signal),
        symbol=signal.symbol,
        side=signal.side,
        qty=qty,
        limit_price=limit_price,
        stop_price=signal.stop_price,
        take_profit=signal.take_profit,
        risk_amount=qty * cost_per_unit,
        reason=signal.reason,
    )


def explain(decision: Decision) -> str:
    """One human-readable line describing *decision* (for logs and the CLI)."""
    if isinstance(decision, Reject):
        return f"REFUSED [{decision.code.value}] {decision.detail}"
    return (
        f"APPROVED {decision.side.value} {decision.qty} {decision.symbol} "
        f"@ limit {decision.limit_price} (stop {decision.stop_price}, "
        f"risking {show(decision.risk_amount)}, id {decision.client_order_id})"
    )


__all__ = [
    "Decision",
    "client_order_id",
    "explain",
    "risk_gate",
    "show",
    "TRADING_DAY_TZ",
    "trading_day",
]


#: Where the daily loss budget's day begins: midnight in Bangkok.  A UTC day
#: would reset the budget at seven in the morning, so a bad evening and the
#: small hours after it shared one budget while "today" on the board did not.
#: Thailand has no daylight saving, so a fixed offset is exact all year and
#: needs no tz database (which a bare Windows Python does not ship).
TRADING_DAY_TZ = timezone(timedelta(hours=7), "Asia/Bangkok")


def trading_day(now: datetime) -> str:
    """The trading day ``now`` falls in, as ``YYYY-MM-DD`` in Thai time.

    The live engine and the backtester both roll the daily budget on this, so
    a backtest's daily stop trips on the same boundary the live bot's does.
    """
    if now.tzinfo is None:
        raise ValueError("now must be timezone-aware (use UTC)")
    return now.astimezone(TRADING_DAY_TZ).date().isoformat()
