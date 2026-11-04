"""Hyperliquid trading bot.

Import layers, outermost first:

* :mod:`hyperbot.money`, :mod:`hyperbot.types`, :mod:`hyperbot.risk` - pure
  logic.  No I/O, no third-party SDK, safe to import anywhere.
* :mod:`hyperbot.strategies` - turn market data into a :class:`~hyperbot.types.Signal`.
* :mod:`hyperbot.brokers` - the only code allowed to touch the network.
* :mod:`hyperbot.core.engine` - wires the three together.

Nothing here imports the exchange SDK at module level, so the package installs
and
its tests run with only PyYAML present.  The SDK is imported the moment a real
:class:`~hyperbot.brokers.hyperliquid.HyperliquidBroker` connects, and not
before.
"""

from __future__ import annotations

__version__ = "2.0.0"

from .money import D
from .risk import client_order_id, risk_gate
from .types import (
    InstrumentSpec,
    PortfolioState,
    Position,
    Reject,
    RejectCode,
    RiskLimits,
    Side,
    Signal,
    SizedOrder,
)

__all__ = [
    "D",
    "InstrumentSpec",
    "PortfolioState",
    "Position",
    "Reject",
    "RejectCode",
    "RiskLimits",
    "Side",
    "Signal",
    "SizedOrder",
    "__version__",
    "client_order_id",
    "risk_gate",
]
