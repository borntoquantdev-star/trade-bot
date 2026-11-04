"""A coin-flip strategy, kept only as a plumbing test.

It exists so you can watch a signal travel through the risk gate to a paper
fill without having written any real logic yet.  It has no edge whatsoever:
over enough trades it loses exactly the fees.  Never point it at ``prod``.
"""

from __future__ import annotations

import logging
import random
from datetime import datetime, timezone
from decimal import Decimal
from typing import Any

from ..money import ONE, D
from ..types import Signal
from .base import MarketView, Strategy


class RandomStrategy(Strategy):
    """Fires a random long or short, with a fixed-percentage stop."""

    description = "coin flip with a fixed % stop - for testing the plumbing only"

    def __init__(
        self,
        config: Any = None,
        logger: logging.Logger | None = None,
        *,
        stop_pct: Decimal | str | float = "1.0",
        signal_chance: float = 0.3,
        min_seconds_between_signals: int = 0,
        rng: random.Random | None = None,
    ) -> None:
        super().__init__(config, logger)
        self.stop_fraction = D(stop_pct) / 100
        if not 0 < self.stop_fraction < 1:
            raise ValueError("stop_pct must be between 0 and 100")
        self.signal_chance = float(signal_chance)
        self.min_gap = int(min_seconds_between_signals)
        # Seeding makes the demo reproducible in tests.
        self.rng = rng or random.Random()
        self._last_signal_at: datetime | None = None

    def initialize(self) -> None:
        super().initialize()
        self.log.warning(
            "RandomStrategy has no edge. It is here to prove the pipeline works, "
            "not to make money. Replace it before going anywhere near real funds."
        )

    def get_signal(self, market: MarketView) -> Signal | None:
        if market.in_position:
            return None  # the gate would refuse anyway; save the round trip

        now = market.now or datetime.now(timezone.utc)
        if self._last_signal_at is not None and self.min_gap > 0:
            elapsed = (now - self._last_signal_at).total_seconds()
            if elapsed < self.min_gap:
                return None

        roll = self.rng.random()
        if roll < self.signal_chance:
            side = "buy"
        elif roll > 1 - self.signal_chance:
            side = "sell"
        else:
            return None

        self._last_signal_at = now
        if side == "buy":
            stop = market.price * (ONE - self.stop_fraction)
        else:
            stop = market.price * (ONE + self.stop_fraction)

        return Signal.build(
            symbol=market.symbol,
            side=side,
            ref_price=market.price,
            stop_price=stop,
            reason=f"coin flip rolled {roll:.3f}",
            created_at=now,
            valid_for_seconds=60,
        )
