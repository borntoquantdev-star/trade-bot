"""Configuration tests, including regressions for two credential bugs."""

from dataclasses import FrozenInstanceError
from decimal import Decimal
from pathlib import Path

import pytest

from hyperbot.core.config import (
    DEFAULTS,
    BotConfig,
    ConfigError,
    Secret,
    describe,
    load_config,
    raw_defaults,
)
from hyperbot.money import D

ADDRESS = "0x1234567890abcdef1234567890abcdef12345678"
KEY = "0xaaaabbbbccccddddeeeeffff00001111222233334444555566667777888899990"

MINIMAL = {
    "hyperliquid": {"account_address": ADDRESS, "api_wallet_key": KEY},
    "trading": {"symbol": "ETH"},
}


def write(tmp_path, text: str):
    path = tmp_path / "config.yaml"
    path.write_text(text, encoding="utf-8")
    return path


# --- regression: shallow copy leaked credentials between objects ------------


def test_loading_one_config_does_not_touch_the_defaults():
    load_config(data=MINIMAL, env={})
    assert DEFAULTS["hyperliquid"]["api_wallet_key"] == ""
    assert DEFAULTS["hyperliquid"]["account_address"] == ""
    assert DEFAULTS["trading"]["symbol"] == "BTC"


def test_two_configs_never_share_state():
    first = load_config(data=MINIMAL, env={})
    second = load_config(env={})
    assert first.hyperliquid.api_wallet_key.reveal() == KEY
    assert second.hyperliquid.api_wallet_key.reveal() == ""
    assert second.trading.symbol == "BTC"


def test_raw_defaults_hands_back_an_independent_tree():
    mine = raw_defaults()
    mine["hyperliquid"]["api_wallet_key"] = "mine"
    mine["risk"]["max_positions"] = 99
    assert DEFAULTS["hyperliquid"]["api_wallet_key"] == ""
    assert DEFAULTS["risk"]["max_positions"] == 1


# --- regression: printing the config used to destroy the credentials -------


def test_describing_a_config_leaves_the_credentials_intact():
    config = load_config(data=MINIMAL, env={})
    text = describe(config)
    assert KEY not in text
    assert config.hyperliquid.api_wallet_key.reveal() == KEY


def test_secrets_refuse_to_appear_in_output():
    secret = Secret("hunter2")
    assert "hunter2" not in repr(secret)
    assert "hunter2" not in str(secret)
    assert "hunter2" not in f"{secret}"
    assert secret.reveal() == "hunter2"
    assert bool(secret) is True
    assert bool(Secret("")) is False


def test_repr_of_the_whole_config_hides_secrets():
    config = load_config(data=MINIMAL, env={})
    assert KEY not in repr(config)


def test_the_account_address_is_only_shown_in_part():
    text = describe(load_config(data=MINIMAL, env={}))
    assert ADDRESS not in text
    assert ADDRESS[:6] in text  # enough to recognise which account it is


# --- the two Hyperliquid identities are kept apart -------------------------


def test_an_api_wallet_key_in_the_address_field_is_rejected():
    """The docs warn that mixing these two up silently returns nothing."""
    with pytest.raises(ConfigError, match="master account"):
        load_config(data={"hyperliquid": {"account_address": KEY}}, env={})


def test_a_malformed_address_is_rejected():
    with pytest.raises(ConfigError, match="42-character"):
        load_config(data={"hyperliquid": {"account_address": "0xabc"}}, env={})


def test_an_empty_address_is_allowed_for_paper_trading():
    config = load_config(env={})
    assert not config.hyperliquid.account_address
    with pytest.raises(ConfigError):
        config.require_live_credentials()


def test_live_trading_demands_both_identities():
    config = load_config(data={"hyperliquid": {"account_address": ADDRESS}}, env={})
    with pytest.raises(ConfigError) as exc:
        config.require_live_credentials()
    assert "api_wallet_key" in str(exc.value)


def test_a_complete_config_passes_the_live_check():
    load_config(data=MINIMAL, env={}).require_live_credentials()


def test_mainnet_is_live_and_testnet_is_not():
    assert load_config(env={}).hyperliquid.is_live is False
    live = load_config(data={"hyperliquid": {"network": "mainnet"}}, env={})
    assert live.hyperliquid.is_live is True


def test_an_unknown_network_is_refused():
    with pytest.raises(ConfigError, match="network"):
        load_config(data={"hyperliquid": {"network": "devnet"}}, env={})


# --- reading files and the environment -------------------------------------


def test_reads_a_yaml_file(tmp_path):
    path = write(
        tmp_path,
        """
hyperliquid:
  network: testnet
trading:
  symbol: SOL
  timeframe: 15m
  loop_interval: 15
  warmup_candles: 50
risk:
  risk_per_trade_pct: 2.5
""",
    )
    config = load_config(path, env={})
    assert config.trading.symbol == "SOL"
    assert config.trading.timeframe == "15m"
    assert config.trading.loop_interval == 15
    assert config.trading.warmup_candles == 50
    assert config.risk.risk_per_trade == D("0.025")
    assert config.source == str(path)


