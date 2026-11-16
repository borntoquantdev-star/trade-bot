"""Replaying stored candles through the real risk gate.

This is the whole reason :func:`~tradebot.risk.risk_gate` was written as a
pure function.  A backtest that re-implements position sizing is a backtest of
a bot you do not own: it proves that *some* bot would have made money.  Here
the sizing, the stop checks, the daily loss limit and the exposure ceiling all
come from the same call the live engine makes, with the same config file.

What the replay adds, and the live engine does not need, is a model of how an
order meets a market:

* **A signal is acted on one bar later.**  The strategy sees a bar close and
  decides; the order can only meet the market on the bar after that.  Reading
  the same bar you traded on is the oldest way to build a profitable backtest
  and a losing bot.
* **A limit order fills, or it does not.**  The gate sets the limit price as a
  slippage guard, so a bar that never trades through it simply leaves the
  order unfilled, counted and reported.  Filling everything at the close is
  how a backtest learns to lie.
* **When a bar holds both the stop and the target, the stop wins.**  Without
  tick data there is no way to know which came first, and assuming the happy
  one turns a losing strategy into a winning chart.
* **Gaps are honoured.**  If a bar opens straight through the stop, the exit
  is the open, not the stop price.  That is where real losses exceed planned
  ones, and a backtest that hides it is hiding the risk it exists to measure.

Fees are charged on both sides at ``risk.fee_rate``.  Funding is not modelled;
on a perpetual held for days that matters, and the report says so rather than
quietly leaving it out.
"""

from __future__ import annotations

import logging
from collections import Counter, deque
from collections.abc import Sequence
from dataclasses import dataclass, replace
from datetime import datetime, timedelta
from decimal import Decimal

from ..core.config import BotConfig
from ..intervals import interval_delta
from ..money import ZERO
from ..risk import risk_gate, trading_day
from ..strategies.base import MarketView, Strategy
from ..types import Candle, PortfolioState, Position, Reject, RejectCode, Side, Signal, SizedOrder
from .result import BacktestResult, EquityPoint, Trade

#: Reject codes the live engine turns into a halt.  Mirrored here so a
#: backtest stops where the real bot would stop, instead of carrying on and
#: reporting profits the bot would never have been allowed to take.
HALTING = (RejectCode.MAX_DRAWDOWN, RejectCode.DAILY_LOSS_LIMIT)


class BacktestError(ValueError):
    """The run cannot start, and guessing would make the result meaningless."""


@dataclass
class _Open:
    """A position the replay is holding."""

    order: SizedOrder
    entry_at: datetime
    entry_price: Decimal
    entry_fee: Decimal
    bars_held: int = 0

    @property
    def position(self) -> Position:
        return Position(
            symbol=self.order.symbol,
            side=self.order.side,
            qty=self.order.qty,
            entry_price=self.entry_price,
        )

    def unrealized(self, price: Decimal) -> Decimal:
        return (price - self.entry_price) * self.order.qty * self.order.side.sign


def fill_price(order: SizedOrder, bar: Candle) -> Decimal | None:
    """Where *order* would fill on *bar*, or ``None`` if it would not.

    A buy limit fills at the open when the market gaps below it (better than
    asked, which is what a limit order means), otherwise at the limit if the
    bar traded down to it.  A sell is the mirror image.
    """
    if order.side is Side.BUY:
        if bar.open <= order.limit_price:
            return bar.open
        if bar.low <= order.limit_price:
            return order.limit_price
        return None
    if bar.open >= order.limit_price:
        return bar.open
    if bar.high >= order.limit_price:
        return order.limit_price
    return None


def _asked_to_leave(
    strategy: Strategy,
    held: _Open,
    bar: Candle,
    symbol: str,
    cash: Decimal,
    window: Sequence[Decimal],
    log: logging.Logger,
) -> str | None:
    """Does the strategy itself want out of this position on this bar?

    Exits at the bar's close, which is the only price the replay knows the
    strategy could have acted on: it is handed the close, so it cannot be
    given a fill at a price it never saw.
    """
    market = MarketView(
        symbol=symbol,
        price=bar.close,
        now=bar.open_time,
        equity=cash + held.unrealized(bar.close),
        position=held.position,
        recent_prices=tuple(window) + (bar.close,),
    )
    try:
        return strategy.get_exit(market)
    except Exception as exc:  # a broken strategy must not end the replay
        log.warning("%s: strategy.get_exit raised %r", bar.open_time, exc)
        return None


