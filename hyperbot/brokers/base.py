"""The broker boundary.

A broker is the **only** part of this package allowed to touch the network.
Strategies and the risk gate deal in :mod:`hyperbot.types` values; brokers
translate those to and from whatever an exchange happens to speak.

Two implementations ship with the bot:

* :class:`~hyperbot.brokers.paper.PaperBroker` - offline, deterministic, needs
  no credentials.  Powers ``--dry-run`` and the test suite.
* :class:`~hyperbot.brokers.hyperliquid.HyperliquidBroker` - the real exchange.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, field
from decimal import Decimal
from typing import Any, Protocol, runtime_checkable

from ..types import Candle, InstrumentSpec, Position, SizedOrder


class BrokerError(RuntimeError):
    """The exchange refused, or answered in a way we refuse to guess about."""


@dataclass(frozen=True)
class OrderReceipt:
    """Proof that an order reached (or did not reach) the exchange.

    ``broker_order_id`` is never invented.  If the exchange did not give us an
    id, placing the order is treated as a failure rather than logged as a
    success with ``id=None`` - the bot must not believe it is in a position it
    cannot point to.
    """

    broker_order_id: str
    client_order_id: str
    symbol: str
    side: str
    qty: Decimal
    limit_price: Decimal
    simulated: bool = False
    raw: Mapping[str, Any] = field(default_factory=dict)


@runtime_checkable
class Broker(Protocol):
    """What the engine needs from an exchange."""

    name: str

    def connect(self) -> None:
        """Open the session.  Called once before the loop starts."""

    def fetch_mark_price(self, symbol: str) -> Decimal:
        """Current price.  Must raise :class:`BrokerError` rather than return 0."""

    def fetch_instrument(self, symbol: str) -> InstrumentSpec:
        """Tick size, lot step and minimum order value for *symbol*."""

    def fetch_equity(self) -> Decimal:
        """Account equity in quote currency."""

    def fetch_positions(self, symbol: str | None = None) -> dict[str, Position]:
        """Open positions, keyed by symbol."""

    def fetch_candles(self, symbol: str, interval: str, limit: int) -> list[Candle]:
        """The most recent *limit* closed bars, oldest first.

        The engine calls this once at start-up to fill the strategy's price
        window, so an indicator needing 50 points does not have to wait 50
        iterations.  A broker with no candle feed may return an empty list.
        """

    def place_order(self, order: SizedOrder) -> OrderReceipt:
        """Send an approved order.  Raise :class:`BrokerError` on failure."""

    def close_position(self, symbol: str) -> OrderReceipt | None:
        """Flatten *symbol*, if anything is open."""


def require_positive(value: Decimal, what: str) -> Decimal:
    """Guard against the exchange handing back zero or nonsense prices."""
    if value <= 0:
        raise BrokerError(f"{what} came back as {value}, which cannot be traded on")
    return value
