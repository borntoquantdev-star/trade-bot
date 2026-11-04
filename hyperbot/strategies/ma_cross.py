"""A moving-average crossover, as a worked example of a real strategy.

It is the simplest thing that is still an actual rule: go long when the fast
average crosses above the slow one, go short when it crosses back below, and
put the stop a fixed percentage away.  Read it as a template for structure -
warm-up handling, cross detection, an explicit stop - not as a recommendation.

Being pure (no I/O, no clock of its own) it is directly unit testable; see
``tests/test_strategies.py``.
"""

from __future__ import annotations

import logging
from collections.abc import Sequence
from decimal import Decimal
from typing import Any

from ..money import ONE, ZERO, D
from ..types import Signal
from .base import MarketView, Strategy


def sma(prices: Sequence[Decimal], window: int) -> Decimal | None:
    """Simple moving average of the last *window* prices, or ``None`` if short."""
    if window <= 0:
        raise ValueError("window must be positive")
    if len(prices) < window:
        return None
    chunk = list(prices)[-window:]
    return sum(chunk, ZERO) / D(window)


class MaCrossStrategy(Strategy):
    """Long on a fast-above-slow cross, short on the way back down."""

    description = "fast/slow SMA crossover with a fixed % stop"

    def __init__(
        self,
        config: Any = None,
        logger: logging.Logger | None = None,
        *,
        fast: int = 10,
        slow: int = 30,
        stop_pct: Decimal | str | float = "1.5",
        allow_short: bool = True,
    ) -> None:
        super().__init__(config, logger)
        if fast >= slow:
            raise ValueError(f"fast window ({fast}) must be shorter than slow ({slow})")
        self.fast = fast
        self.slow = slow
        self.stop_fraction = D(stop_pct) / 100
        if not 0 < self.stop_fraction < 1:
            raise ValueError("stop_pct must be between 0 and 100")
        self.allow_short = allow_short

    def initialize(self) -> None:
        super().initialize()
        self.log.info(
            "MA cross: fast %s, slow %s, stop %s%% - needs %s observations before it speaks",
            self.fast,
            self.slow,
            self.stop_fraction * 100,
            self.slow + 1,
        )

    def get_signal(self, market: MarketView) -> Signal | None:
        if market.in_position:
            return None

        prices = list(market.recent_prices)
        # One extra point is needed to know which side of the cross we were on.
        if len(prices) < self.slow + 1:
            return None

        fast_now = sma(prices, self.fast)
        slow_now = sma(prices, self.slow)
        fast_before = sma(prices[:-1], self.fast)
        slow_before = sma(prices[:-1], self.slow)
        # Explicit checks rather than `None in (...)`: this is the form that
        # both a reader and a type checker can narrow.
        if fast_now is None or slow_now is None:
            return None
        if fast_before is None or slow_before is None:
            return None

        crossed_up = fast_before <= slow_before and fast_now > slow_now
        crossed_down = fast_before >= slow_before and fast_now < slow_now

        if crossed_up:
            side, stop = "buy", market.price * (ONE - self.stop_fraction)
        elif crossed_down and self.allow_short:
            side, stop = "sell", market.price * (ONE + self.stop_fraction)
        else:
            return None

        return Signal.build(
            symbol=market.symbol,
            side=side,
            ref_price=market.price,
            stop_price=stop,
            reason=(
                f"SMA{self.fast} {fast_now:.2f} crossed "
                f"{'above' if side == 'buy' else 'below'} SMA{self.slow} {slow_now:.2f}"
            ),
            created_at=market.now,
            valid_for_seconds=60,
        )
