"""Strategy registry.

Add a strategy by writing the class and putting it in :data:`STRATEGIES`.  The
CLI reads this dictionary for ``--strategy`` and ``--list-strategies``, so a
new entry needs no changes anywhere else.
"""

from __future__ import annotations

import logging
from typing import Any

from .base import MarketView, Strategy
from .ma_cross import MaCrossStrategy
from .random_strategy import RandomStrategy

STRATEGIES: dict[str, type[Strategy]] = {
    "random": RandomStrategy,
    "ma_cross": MaCrossStrategy,
}


class UnknownStrategy(KeyError):
    """The requested strategy name is not registered."""


def build_strategy(
    name: str,
    config: Any = None,
    logger: logging.Logger | None = None,
    **kwargs: Any,
) -> Strategy:
    """Instantiate the registered strategy called *name*."""
    try:
        cls = STRATEGIES[name]
    except KeyError as exc:
        raise UnknownStrategy(
            f"unknown strategy {name!r}; available: {', '.join(sorted(STRATEGIES))}"
        ) from exc
    return cls(config, logger, **kwargs)


def describe_strategies() -> str:
    """A table of registered strategies for the CLI."""
    width = max(len(name) for name in STRATEGIES)
    return "\n".join(
        f"  {name.ljust(width)}  {cls.description or '(no description)'}"
        for name, cls in sorted(STRATEGIES.items())
    )


__all__ = [
    "MaCrossStrategy",
    "MarketView",
    "RandomStrategy",
    "STRATEGIES",
    "Strategy",
    "UnknownStrategy",
    "build_strategy",
    "describe_strategies",
]
