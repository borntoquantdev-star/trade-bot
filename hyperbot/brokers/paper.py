"""An offline broker.

``PaperBroker`` needs no API key, no network and no SDK.  It is what makes
``hyperbot --dry-run`` genuinely dry: the old bot still authenticated against
the live exchange before deciding not to trade, so you could not test a
strategy without real credentials.

Fills are optimistic and immediate at the order's limit price.  That is a
deliberate simplification - paper results are for checking that the *plumbing*
works, never for judging whether a strategy makes money.  Use a backtest with
real candles for that.
"""

from __future__ import annotations

import contextlib
import logging
from collections.abc import Callable, Iterable, Iterator, Sequence
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from decimal import Decimal

from ..money import ZERO, D, Number
from ..types import Candle, InstrumentSpec, Position, Side, SizedOrder
from .base import BrokerError, OrderReceipt, require_positive

PriceSource = Callable[[str], Decimal]


def fixed_price(price: Number) -> PriceSource:
    """A price source that always answers the same number."""
    value = D(price)
    return lambda _symbol: value


def price_series(prices: Iterable[Number], *, loop: bool = True) -> PriceSource:
    """Walk through *prices*, one per call, optionally looping at the end."""
    values = [D(p) for p in prices]
    if not values:
        raise ValueError("price_series needs at least one price")

    def generator() -> Iterator[Decimal]:
        while True:
            yield from values
            if not loop:
                return

    stream = generator()
    last = values[-1]

    def source(_symbol: str) -> Decimal:
        nonlocal last
        # A non-looping series holds its final price once exhausted.
        with contextlib.suppress(StopIteration):
            last = next(stream)
        return last

    return source


@dataclass
class _Fill:
    symbol: str
    side: Side
    qty: Decimal
    price: Decimal


class PaperBroker:
    """Simulated exchange: deterministic, offline, and honest about it."""

    name = "paper"

    def __init__(
        self,
        price_source: PriceSource,
        instrument: InstrumentSpec,
        equity: Number = 10_000,
        logger: logging.Logger | None = None,
        history: Sequence[Number] | None = None,
    ) -> None:
        self._price = price_source
        self._instrument = instrument
        self._history = [D(p) for p in history] if history else None
        self._equity = D(equity)
        self._start_equity = self._equity
        self._positions: dict[str, Position] = {}
        self._realized_pnl = ZERO
        self.fills: list[_Fill] = []
        self.log = logger or logging.getLogger(__name__)

    # --- Broker protocol -------------------------------------------------

    def connect(self) -> None:
        self.log.info(
            "paper broker ready: equity %s, %s (tick %s, lot %s, min %s)",
            self._equity,
            self._instrument.symbol,
            self._instrument.tick_size,
            self._instrument.lot_step,
            self._instrument.min_notional,
        )

    def fetch_mark_price(self, symbol: str) -> Decimal:
        return require_positive(D(self._price(symbol)), f"paper price for {symbol}")

    def fetch_instrument(self, symbol: str) -> InstrumentSpec:
        if symbol != self._instrument.symbol:
            raise BrokerError(
                f"paper broker was set up for {self._instrument.symbol}, not {symbol}"
            )
        return self._instrument

    def fetch_equity(self) -> Decimal:
        return self._equity

    def fetch_positions(self, symbol: str | None = None) -> dict[str, Position]:
        if symbol is None:
            return dict(self._positions)
        held = self._positions.get(symbol)
        return {symbol: held} if held else {}

    def fetch_candles(self, symbol: str, interval: str, limit: int) -> list[Candle]:
        """Bars built from *history*, or flat bars at the current price.

        Synthetic either way: it exists so the warm-up path is exercised
        offline. Flat bars mean a crossover strategy will not fire in a paper
        run, which is honest - there is no real price movement to cross.
        """
        if limit <= 0:
            return []
        prices = self._history or [self.fetch_mark_price(symbol)] * limit
        prices = prices[-limit:]
        step = timedelta(minutes=1)
        start = datetime.now(timezone.utc) - step * len(prices)
        return [
            Candle(
                symbol=symbol,
                interval=interval,
                open_time=start + step * i,
                open=price,
                high=price,
                low=price,
                close=price,
                volume=ZERO,
            )
            for i, price in enumerate(prices)
        ]

    def place_order(self, order: SizedOrder) -> OrderReceipt:
        if order.symbol in self._positions:
            raise BrokerError(f"paper broker already holds {order.symbol}")
        self._positions[order.symbol] = Position(
            symbol=order.symbol,
            side=order.side,
            qty=order.qty,
            entry_price=order.limit_price,
        )
        self.fills.append(_Fill(order.symbol, order.side, order.qty, order.limit_price))
        self.log.info(
            "[PAPER] filled %s %s %s @ %s (id %s)",
            order.side.value,
            order.qty,
            order.symbol,
            order.limit_price,
            order.client_order_id,
        )
        return OrderReceipt(
            broker_order_id=f"paper-{order.client_order_id}",
            client_order_id=order.client_order_id,
            symbol=order.symbol,
            side=order.side.value,
            qty=order.qty,
            limit_price=order.limit_price,
            simulated=True,
            raw={"paper": True},
        )

    def close_position(self, symbol: str) -> OrderReceipt | None:
        held = self._positions.pop(symbol, None)
        if held is None:
            return None
        exit_price = self.fetch_mark_price(symbol)
        pnl = (exit_price - held.entry_price) * held.qty * held.side.sign
        self._realized_pnl += pnl
        self._equity += pnl
        self.fills.append(_Fill(symbol, held.side.opposite, held.qty, exit_price))
        self.log.info(
            "[PAPER] closed %s at %s, realised %s (equity now %s)",
            symbol,
            exit_price,
            pnl,
            self._equity,
        )
        return OrderReceipt(
            broker_order_id=f"paper-close-{symbol}",
            client_order_id=f"close-{symbol}",
            symbol=symbol,
            side=held.side.opposite.value,
            qty=held.qty,
            limit_price=exit_price,
            simulated=True,
        )

    # --- extras the engine uses for reporting ----------------------------

    @property
    def realized_pnl(self) -> Decimal:
        return self._realized_pnl

    @property
    def peak_equity(self) -> Decimal:
        return max(self._start_equity, self._equity)
