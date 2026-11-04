"""Tests that talk to the real Hyperliquid testnet.

Run them on purpose, never by accident:

    pytest --run-integration

They need ``pip install -e ".[hyperliquid]"``, ``network: testnet`` and
credentials in the environment:

    HYPERLIQUID_ACCOUNT_ADDRESS   the master account (0x + 40 hex)
    HYPERLIQUID_API_WALLET_KEY    an API wallet private key it has approved

Every one of those is checked before anything is sent, and the module refuses
to run against mainnet. The one order it places is far from the market price
and is cancelled in a ``finally`` block.
"""

from __future__ import annotations

import os

import pytest

from hyperbot.brokers.base import BrokerError
from hyperbot.core.config import ConfigError, load_config
from hyperbot.money import D
from hyperbot.risk import risk_gate
from hyperbot.types import PortfolioState, Signal

pytestmark = pytest.mark.integration

REQUIRED_ENV = ("HYPERLIQUID_ACCOUNT_ADDRESS", "HYPERLIQUID_API_WALLET_KEY")


@pytest.fixture(scope="module")
def config():
    path = os.environ.get("HYPERBOT_TEST_CONFIG", "config/config.yaml")
    try:
        cfg = load_config(path if os.path.exists(path) else None)
    except ConfigError as exc:
        pytest.skip(f"no usable config: {exc}")

    if cfg.hyperliquid.network != "testnet":
        pytest.fail(
            f"integration tests refuse to run against {cfg.hyperliquid.network!r}. "
            "Set hyperliquid.network to 'testnet'."
        )
    missing = [name for name in REQUIRED_ENV if not os.environ.get(name)]
    if missing:
        pytest.skip(f"missing credentials in the environment: {', '.join(missing)}")
    return cfg


@pytest.fixture(scope="module")
def broker(config):
    try:
        from hyperbot.brokers.hyperliquid import HyperliquidBroker
    except ImportError as exc:  # pragma: no cover
        pytest.skip(f"Hyperliquid SDK not installed: {exc}")

    instance = HyperliquidBroker(
        settings=config.hyperliquid,
        min_notional=config.instrument.min_notional,
    )
    try:
        instance.connect()
    except BrokerError as exc:
        pytest.skip(f"cannot connect to the testnet: {exc}")
    return instance


# --- read-only checks --------------------------------------------------------


def test_the_mark_price_is_tradable(broker, config):
    assert broker.fetch_mark_price(config.trading.symbol) > 0


def test_the_account_reports_equity(broker):
    """Fails loudly if the address is an API wallet rather than the master."""
    assert broker.fetch_equity() >= 0


def test_positions_can_be_read(broker, config):
    assert isinstance(broker.fetch_positions(config.trading.symbol), dict)


def test_the_instrument_precision_matches_the_exchange(broker, config):
    spec = broker.fetch_instrument(config.trading.symbol)
    assert spec.tick_size > 0
    assert spec.lot_step > 0
    assert spec.price_sig_figs == 5
    # szDecimals drives both, so they multiply out to the perp maximum.
    assert spec.tick_size * spec.lot_step == D(1).scaleb(-6)


def test_candles_come_back_in_order(broker, config):
    candles = broker.fetch_candles(config.trading.symbol, config.trading.timeframe, 20)
    assert candles, "the testnet returned no candles"
    assert all(c.close > 0 for c in candles)
    assert candles == sorted(candles, key=lambda c: c.open_time)


def test_a_rounded_price_is_accepted_by_the_exchange_rules(broker, config):
    """A sanity check on our price rounding against a real mark price."""
    symbol = config.trading.symbol
    spec = broker.fetch_instrument(symbol)
    mark = broker.fetch_mark_price(symbol)
    rounded = spec.round_price(mark * D("0.999"), favour="down")
    digits = rounded.as_tuple()
    significant = len(str(abs(int(rounded.scaleb(-digits.exponent))))) if digits.exponent else 0
    assert rounded > 0
    if digits.exponent < 0:  # not an integer price, so the 5-figure cap applies
        assert significant <= 5


# --- one order, placed through the same gate the bot uses -------------------


def test_a_gated_order_round_trip(broker, config):
    """Place one far-from-market order sized by the risk gate, then cancel it.

    Sizing comes from :func:`risk_gate`, not by hand, so this also proves the
    quantity and price the live bot would send are acceptable to the exchange.
    """
    symbol = config.trading.symbol
    spec = broker.fetch_instrument(symbol)
    mark = broker.fetch_mark_price(symbol)

    # 30% below the market: far enough that it will not fill while we look.
    entry = spec.round_price(mark * D("0.70"), favour="down")
    signal = Signal.build(
        symbol=symbol,
        side="buy",
        ref_price=entry,
        stop_price=spec.round_price(entry * D("0.95"), favour="down"),
        reason="integration test",
        valid_for_seconds=120,
    )
    equity = max(broker.fetch_equity(), D(1000))
    state = PortfolioState(equity=equity, peak_equity=equity, positions={})
    from hyperbot.core.engine import utc_now

    decision = risk_gate(signal, state, config.risk, spec, entry, utc_now())
    if hasattr(decision, "code"):
        pytest.skip(f"the gate refused this test order: {decision}")

    receipt = broker.place_order(decision)
    try:
        assert receipt.broker_order_id
        assert receipt.simulated is False
    finally:
        try:
            broker.cancel_order(symbol, decision.client_order_id)
        except BrokerError as exc:  # pragma: no cover - cleanup only
            pytest.fail(
                f"could not cancel test order {receipt.broker_order_id}: {exc}. "
                "Cancel it by hand on the testnet."
            )
