"""Broker implementations and the factory that picks one.

    from tradebot.brokers import build_live_broker
    from tradebot.brokers.paper import PaperBroker      # offline

Both live brokers import their exchange SDK lazily, inside ``connect()``, so
importing this package costs nothing and the test suite runs without either.
"""

from __future__ import annotations

import logging
from typing import Any

from .base import Broker, BrokerError, OrderReceipt


def build_live_broker(config: Any, logger: logging.Logger | None = None) -> Broker:
    """Return the broker for ``config.exchange``, ready to ``connect()``.

    Raises :class:`BrokerError` for an unknown exchange rather than falling
    back to one, because silently trading on the wrong venue is the worst
    possible default.
    """
    if config.exchange == "grvt":
        from .grvt import GrvtBroker  # noqa: PLC0415  (keeps import cost off the common path)

        return GrvtBroker(
            settings=config.grvt,
            logger=logger,
            fallback_instrument=config.instrument,
        )
    if config.exchange == "hyperliquid":
        from .hyperliquid import HyperliquidBroker  # noqa: PLC0415

        return HyperliquidBroker(
            settings=config.hyperliquid,
            logger=logger,
            min_notional=config.instrument.min_notional,
        )
    raise BrokerError(f"unknown exchange {config.exchange!r}")


__all__ = ["Broker", "BrokerError", "OrderReceipt", "build_live_broker"]