def exit_on_bar(
    side: Side, stop: Decimal, take_profit: Decimal | None, bar: Candle
) -> tuple[Decimal, str] | None:
    """Where and why a position leaves on *bar*, if it leaves at all.

    The stop is checked first on purpose; see the module docstring.
    """
    if side is Side.BUY:
        if bar.open <= stop:
            return bar.open, "stop (gap)"
        if bar.low <= stop:
            return stop, "stop"
        if take_profit is not None:
            if bar.open >= take_profit:
                return bar.open, "target (gap)"
            if bar.high >= take_profit:
                return take_profit, "target"
        return None
    if bar.open >= stop:
        return bar.open, "stop (gap)"
    if bar.high >= stop:
        return stop, "stop"
    if take_profit is not None:
        if bar.open <= take_profit:
            return bar.open, "target (gap)"
        if bar.low <= take_profit:
            return take_profit, "target"
    return None


def run_backtest(
    candles: Sequence[Candle],
    strategy: Strategy,
    config: BotConfig,
    log: logging.Logger | None = None,
    exchange: str = "",
    halt_on_limits: bool = True,
) -> BacktestResult:
    """Replay *candles* through *strategy* and the real risk gate.

    Args:
        candles: Closed bars, oldest first, one symbol and one interval.
        strategy: Any :class:`~tradebot.strategies.base.Strategy`.
        config: The same config the bot trades with.  Risk limits, the
            instrument spec and the starting equity all come from it, so a
            backtest cannot quietly use friendlier numbers than the live run.
        exchange: Only a label for the report.
        halt_on_limits: Stop trading when the gate reports a limit the live
            engine would halt on.  Turn it off to see what the rest of the
            period would have done, knowing the real bot would not have been
            there to see it.

    Returns:
        A :class:`~tradebot.backtest.result.BacktestResult`.
    """
    log = log or logging.getLogger("tradebot.backtest")
    bars = list(candles)
    if not bars:
        raise BacktestError(
            "no candles to run on. Download some first: " "tradebot --fetch-candles --days 365"
        )

    spec = config.instrument
    symbols = {bar.symbol for bar in bars}
    if len(symbols) != 1:
        raise BacktestError(f"candles mix symbols: {sorted(symbols)}. One run covers one market.")
    symbol = symbols.pop()
    if spec.symbol != symbol:
        raise BacktestError(
            f"the config describes {spec.symbol} but the candles are {symbol}. "
            "A backtest with the wrong tick size and minimum order is worse than none."
        )
    intervals = {bar.interval for bar in bars}
    if len(intervals) != 1:
        raise BacktestError(f"candles mix intervals: {sorted(intervals)}")
    interval = intervals.pop()
    step = interval_delta(interval)

    bars.sort(key=lambda bar: bar.open_time)
    result = BacktestResult(
        symbol=symbol,
        interval=interval,
        strategy=strategy.name,
        exchange=exchange,
        starting_equity=config.starting_equity,
        bars=len(bars),
        start=bars[0].open_time,
        end=bars[-1].open_time + step,
        first_price=bars[0].open,
        last_price=bars[-1].close,
        rejects=Counter(),
    )

    limits = config.risk
    window: deque[Decimal] = deque(maxlen=max(config.trading.warmup_candles, 2))
    cash = config.starting_equity
    peak_equity = cash
    day = ""
    day_start_equity = cash
    halted = False
    halt_reason = ""
    held: _Open | None = None
    pending: SizedOrder | None = None
    restamped = False

    strategy.initialize()

    for index, bar in enumerate(bars):
        now = bar.open_time + step  # a decision is made when the bar closes

        # 1. An order approved on the previous bar meets this one.
        if pending is not None:
            price = fill_price(pending, bar)
            if price is None:
                result.unfilled_orders += 1
                log.debug(
                    "%s: limit %s never traded, order dropped", bar.open_time, pending.limit_price
                )
            else:
                fee = price * pending.qty * limits.fee_rate
                cash -= fee
                held = _Open(
                    order=pending, entry_at=bar.open_time, entry_price=price, entry_fee=fee
                )
                log.debug(
                    "%s: filled %s %s at %s", bar.open_time, pending.side.value, pending.qty, price
                )
            pending = None

        # 2. A position already open may leave on this bar.
        if held is not None:
            held.bars_held += 1
            leaving = exit_on_bar(
                held.order.side, held.order.stop_price, held.order.take_profit, bar
            )
            if leaving is not None:
                exit_price, why = leaving
                cash = _close(cash, held, exit_price, bar.open_time, why, limits.fee_rate, result)
                held = None
            else:
                # The stop and the target are the market's reasons to leave.
                # A strategy may have its own - a time limit, a signal that
                # faded - and the live loop honours those, so the replay has
                # to as well, or the two would be testing different bots.
                asked = _asked_to_leave(strategy, held, bar, symbol, cash, window, log)
                if asked is not None:
                    cash = _close(
                        cash, held, bar.close, bar.open_time, asked, limits.fee_rate, result
                    )
                    held = None

        # 3. Mark the account to market.
        equity = cash + (held.unrealized(bar.close) if held else ZERO)
        peak_equity = max(peak_equity, equity)
        today = trading_day(now)
        if day != today:
            day = today
            day_start_equity = equity
        result.equity_curve.append(EquityPoint(at=now, equity=equity, in_position=held is not None))

        if halted or index == len(bars) - 1:
            # Nothing is opened on the final bar: there is no bar left to fill it.
            continue

        # 4. Ask the strategy.
        market = MarketView(
            symbol=symbol,
            price=bar.close,
            now=now,
            equity=equity,
            position=held.position if held else None,
            recent_prices=tuple(window) + (bar.close,),
        )
        window.append(bar.close)
        try:
            signal = strategy.get_signal(market)
        except Exception as exc:  # a broken strategy must not kill the run
            strategy.on_error(exc)
            result.warnings.append(f"the strategy raised at {now.isoformat()}: {exc!r}")
            continue
        if signal is None:
            continue
        result.signals += 1

        signal, restamped = _stamp(signal, now, step, restamped, result)

        # 5. The same gate the live engine calls, with the same limits.
        state = PortfolioState(
            equity=equity,
            peak_equity=peak_equity,
            realized_pnl_today=equity - day_start_equity,
            positions={held.order.symbol: held.position} if held else {},
            halted=False,
            halt_reason="",
        )
        decision = risk_gate(
            signal=signal,
            state=state,
            limits=limits,
            spec=spec,
            mark_price=bar.close,
            now=now,
        )
        if isinstance(decision, Reject):
            result.rejects[decision.code.value] += 1
            strategy.on_reject(decision)
            if halt_on_limits and decision.code in HALTING:
                halted = True
                halt_reason = str(decision)
                result.halted_at = now
                result.halt_reason = halt_reason
                log.warning("backtest halted at %s: %s", now.isoformat(), halt_reason)
            continue

        pending = decision

    # The run ends holding nothing: an open position is closed at the last
    # close so the final equity is money, not a position that might yet move.
    if held is not None:
        last = bars[-1]
        cash = _close(
            cash,
            held,
            last.close,
            last.open_time,
            "still open at the end",
            limits.fee_rate,
            result,
        )
        if result.equity_curve:
            result.equity_curve[-1] = EquityPoint(
                at=result.equity_curve[-1].at, equity=cash, in_position=False
            )

    strategy.cleanup()
    return result


