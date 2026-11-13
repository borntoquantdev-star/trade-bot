"""Real prices for paper trading, without a single credential.

The paper broker normally invents its prices: a fixed number, or a random
walk around one.  That is fine for tests and useless in a classroom - the
board on the next screen is drawing the real market, and a bot buying at
100,000 while the market says 69,000 teaches the wrong lesson about what it
is watching.

GRVT publishes its mark price with no key and no cookie, which is the same
fact the board is built on.  This turns that into a
:data:`~tradebot.brokers.paper.PriceSource`, so ``--dry-run --paper-live``
is paper money moving at the speed of the real market.

Only the price is real.  The fills are still the paper broker's optimistic
ones; nothing here can place an order, and nothing here holds a secret.
"""

from __future__ import annotations

import json
import logging
import time
import urllib.error
import urllib.request
from decimal import Decimal, InvalidOperation

from ..money import D
from .base import BrokerError

#: How long a price is reused before the market is asked again.  The loop
#: may run faster than this; the exchange does not need to hear about it.
CACHE_SECONDS = 1.0

REST_ROOTS = {
    ("grvt", "testnet"): "https://market-data.testnet.grvt.io",
    ("grvt", "prod"): "https://market-data.grvt.io",
    ("grvt", "mainnet"): "https://market-data.grvt.io",
}


def _post_json(url: str, body: dict[str, object], timeout: float) -> dict[str, object]:
    request = urllib.request.Request(
        url,
        data=json.dumps(body).encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:  # noqa: S310
        payload = json.loads(response.read().decode("utf-8"))
    if not isinstance(payload, dict):
        raise BrokerError(f"{url} did not answer with an object")
    return payload


def live_mark_price(
    exchange: str,
    env: str,
    timeout: float = 5.0,
    logger: logging.Logger | None = None,
    clock=time.monotonic,
):
    """A price source that reads the public mark price of *exchange*.

    Raises :class:`BrokerError` when the venue has no public feed here, or
    when the market cannot be reached - the engine turns that into one
    recorded "broker read failed" tick and carries on, which is what a flaky
    connection should cost.
    """
    log = logger or logging.getLogger("tradebot.public_price")
    root = REST_ROOTS.get((exchange.lower(), env.lower()))
    if root is None:
        raise BrokerError(
            f"no public price feed for {exchange}/{env}; "
            "use --paper-walk for a simulated price instead"
        )

    cache: dict[str, tuple[float, Decimal]] = {}

    def source(symbol: str) -> Decimal:
        hit = cache.get(symbol)
        now = clock()
        if hit is not None and now - hit[0] < CACHE_SECONDS:
            return hit[1]
        try:
            payload = _post_json(f"{root}/full/v1/ticker", {"instrument": symbol}, timeout)
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError, OSError) as exc:
            raise BrokerError(f"public price for {symbol} unavailable: {exc}") from exc

        result = payload.get("result")
        if not isinstance(result, dict):
            message = payload.get("message", "no result")
            raise BrokerError(f"public price for {symbol} refused: {message}")
        raw = result.get("mark_price")
        try:
            price = D(str(raw))
        except (InvalidOperation, TypeError) as exc:
            raise BrokerError(f"public price for {symbol} unreadable: {raw!r}") from exc
        if price <= 0:
            raise BrokerError(f"public price for {symbol} came back as {price}")

        if symbol not in cache:
            log.info("paper trading on the live %s mark price (%s)", exchange, price)
        cache[symbol] = (now, price)
        return price

    return source


__all__ = ["CACHE_SECONDS", "REST_ROOTS", "live_mark_price"]
