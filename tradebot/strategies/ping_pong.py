"""A strategy whose only job is to make the machine visible.

It trades often, on purpose.  Watching one trade a day go past teaches you
nothing about whether your bot works; watching a signal become an order,
become a position and then be closed again - every minute or two, with the
reason printed each time - teaches you the whole shape of it in ten minutes.

How it decides, in one line: when the price stretches away from its own
recent average it bets on the stretch closing, and it gets out when the
price comes back, when the stop is hit, or when the trade has simply been
open long enough.

**It is not an edge.**  The stretch it trades is smaller than the round
trip in fees, so over many trades it loses roughly the fees and nothing
else surprising happens.  That is deliberate: a plumbing test that appeared
to make money would be the most expensive lesson in this repository.  Run it
on ``--dry-run`` or on a testnet, read the audit trail, then write something
of your own.
"""

from __future__ import annotations

import logging
from datetime import datetime, timezone
from decimal import Decimal
from typing import Any

from ..money import ONE, D
from ..types import Side, Signal
from .base import MarketView, Strategy


class PingPongStrategy(Strategy):
    """Fade a small stretch away from the recent average, then come back out."""

    description = "trades every minute or so to show the pipeline working - no edge"

    def __init__(
        self,
        config: Any = None,
        logger: logging.Logger | None = None,
        *,
        window: int = 12,
        edge_pct: Decimal | str | float = "0.03",
        stop_pct: Decimal | str | float = "0.4",
        max_hold_seconds: int = 180,
    ) -> None:
        super().__init__(config, logger)
        self.window = int(window)
        if self.window < 2:
            raise ValueError("window must cover at least two prices")
        self.edge_fraction = D(edge_pct) / 100
        if not 0 < self.edge_fraction < 1:
            raise ValueError("edge_pct must be between 0 and 100")
        self.stop_fraction = D(stop_pct) / 100
        if not 0 < self.stop_fraction < 1:
            raise ValueError("stop_pct must be between 0 and 100")
        if self.stop_fraction <= self.edge_fraction:
            # Otherwise the stop sits inside the move that opened the trade
            # and every position is stopped out on the tick it opened.
            raise ValueError("stop_pct must be wider than edge_pct")
        self.max_hold_seconds = int(max_hold_seconds)
        self._opened_at: datetime | None = None

    def initialize(self) -> None:
        super().initialize()
        self.log.warning(
            "PingPongStrategy trades often so you can watch the machine run. "
            "The move it trades is smaller than the fees, so it loses money by "
            "design. Use it to prove the plumbing, never to grow an account."
        )

    # --- the average it trades around -------------------------------------

    def average(self, market: MarketView) -> Decimal | None:
        """The mean of the last ``window`` prices, or ``None`` until there are."""
        prices = list(market.recent_prices)[-self.window :]
        if len(prices) < self.window:
            return None
        # Averaging through float would round money; keep it in Decimal.
        return sum(prices, D(0)) / D(len(prices))

    # --- getting in --------------------------------------------------------

    def get_signal(self, market: MarketView) -> Signal | None:
        if market.in_position:
            return None

        middle = self.average(market)
        if middle is None or middle <= 0:
            return None

        low = middle * (ONE - self.edge_fraction)
        high = middle * (ONE + self.edge_fraction)
        now = market.now or datetime.now(timezone.utc)

        if market.price <= low:
            side = "buy"
            stop = market.price * (ONE - self.stop_fraction)
        elif market.price >= high:
            side = "sell"
            stop = market.price * (ONE + self.stop_fraction)
        else:
            return None

        away = (market.price - middle) / middle * 100
        return Signal.build(
            symbol=market.symbol,
            side=side,
            ref_price=market.price,
            stop_price=stop,
            reason=f"price is {away:+.3f}% from its {self.window}-point average",
            created_at=now,
            valid_for_seconds=30,
        )

    # --- getting out -------------------------------------------------------

    def get_exit(self, market: MarketView) -> str | None:
        held = market.position
        if held is None:
            self._opened_at = None
            return None

        now = market.now or datetime.now(timezone.utc)
        if self._opened_at is None:
            # First sight of this position: it may have been opened by an
            # earlier run, so the clock starts now rather than never.
            self._opened_at = now

        stop = self.stop_price(held.side, held.entry_price)
        if self.stop_hit(market, stop):
            return f"stop hit at {stop:.2f}"

        middle = self.average(market)
        if middle is not None:
            came_back = market.price >= middle if held.side is Side.BUY else market.price <= middle
            if came_back:
                return "price came back to the average"

        held_for = (now - self._opened_at).total_seconds()
        if held_for >= self.max_hold_seconds:
            return f"held {held_for:.0f}s without coming back"

        return None

    def stop_price(self, side: Side, entry_price: Decimal) -> Decimal:
        """Where the stop sits for a position opened at ``entry_price``.

        Worked out from the position rather than remembered from the signal,
        so a restart - or a fill at a price the signal did not predict - does
        not leave the strategy guarding a stop that no longer matches the
        trade it is in.
        """
        if side is Side.BUY:
            return entry_price * (ONE - self.stop_fraction)
        return entry_price * (ONE + self.stop_fraction)

    # --- for the board -----------------------------------------------------

    def chart(self, market: MarketView) -> dict[str, Decimal] | None:
        """The average, the band either side of it, and the stop if holding.

        The settings travel too, so the board can say "average of 12 prices,
        0.03% either side" without knowing this class exists.
        """
        middle = self.average(market)
        if middle is None or middle <= 0:
            return None
        view = {
            "average": middle,
            "buy_below": middle * (ONE - self.edge_fraction),
            "sell_above": middle * (ONE + self.edge_fraction),
            "window": D(self.window),
            "edge_pct": self.edge_fraction * 100,
            "stop_pct": self.stop_fraction * 100,
            "max_hold_s": D(self.max_hold_seconds),
        }
        held = market.position
        if held is not None:
            view["stop"] = self.stop_price(held.side, held.entry_price)
            if self._opened_at is not None and market.now is not None:
                view["held_s"] = D(int((market.now - self._opened_at).total_seconds()))
        return view


__all__ = ["PingPongStrategy"]