def test_a_missing_file_is_an_error_not_a_silent_default(tmp_path):
    with pytest.raises(ConfigError, match="not found"):
        load_config(tmp_path / "nope.yaml")


def test_a_non_mapping_file_is_rejected(tmp_path):
    path = write(tmp_path, "- just\n- a list\n")
    with pytest.raises(ConfigError, match="mapping"):
        load_config(path)


def test_dollar_brace_values_come_from_the_environment(monkeypatch, tmp_path):
    monkeypatch.setenv("MY_KEY", "from-env")
    path = write(tmp_path, 'hyperliquid:\n  api_wallet_key: "${MY_KEY}"\n')
    config = load_config(path, env={})
    assert config.hyperliquid.api_wallet_key.reveal() == "from-env"


def test_a_missing_referenced_variable_is_reported(monkeypatch, tmp_path):
    monkeypatch.delenv("ABSENT_KEY", raising=False)
    path = write(tmp_path, 'hyperliquid:\n  api_wallet_key: "${ABSENT_KEY}"\n')
    with pytest.raises(ConfigError, match="ABSENT_KEY"):
        load_config(path)


def test_environment_variables_win_over_the_file(tmp_path):
    path = write(tmp_path, "trading:\n  symbol: BTC\n")
    config = load_config(path, env={"HYPERBOT_SYMBOL": "ETH"})
    assert config.trading.symbol == "ETH"
    assert config.instrument.symbol == "ETH"


def test_blank_environment_variables_are_ignored(tmp_path):
    path = write(tmp_path, "trading:\n  symbol: BTC\n")
    config = load_config(path, env={"HYPERBOT_SYMBOL": ""})
    assert config.trading.symbol == "BTC"


def test_the_network_can_come_from_the_environment():
    config = load_config(env={"HYPERLIQUID_NETWORK": "mainnet"})
    assert config.hyperliquid.is_live is True


# --- validation -------------------------------------------------------------


def test_percentages_become_fractions():
    config = load_config(
        data={"risk": {"max_daily_loss_pct": 3, "max_drawdown_pct": 10, "max_slippage_pct": 0.1}},
        env={},
    )
    assert config.risk.max_daily_loss == D("0.03")
    assert config.risk.max_drawdown == D("0.1")
    assert config.risk.max_slippage == D("0.001")


@pytest.mark.parametrize(
    "patch",
    [
        {"trading": {"loop_interval": 0}},
        {"trading": {"loop_interval": "soon"}},
        {"trading": {"timeframe": "7m"}},
        {"trading": {"warmup_candles": -1}},
        {"trading": {"symbol": ""}},
        {"risk": {"risk_per_trade_pct": 0}},
        {"risk": {"risk_per_trade_pct": 80}},
        {"risk": {"max_positions": 0}},
        {"risk": {"starting_equity_usdt": -1}},
        {"instrument": {"lot_step": 0}},
        {"instrument": {"tick_size": -1}},
        {"instrument": {"price_sig_figs": -1}},
    ],
)
def test_unusable_numbers_are_refused(patch):
    with pytest.raises(ConfigError):
        load_config(data=patch, env={})


def test_only_real_candle_intervals_are_accepted():
    for good in ("1m", "5m", "1h", "4h", "1d"):
        config = load_config(data={"trading": {"timeframe": good}}, env={})
        assert config.trading.timeframe == good


def test_the_instrument_follows_the_traded_symbol():
    config = load_config(data={"trading": {"symbol": "SOL"}}, env={})
    assert config.instrument.symbol == "SOL"


def test_describe_mentions_real_money_only_for_mainnet():
    paper = describe(load_config(env={}))
    live = describe(load_config(data={"hyperliquid": {"network": "mainnet"}}, env={}))
    assert "REAL MONEY" not in paper
    assert "REAL MONEY" in live


def test_a_config_is_frozen():
    config = load_config(env={})
    assert isinstance(config, BotConfig)
    with pytest.raises(FrozenInstanceError):
        config.trading = None  # type: ignore[misc]


def test_decimals_all_the_way_down():
    config = load_config(env={})
    assert isinstance(config.risk.risk_per_trade, Decimal)
    assert isinstance(config.instrument.tick_size, Decimal)
    assert isinstance(config.starting_equity, Decimal)


def test_the_default_instrument_matches_btc_on_hyperliquid():
    """BTC has szDecimals 5, so 1e-5 lots and at most 1 price decimal."""
    spec = load_config(env={}).instrument
    assert spec.lot_step == D("0.00001")
    assert spec.tick_size == D("0.1")
    assert spec.price_sig_figs == 5


def test_the_shipped_example_config_loads_as_is():
    """`cp config.example.yaml config.yaml` then `--dry-run` must just work."""
    example = Path(__file__).resolve().parent.parent / "config" / "config.example.yaml"
    config = load_config(example, env={})
    assert config.hyperliquid.network == "testnet"
    assert config.trading.symbol == "BTC"
    with pytest.raises(ConfigError):
        config.require_live_credentials()
