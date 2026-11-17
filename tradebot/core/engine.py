"""The trading loop.

One iteration, in order:

1. read the market and the account from the broker,
2. ask the strategy for a signal,
3. put the signal through :func:`~tradebot.risk.risk_gate`,
4. send the order the gate approved - and nothing else.

The engine owns no trading rules.  It cannot decide to skip a check, resize an
order or trade a different symbol: whatever the gate hands back is what gets
sent, and a :class:`~tradebot.types.Reject` is simply logged.  That separation
is the whole point of the package.

State that must outlive the process - the day's starting equity, the peak
equity behind the drawdown stop, and the ids of orders already sent - is kept
in a small JSON file.  Without it a restart would reset the daily loss limit,
which is exactly when you least want it reset.
"""

from __future__ import annotations

import json
import logging
import time
from collections import deque
from collections.abc import Callable
from dataclasses import dataclass, field
from datetime import datetime, timezone
from decimal import Decimal
from pathlib import Path
from typing import Any

from ..brokers.base import Broker, BrokerError, OrderReceipt
from ..money import ZERO, D
from ..risk import Decision, explain, risk_gate, trading_day
from ..strategies.base import MarketView, Strategy
from ..trace import Trace
from ..types import PortfolioState, Position, Reject, RejectCode, Signal, SizedOrder
from .config import BotConfig

Clock = Callable[[], datetime]
Sleeper = Callable[[float], None]

#: When the trace file is rolled over to ``trace.jsonl.1``.  A trace is a few
#: kilobytes a tick, so a bot left running all day would otherwise grow one
#: file without limit; the board only ever looks at recent ticks anyway.
TRACE_ROTATE_BYTES = 64 * 1024 * 1024


def utc_now() -> datetime:
    return datetime.now(timezone.utc)


@dataclass
class EngineState:
    """The little that has to survive a restart."""

    day: str = ""
    day_start_equity: Decimal = ZERO
    peak_equity: Decimal = ZERO
    halted: bool = False
    halt_reason: str = ""
    #: The trading day the halt began.  A daily-loss halt lifts itself once
    #: the day has moved on; without the day it began there is no telling
    #: whether that has happened, so a halt with none stays put.
    halt_day: str = ""
    sent_order_ids: list[str] = field(default_factory=list)
    #: Which modules this bot is running, written at start-up.
    #:
    #: Not state that has to survive a restart - it is rewritten every time
    #: the engine starts.  It lives here because this file is the one a
    #: watching dashboard already reads, and because a dashboard that has to
    #: guess the strategy from a config file will sooner or later guess the
    #: wrong one: the config it can see is not always the config the bot was
    #: started with.
    strategy_module: str = ""
    broker_module: str = ""

    @classmethod
    def load(cls, path: Path | None) -> EngineState:
        if path is None or not path.exists():
            return cls()
        try:
            raw = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            return cls()
        return cls(
            day=str(raw.get("day", "")),
            day_start_equity=D(raw.get("day_start_equity", 0)),
            peak_equity=D(raw.get("peak_equity", 0)),
            halted=bool(raw.get("halted", False)),
            halt_reason=str(raw.get("halt_reason", "")),
            halt_day=str(raw.get("halt_day", "")),
            sent_order_ids=list(raw.get("sent_order_ids", []))[-500:],
            strategy_module=str(raw.get("strategy_module", "")),
            broker_module=str(raw.get("broker_module", "")),
        )

    def save(self, path: Path | None) -> None:
        if path is None:
            return
        path.parent.mkdir(parents=True, exist_ok=True)
        payload = {
            "day": self.day,
            "day_start_equity": format(self.day_start_equity, "f"),
            "peak_equity": format(self.peak_equity, "f"),
            "halted": self.halted,
            "halt_reason": self.halt_reason,
            "halt_day": self.halt_day,
            "sent_order_ids": self.sent_order_ids[-500:],
            "strategy_module": self.strategy_module,
            "broker_module": self.broker_module,
        }
        path.write_text(json.dumps(payload, indent=2), encoding="utf-8")


