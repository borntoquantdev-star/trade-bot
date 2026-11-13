"""Bar intervals, in one place.

The same table used to live in three modules (``core.config`` and both live
brokers), which is three chances to let them drift apart.  A timeframe the
config accepts but a broker does not know about fails at the worst possible
moment: after the credentials check, on the first call for history.

Durations are exact except for ``1M``, which the venues quote as a flat 30
days.  Nothing here does calendar arithmetic, so a month is a fixed span and
is documented as such rather than quietly assumed.
"""

from __future__ import annotations

from datetime import timedelta

#: Every interval the package knows, shortest first.  Venues advertise
#: subsets of this; each broker keeps its own tuple.
INTERVAL_SECONDS: dict[str, int] = {
    "1m": 60,
    "3m": 180,
    "5m": 300,
    "15m": 900,
    "30m": 1_800,
    "1h": 3_600,
    "2h": 7_200,
    "4h": 14_400,
    "8h": 28_800,
    "12h": 43_200,
    "1d": 86_400,
    "3d": 259_200,
    "1w": 604_800,
    "1M": 2_592_000,
}

#: What the config file accepts.  ``1M`` is left out on purpose: a monthly bar
#: is useless to a bot that checks the market every few minutes, and GRVT does
#: not serve it.
INTERVALS: tuple[str, ...] = tuple(k for k in INTERVAL_SECONDS if k != "1M")

INTERVAL_MS: dict[str, int] = {name: secs * 1000 for name, secs in INTERVAL_SECONDS.items()}


class UnknownInterval(ValueError):
    """The interval is not one this package knows how to measure."""


def interval_seconds(interval: str) -> int:
    """Length of one *interval* bar in seconds."""
    try:
        return INTERVAL_SECONDS[interval]
    except KeyError:
        raise UnknownInterval(
            f"interval must be one of {', '.join(INTERVAL_SECONDS)}, got {interval!r}"
        ) from None


def interval_delta(interval: str) -> timedelta:
    """Length of one *interval* bar as a :class:`~datetime.timedelta`."""
    return timedelta(seconds=interval_seconds(interval))


def interval_ms(interval: str) -> int:
    """Length of one *interval* bar in milliseconds."""
    return interval_seconds(interval) * 1000


__all__ = [
    "INTERVALS",
    "INTERVAL_MS",
    "INTERVAL_SECONDS",
    "UnknownInterval",
    "interval_delta",
    "interval_ms",
    "interval_seconds",
]
