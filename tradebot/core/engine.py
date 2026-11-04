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
from ..risk import Decision, explain, risk_gate
from ..strategies.base import MarketView, Strategy
from ..types import PortfolioState, Position, Reject, RejectCode, Signal, SizedOrder
from .config import BotConfig

Clock = Callable[[], datetime]
Sleeper = Callable[[float], None]


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
    sent_order_ids: list[str] = field(default_factory=list)

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
            sent_order_ids=list(raw.get("sent_order_ids", []))[-500:],
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
            "sent_order_ids": self.sent_order_ids[-500:],
        }
        path.write_text(json.dumps(payload, indent=2), encoding="utf-8")


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
    error: str | None = None

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
        if self.state.halted:
            self.log.error(
                "state file says trading is HALTED: %s. Clear it with "
                "`tradebot resume` once you know why.",
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
        try:
            result.receipt = self.broker.place_order(decision)
        except BrokerError as exc:
            result.error = f"order rejected by broker: {exc}"
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

    # --- helpers ---------------------------------------------------------

    def halt(self, reason: str) -> None:
        """Stop trading until a human clears the state file."""
        if not self.state.halted:
            self.log.critical("HALTING: %s", reason)
        self.state.halted = True
        self.state.halt_reason = reason
        self.state.save(self.state_path)

    def resume(self) -> None:
        """Clear a halt (used by ``tradebot resume``)."""
        self.state.halted = False
        self.state.halt_reason = ""
        self.state.save(self.state_path)

    def _roll_day(self, now: datetime, equity: Decimal) -> None:
        """Start a new daily loss budget at each UTC midnight."""
        today = now.astimezone(timezone.utc).date().isoformat()
        if self.state.day != today:
            self.state.day = today
            self.state.day_start_equity = equity
            self.log.info("new trading day %s, starting equity %s", today, equity)
        if self.state.peak_equity <= ZERO:
            self.state.peak_equity = equity

    def _count_error(self, message: str) -> None:
        self._consecutive_errors += 1
        self.log.error("%s (%s in a row)", message, self._consecutive_errors)
        if self._consecutive_errors >= self.max_consecutive_errors:
            self.halt(f"{self._consecutive_errors} consecutive failures; last was {message}")

    def _record(self, result: TickResult) -> TickResult:
        """Append one line to the audit trail."""
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
        if result.receipt is not None:
            row["receipt"] = {
                "broker_order_id": result.receipt.broker_order_id,
                "simulated": result.receipt.simulated,
            }
        if result.error:
            row["error"] = result.error
        try:
            self.audit_path.parent.mkdir(parents=True, exist_ok=True)
            with self.audit_path.open("a", encoding="utf-8") as handle:
                handle.write(json.dumps(row, ensure_ascii=False) + "\n")
        except OSError as exc:  # never let logging break trading
            self.log.warning("could not write the audit trail: %s", exc)
        return result


__all__ = ["Clock", "Engine", "EngineState", "Sleeper", "TickResult", "utc_now"]