def _close(
    cash: Decimal,
    held: _Open,
    exit_price: Decimal,
    exit_at: datetime,
    why: str,
    fee_rate: Decimal,
    result: BacktestResult,
) -> Decimal:
    """Book the round trip and hand back the new cash balance."""
    exit_fee = exit_price * held.order.qty * fee_rate
    gross = (exit_price - held.entry_price) * held.order.qty * held.order.side.sign
    result.trades.append(
        Trade(
            symbol=held.order.symbol,
            side=held.order.side,
            qty=held.order.qty,
            entry_at=held.entry_at,
            entry_price=held.entry_price,
            exit_at=exit_at,
            exit_price=exit_price,
            stop_price=held.order.stop_price,
            risk_amount=held.order.risk_amount,
            fees=held.entry_fee + exit_fee,
            exit_reason=why,
            signal_reason=held.order.reason,
        )
    )
    return cash + gross - exit_fee


def _stamp(
    signal: Signal, now: datetime, step: timedelta, restamped: bool, result: BacktestResult
) -> tuple[Signal, bool]:
    """Keep a signal on market time, not wall-clock time.

    A strategy that builds its signal without passing ``created_at`` gets the
    real clock, which in a replay of last year is a date in the future.  The
    expiry check would then be meaningless, and the client order id - which
    hashes the timestamp - would change on every run, so the same backtest
    would not reproduce.
    """
    if abs(signal.created_at - now) <= step:
        return signal, restamped
    if not restamped:
        result.warnings.append(
            "the strategy stamped its signals with the wall clock instead of "
            "market.now; the backtest restamped them so expiry and order ids "
            "stay reproducible"
        )
    return replace(signal, created_at=now), True


__all__ = ["BacktestError", "exit_on_bar", "fill_price", "run_backtest"]
