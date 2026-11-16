"""What a strategy is, and what it is not allowed to do.

A strategy answers one question: *given what the market looks like right now,
do I want to open a position, and where is the stop?*  It does not size the
position, it does not check risk, and it never calls the exchange.  Those are
the risk gate's and the broker's jobs respectively, which is what keeps a
strategy small enough to reason about and easy to test.

Writing one:

```python
class MyStrategy(Strategy):
    def get_signal(self, market):
        if not self.want_to_buy(market):
            return None
        return Signal.build(
            symbol=market.symbol,
            side="buy",
            ref_price=market.price,
            stop_price=market.price * D("0.99"),
            reason="my edge fired",
        )
```
"""

from __future__ import annotations

import logging
from abc import ABC, abstractmethod
from collections.abc import Sequence
from dataclasses import dataclass, field
from datetime import datetime
from decimal import Decimal
from typing import Any

from ..types import Position, Signal


@dataclass(frozen=True)
class MarketView:
    """Everything a strategy may look at on one iteration.

    ``recent_prices`` is a rolling window of the marks this process has
    observed, oldest first - not exchange candles.  It starts empty on every
    restart, so an indicator that needs 50 points needs 50 iterations before
    it speaks.  Swapping this for real candle history is the obvious next
    upgrade; see docs/architecture.md.
    """

    symbol: str
    price: Decimal
    now: datetime
    equity: Decimal
    position: Position | None = None
    recent_prices: Sequence[Decimal] = field(default_factory=tuple)

    @property
    def in_position(self) -> bool:
        return self.position is not None


class Strategy(ABC):
    """Base class for all strategies."""

    #: Shown by ``tradebot --list-strategies``.
    description = ""

    def __init__(self, config: Any = None, logger: logging.Logger | None = None) -> None:
        self.config = config
        self.log = logger or logging.getLogger(self.__class__.__name__)
        self.name = self.__class__.__name__

    @abstractmethod
    def get_signal(self, market: MarketView) -> Signal | None:
        """Return a :class:`~tradebot.types.Signal`, or ``None`` to do nothing.

        Must be free of I/O and of surprises: called once per loop iteration.
        """

    def get_exit(self, market: MarketView) -> str | None:
        """Ask to close the open position, returning why - or ``None`` to hold.

        Called once per iteration, before :meth:`get_signal`, and only while a
        position is open.  The reason is written to the audit trail and shown
        on the dashboard, so write it for a person: ``"target reached"`` beats
        ``"cond_3"``.

        Two rules that are easy to get wrong:

        * **Closing is not sized and not gated.**  The risk gate approves
          *taking* risk; getting out reduces it.  A halted bot must still be
          able to flatten, or a halt would trap the position it was meant to
          protect.
        * **This template does not enforce your stop for you.**  The
          backtester models stops and targets because it owns the bar; the
          live loop only knows the price it just read, so if the stop matters
          it has to be checked here.  :meth:`stop_hit` is the usual way.
        """
        return None

    @staticmethod
    def stop_hit(market: MarketView, stop_price: Decimal) -> bool:
        """Has the market reached the stop of the open position?"""
        held = market.position
        if held is None:
            return False
        if held.side.value == "buy":
            return market.price <= stop_price
        return market.price >= stop_price

    # --- optional hooks ---------------------------------------------------

    def initialize(self) -> None:
        """Called once before the loop starts."""
        self.log.info("%s ready", self.name)

    def on_order_placed(self, receipt: Any) -> None:
        """Called after an order reached the exchange."""
        self.log.info("%s: order %s accepted", self.name, receipt.broker_order_id)

    def on_reject(self, reject: Any) -> None:
        """Called when the risk gate refused this strategy's signal."""
        self.log.info("%s: signal refused (%s)", self.name, reject.code.value)

    def on_error(self, error: Exception) -> None:
        """Called when the iteration raised."""
        self.log.error("%s: %s", self.name, error)

    def cleanup(self) -> None:
        """Called once on shutdown."""
        self.log.info("%s stopped", self.name)

    def __repr__(self) -> str:
        return f"{self.name}()"

    # --- for the board ---------------------------------------------------

    def chart(self, market: MarketView) -> dict[str, Decimal] | None:
        """The levels this strategy is watching right now, or ``None``.

        Only for drawing: the engine writes the answer into the audit row as
        ``view`` and the board draws it under the strategy card, so a student
        sees the lines the strategy compared the price with rather than a
        guess at them.  Nothing the bot does may depend on it, which is why it
        is asked after the tick has been decided and why an exception here is
        swallowed.  Keep it pure and cheap; do not touch strategy state.

        Price levels are drawn as lines.  Well-known keys get their own style
        on the board: ``average``, ``buy_below``, ``sell_above``, ``stop``.
        """
        return None