@dataclass(frozen=True)
class Sending:
    """When an order left for the exchange, and how long the answer took.

    Wall-clock time, not the engine's clock: this is a measurement of the
    trip to the exchange, which a test's frozen clock would say took no time.
    """

    at: datetime
    ms: float


def _since(began: float) -> float:
    return round((time.perf_counter() - began) * 1000, 1)


@dataclass
class TickResult:
    """What happened in one iteration, for logs, tests and the audit trail."""

    at: datetime
    price: Decimal
    equity: Decimal
    symbol: str = ""
    position: Position | None = None
    signal: Signal | None = None
    decision: Decision | None = None
    receipt: OrderReceipt | None = None
    #: Why the strategy asked to close, and what the broker said.
    exit_reason: str | None = None
    exit_receipt: OrderReceipt | None = None
    error: str | None = None
    #: The trip to the exchange for the order, and for the exit.
    sent: Sending | None = None
    exit_sent: Sending | None = None

    @property
    def order(self) -> SizedOrder | None:
        return self.decision if isinstance(self.decision, SizedOrder) else None

    @property
    def reject(self) -> Reject | None:
        return self.decision if isinstance(self.decision, Reject) else None


class Engine:
    """Wires a broker, a strategy and the risk gate into a loop."""

    def __init__(
        self,
        config: BotConfig,
        broker: Broker,
        strategy: Strategy,
        logger: logging.Logger | None = None,
        state_path: Path | None = None,
        audit_path: Path | None = None,
        clock: Clock = utc_now,
        history: int = 200,
        max_consecutive_errors: int = 5,
        trace: bool = False,
        trace_path: Path | None = None,
    ) -> None:
        self.config = config
        self.broker = broker
        self.strategy = strategy
        self.log = logger or logging.getLogger("tradebot.engine")
        self.state_path = Path(state_path) if state_path else None
        self.audit_path = Path(audit_path) if audit_path else None
        self.clock = clock
        self.state = EngineState.load(self.state_path)
        # The window must hold everything the warm-up asks for, or priming it
        # would silently throw away the oldest candles.
        self.prices: deque[Decimal] = deque(maxlen=max(history, config.trading.warmup_candles))
        self.max_consecutive_errors = max_consecutive_errors
        self._consecutive_errors = 0
        self._instrument = config.instrument
        # Which lines each tick ran, for the board.  Off unless asked for;
        # the CLI asks for it by default.  With a trace file each trace goes
        # there and the audit row only says it has one, which keeps the audit
        # trail a file a person can read line by line - a trace is fifteen
        # times the size of the rest of a row.  Without one the trace rides
        # inside the row, as it did before the file existed.
        self.trace_enabled = trace
        self.trace_path = Path(trace_path) if trace_path else None
        self._trace = Trace(enabled=False)
        # The market this tick was decided on, kept only so `_record` can
        # ask the strategy what it was looking at (see `_view`).
        self._market: MarketView | None = None
        # What this run has done so far, for the run log.  Counted here
        # rather than derived from the audit trail later, because the trail
        # is trimmed to its tail and a run from last week would come back
        # with no numbers at all.
        self.ticks = 0
        self.orders = 0
        self.exits = 0
        self.rejects = 0
        self.first_equity: Decimal | None = None
        self.last_equity: Decimal | None = None

    # --- setup -----------------------------------------------------------

    def start(self) -> None:
        """Connect, read the instrument spec, and announce the settings."""
        self.broker.connect()
        try:
            self._instrument = self.broker.fetch_instrument(self.config.trading.symbol)
        except BrokerError as exc:
            self.log.warning("using instrument values from the config file (%s)", exc)
            self._instrument = self.config.instrument
        self._warm_up()
        self.strategy.initialize()
        # Write down what is running before the first tick, so anything
        # watching knows whose code to show without being told.
        self.state.strategy_module = type(self.strategy).__module__
        self.state.broker_module = type(self.broker).__module__
        self._lift_daily_halt(trading_day(self.clock()))
        self.state.save(self.state_path)
        if self.state.halted:
            self.log.error(
                "state file says trading is HALTED: %s. Once you know why, clear it "
                "with `tradebot --config <the same file> --resume` (add --live for "
                "an exchange account).",
                self.state.halt_reason,
            )

    def _warm_up(self) -> None:
        """Fill the strategy's price window from real candles before trading.

        Without this, a strategy needing 30 observations waits 30 iterations -
        half an hour at the default loop interval - and starts from nothing
        again after every restart. A broker with no candle feed simply leaves
        the window empty, which is the old behaviour.
        """
        wanted = self.config.trading.warmup_candles
        if wanted <= 0:
            return
        fetch = getattr(self.broker, "fetch_candles", None)
        if not callable(fetch):
            self.log.info("this broker has no candle feed; starting with an empty window")
            return
        try:
            candles = fetch(self.config.trading.symbol, self.config.trading.timeframe, wanted)
        except BrokerError as exc:
            self.log.warning("could not load warm-up candles (%s); starting empty", exc)
            return
        self.prices.extend(candle.close for candle in candles)
        if candles:
            self.log.info(
                "warmed up with %s %s candles, %s to %s",
                len(candles),
                self.config.trading.timeframe,
                candles[0].open_time.isoformat(timespec="minutes"),
                candles[-1].open_time.isoformat(timespec="minutes"),
            )
        else:
            self.log.warning("the candle feed returned nothing; starting with an empty window")

    # --- one iteration ---------------------------------------------------

    def tick(self) -> TickResult:
        """Run one full decision cycle.  Never raises for market conditions."""
        # Recording which lines of this package ran, so the board can show
        # the path a tick really took instead of guessing it from the row it
        # left behind.  Off unless somebody asked for it.
        self._market = None
        with Trace(enabled=self.trace_enabled) as self._trace:
            now = self.clock()
            symbol = self.config.trading.symbol

            try:
                price = self.broker.fetch_mark_price(symbol)
                equity = self.broker.fetch_equity()
                positions = dict(self.broker.fetch_positions())
            except BrokerError as exc:
                return self._record(
                    TickResult(
                        at=now,
                        price=ZERO,
                        equity=ZERO,
                        symbol=symbol,
                        error=f"broker read failed: {exc}",
                    )
                )

            self.prices.append(price)
            self._roll_day(now, equity)
            self.state.peak_equity = max(self.state.peak_equity, equity)

            state = PortfolioState(
                equity=equity,
                peak_equity=self.state.peak_equity,
                realized_pnl_today=equity - self.state.day_start_equity,
                positions=positions,
                halted=self.state.halted,
                halt_reason=self.state.halt_reason,
            )

            market = MarketView(
                symbol=symbol,
                price=price,
                now=now,
                equity=equity,
                position=positions.get(symbol),
                recent_prices=tuple(self.prices),
            )
            self._market = market

            # Getting out comes before getting in, and never passes the risk
            # gate: the gate approves *taking* risk, and closing reduces it.  A
            # halted bot can still flatten, which is the point of a halt - stop
            # opening, not trap the position the halt was meant to protect.
            if market.position is not None:
                try:
                    leaving = self.strategy.get_exit(market)
                except Exception as exc:  # a broken strategy must not trap a position
                    self.strategy.on_error(exc)
                    leaving = None
                    self.log.error("strategy.get_exit raised: %r", exc)
                if leaving:
                    return self._close(market, leaving)

            try:
                signal = self.strategy.get_signal(market)
            except Exception as exc:  # a broken strategy must not take the bot down
                self.strategy.on_error(exc)
                return self._record(
                    TickResult(
                        at=now,
                        price=price,
                        equity=equity,
                        symbol=symbol,
                        position=positions.get(symbol),
                        error=f"strategy raised: {exc!r}",
                    )
                )

            if signal is None:
                return self._record(
                    TickResult(
                        at=now,
                        price=price,
                        equity=equity,
                        symbol=symbol,
                        position=positions.get(symbol),
                    )
                )

            decision = risk_gate(
                signal=signal,
                state=state,
                limits=self.config.risk,
                spec=self._instrument,
                mark_price=price,
                now=now,
            )
            result = TickResult(
                at=now,
                price=price,
                equity=equity,
                symbol=symbol,
                position=positions.get(symbol),
                signal=signal,
                decision=decision,
            )

            if isinstance(decision, Reject):
                level = (
                    logging.ERROR
                    if decision.code
                    in (RejectCode.HALTED, RejectCode.MAX_DRAWDOWN, RejectCode.DAILY_LOSS_LIMIT)
                    else logging.INFO
                )
                self.log.log(level, explain(decision))
                self.strategy.on_reject(decision)
                if decision.code in (RejectCode.MAX_DRAWDOWN, RejectCode.DAILY_LOSS_LIMIT):
                    self.halt(f"{decision.code.value}: {decision.detail}")
                return self._record(result)

            if decision.client_order_id in self.state.sent_order_ids:
                self.log.warning(
                    "order %s was already sent; not sending it twice", decision.client_order_id
                )
                return self._record(result)

            self.log.info(explain(decision))
            sent_at, began = datetime.now(timezone.utc), time.perf_counter()
            try:
                result.receipt = self.broker.place_order(decision)
            except BrokerError as exc:
                result.error = f"order rejected by broker: {exc}"
            result.sent = Sending(sent_at, _since(began))
            if result.error is not None:
                self.log.error(result.error)
                return self._record(result)

            # Write the id to disk the moment the order is accepted, not at the end
            # of the loop: a crash in between is exactly the case idempotency
            # exists for.  A broker *rejection* deliberately does not record the
            # id, so a genuine retry stays possible; the ambiguous case (we timed
            # out but the order did reach the book) is covered by the exchange,
            # which holds the same client_order_id and will refuse the duplicate.
            self.state.sent_order_ids.append(decision.client_order_id)
            self.state.save(self.state_path)
            self.strategy.on_order_placed(result.receipt)
            return self._record(result)

    # --- loop ------------------------------------------------------------

    def run(
        self,
        max_iterations: int | None = None,
        sleeper: Sleeper = time.sleep,
    ) -> int:
        """Loop until interrupted (or *max_iterations* is reached, for tests).

        Returns a process exit code: 0 for a clean stop, 1 if the bot halted
        itself.
        """
        interval = self.config.trading.loop_interval
        iteration = 0
        try:
            while max_iterations is None or iteration < max_iterations:
                iteration += 1
                self.log.debug("iteration %s", iteration)
                try:
                    result = self.tick()
                except Exception as exc:  # last-resort guard
                    self._count_error(f"unexpected: {exc!r}")
                else:
                    if result.error:
                        self._count_error(result.error)
                    else:
                        self._consecutive_errors = 0

                self.state.save(self.state_path)
                if self.state.halted:
                    self.log.error("halted - stopping the loop")
                    return 1
                if max_iterations is None or iteration < max_iterations:
                    sleeper(interval)
            return 0
        except KeyboardInterrupt:
            self.log.info("stopped by user")
            return 0
        finally:
            self.strategy.cleanup()
            self.state.save(self.state_path)

    def flatten(self, reason: str) -> TickResult | None:
        """Close the configured symbol's position, if there is one.

        For the end of a run that was told to stop after N iterations: a
        bot that stops while holding leaves the position for nobody to look
        after, and on a live account that is the one thing a demo must never
        do.  It goes through the same ``_close`` as a strategy's own exit, so
        the audit trail shows the close and why, and it never passes the risk
        gate - closing reduces risk, which is not what the gate approves.

        Returns ``None`` when there was nothing to close or the account
        could not be read, which is said in the log either way.
        """
        symbol = self.config.trading.symbol
        self._market = None
        with Trace(enabled=self.trace_enabled) as self._trace:
            try:
                positions = dict(self.broker.fetch_positions())
                position = positions.get(symbol)
                if position is None:
                    self._trace.stop()
                    self.log.info("nothing open on %s at the end of the run", symbol)
                    return None
                price = self.broker.fetch_mark_price(symbol)
                equity = self.broker.fetch_equity()
            except BrokerError as exc:
                self._trace.stop()
                self.log.error("could not read the account to close at the end: %s", exc)
                return None
            market = MarketView(
                symbol=symbol,
                price=price,
                now=self.clock(),
                equity=equity,
                position=position,
                recent_prices=tuple(self.prices),
            )
            return self._close(market, reason)

    # --- helpers ---------------------------------------------------------

    def halt(self, reason: str) -> None:
        """Stop trading until a human clears the state file."""
        if not self.state.halted:
            self.log.critical("HALTING: %s", reason)
        self.state.halted = True
        self.state.halt_reason = reason
        self.state.halt_day = trading_day(self.clock())
        self.state.save(self.state_path)

    def resume(self) -> None:
        """Clear a halt (used by ``tradebot --resume``)."""
        self.state.halted = False
        self.state.halt_reason = ""
        self.state.halt_day = ""
        self.state.save(self.state_path)

    def _lift_daily_halt(self, today: str) -> None:
        """Lift a daily-loss halt once its day is over.

        The daily budget is a budget for one day: at midnight, Thai time, it
        starts again, so a halt that exists only because yesterday's budget
        ran out has nothing left to protect.  Every other halt - the drawdown
        kill switch, repeated failures, a human's - still waits for a human.
        """
        state = self.state
        if not (state.halted and state.halt_day and state.halt_day != today):
            return
        if not state.halt_reason.startswith(f"{RejectCode.DAILY_LOSS_LIMIT.value}:"):
            return
        self.log.warning(
            "lifting the daily-loss halt from %s: a new day (%s) has a new budget",
            state.halt_day,
            today,
        )
        self.resume()

    def _roll_day(self, now: datetime, equity: Decimal) -> None:
        """Start a new daily loss budget at each midnight, Thai time."""
        today = trading_day(now)
        if self.state.day != today:
            self.state.day = today
            self.state.day_start_equity = equity
            self.log.info("new trading day %s, starting equity %s", today, equity)
        self._lift_daily_halt(today)
        if self.state.peak_equity <= ZERO:
            self.state.peak_equity = equity

    def _view(self) -> dict[str, str] | None:
        """What the strategy was watching this tick, for the board to draw.

        Asked after the tracer has stopped, so it never shows up as a line
        the tick ran, and after the tick was decided, so it cannot change
        what the bot did.  A strategy whose ``chart`` raises loses its
        picture for that tick and nothing else.
        """
        if self._market is None:
            return None
        try:
            levels = self.strategy.chart(self._market)
        except Exception as exc:  # a drawing aid must never cost a tick
            self.log.debug("strategy.chart raised: %r", exc)
            return None
        if not levels:
            return None
        return {key: format(D(value), "f") for key, value in levels.items()}

    def _count_error(self, message: str) -> None:
        self._consecutive_errors += 1
        self.log.error("%s (%s in a row)", message, self._consecutive_errors)
        if self._consecutive_errors >= self.max_consecutive_errors:
            self.halt(f"{self._consecutive_errors} consecutive failures; last was {message}")

    def _close(self, market: MarketView, reason: str) -> TickResult:
        """Do what the strategy asked: flatten, and write down why."""
        result = TickResult(
            at=market.now,
            price=market.price,
            equity=market.equity,
            symbol=market.symbol,
            position=market.position,
            exit_reason=reason,
        )
        sent_at, began = datetime.now(timezone.utc), time.perf_counter()
        try:
            result.exit_receipt = self.broker.close_position(market.symbol)
        except BrokerError as exc:
            result.error = f"close rejected by broker: {exc}"
        result.exit_sent = Sending(sent_at, _since(began))
        if result.error is not None:
            self.log.error(result.error)
            return self._record(result)
        self.log.info(
            "closed %s: %s",
            market.symbol,
            reason,
        )
        return self._record(result)

    def _record(self, result: TickResult) -> TickResult:
        """Append one line to the audit trail."""
        # Stop before the row is built, or the picture of the tick would end
        # up including the code that writes the picture.
        self._trace.stop()
        self.ticks += 1
        if result.order is not None:
            self.orders += 1
        if result.reject is not None:
            self.rejects += 1
        if result.exit_reason is not None:
            self.exits += 1
        if result.equity > ZERO:
            if self.first_equity is None:
                self.first_equity = result.equity
            self.last_equity = result.equity
        if self.audit_path is None:
            return result
        row: dict[str, Any] = {
            "at": result.at.isoformat(),
            "symbol": result.symbol or self.config.trading.symbol,
            "price": format(result.price, "f"),
            "equity": format(result.equity, "f"),
        }
        if result.position is not None:
            row["position"] = {
                "side": result.position.side.value,
                "qty": format(result.position.qty, "f"),
                "entry_price": format(result.position.entry_price, "f"),
            }
        if result.signal is not None:
            row["signal"] = {
                "side": result.signal.side.value,
                "ref_price": format(result.signal.ref_price, "f"),
                "stop_price": format(result.signal.stop_price, "f"),
                "reason": result.signal.reason,
            }
        if result.reject is not None:
            row["reject"] = {"code": result.reject.code.value, "detail": result.reject.detail}
        if result.order is not None:
            row["order"] = {
                "client_order_id": result.order.client_order_id,
                "side": result.order.side.value,
                "qty": format(result.order.qty, "f"),
                "limit_price": format(result.order.limit_price, "f"),
                "risk_amount": format(result.order.risk_amount, "f"),
            }
            if result.sent is not None:
                row["order"]["sent_at"] = result.sent.at.isoformat()
                row["order"]["answered_ms"] = result.sent.ms
        if result.receipt is not None:
            row["receipt"] = {
                "broker_order_id": result.receipt.broker_order_id,
                "simulated": result.receipt.simulated,
            }
            if result.receipt.venue_client_order_id:
                row["receipt"]["venue_id"] = result.receipt.venue_client_order_id
        if result.exit_reason is not None:
            row["exit"] = {
                "reason": result.exit_reason,
                "broker_order_id": (
                    result.exit_receipt.broker_order_id if result.exit_receipt else ""
                ),
                "simulated": (result.exit_receipt.simulated if result.exit_receipt else False),
            }
            if result.exit_sent is not None:
                row["exit"]["sent_at"] = result.exit_sent.at.isoformat()
                row["exit"]["answered_ms"] = result.exit_sent.ms
            if result.exit_receipt is not None and result.exit_receipt.venue_client_order_id:
                row["exit"]["venue_id"] = result.exit_receipt.venue_client_order_id
        if result.error:
            row["error"] = result.error
        view = self._view()
        if view:
            row["view"] = view
        trace = self._trace.as_json()
        if trace is not None:
            if self.trace_path is None:
                row["trace"] = trace
            elif self._write_trace(row["at"], trace):
                # Written before the row, so anything that sees "traced" in
                # the trail can already find the trace it points at.
                row["traced"] = True
        try:
            self.audit_path.parent.mkdir(parents=True, exist_ok=True)
            with self.audit_path.open("a", encoding="utf-8") as handle:
                handle.write(json.dumps(row, ensure_ascii=False) + "\n")
        except OSError as exc:  # never let logging break trading
            self.log.warning("could not write the audit trail: %s", exc)
        return result

    def _write_trace(self, at: str, trace: dict[str, Any]) -> bool:
        """Append one tick's trace to the trace file, rolling it over when big.

        Returns False, and says so in the log, if it could not be written:
        the audit row then carries no mark, rather than a mark pointing at a
        trace that is not there.
        """
        path = self.trace_path
        if path is None:
            return False
        try:
            path.parent.mkdir(parents=True, exist_ok=True)
            if path.exists() and path.stat().st_size > TRACE_ROTATE_BYTES:
                path.replace(path.with_name(path.name + ".1"))
            with path.open("a", encoding="utf-8") as handle:
                handle.write(json.dumps({"at": at, "trace": trace}, ensure_ascii=False) + "\n")
        except OSError as exc:  # never let the picture break the trading
            self.log.warning("could not write the trace file: %s", exc)
            return False
        return True


__all__ = [
    "TRACE_ROTATE_BYTES",
    "Clock",
    "Engine",
    "EngineState",
    "Sleeper",
    "TickResult",
    "utc_now",
]
