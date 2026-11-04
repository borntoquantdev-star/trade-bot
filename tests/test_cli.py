"""CLI tests.

These call :func:`tradebot.cli.main.main` directly with argument lists, so they
cover the paths a user actually types - including the ones that must refuse to
do anything.
"""

import pytest

from tradebot.cli.main import build_parser, main

TESTNET_CONFIG = """
exchange: grvt
grvt:
  env: testnet
trading:
  symbol: BTC_USDT_Perp
  loop_interval: 1
  warmup_candles: 5
"""

SECRET_CONFIG = """
exchange: grvt
grvt:
  env: testnet
  api_key: SUPERSECRET
trading:
  symbol: BTC_USDT_Perp
  loop_interval: 1
  warmup_candles: 0
"""

MAINNET_CONFIG = """
exchange: grvt
grvt:
  env: prod
  api_key: k
  private_key: p
  trading_account_id: a
trading:
  symbol: BTC_USDT_Perp
  loop_interval: 1
"""

HYPERLIQUID_CONFIG = """
exchange: hyperliquid
hyperliquid:
  network: mainnet
  account_address: "0x1234567890abcdef1234567890abcdef12345678"
  api_wallet_key: "0xkey"
trading:
  symbol: BTC
  loop_interval: 1
"""


@pytest.fixture
def config_file(tmp_path):
    def write(text):
        path = tmp_path / "config.yaml"
        path.write_text(text, encoding="utf-8")
        return str(path)

    return write


def paths(tmp_path):
    return [
        "--log-file",
        str(tmp_path / "bot.log"),
        "--state-file",
        str(tmp_path / "state.json"),
        "--audit-file",
        str(tmp_path / "audit.jsonl"),
    ]


def test_the_parser_builds():
    assert build_parser().prog == "tradebot"


def test_listing_strategies_needs_no_config(capsys):
    assert main(["--list-strategies"]) == 0
    out = capsys.readouterr().out
    assert "random" in out and "ma_cross" in out


def test_a_dry_run_trades_on_paper(tmp_path, config_file):
    code = main(
        [
            "--config",
            config_file(TESTNET_CONFIG),
            "--dry-run",
            "--iterations",
            "3",
            "--strategy",
            "random",
            *paths(tmp_path),
        ]
    )
    assert code == 0
    assert (tmp_path / "audit.jsonl").exists()


def test_a_dry_run_needs_no_credentials(tmp_path, config_file):
    # TESTNET_CONFIG has no keys at all.
    assert (
        main(
            [
                "--config",
                config_file(TESTNET_CONFIG),
                "--dry-run",
                "--iterations",
                "1",
                *paths(tmp_path),
            ]
        )
        == 0
    )


def test_testnet_without_live_still_uses_the_paper_broker(tmp_path, config_file, capsys):
    main(["--config", config_file(TESTNET_CONFIG), "--iterations", "1", *paths(tmp_path)])
    assert "paper (offline)" in capsys.readouterr().out


def test_mainnet_refuses_to_run_without_the_live_flag(tmp_path, config_file, capsys):
    code = main(["--config", config_file(MAINNET_CONFIG), "--iterations", "1", *paths(tmp_path)])
    assert code == 2
    assert "--live was not passed" in capsys.readouterr().out


def test_dry_run_and_live_together_are_refused(tmp_path, config_file):
    code = main(["--config", config_file(TESTNET_CONFIG), "--dry-run", "--live", *paths(tmp_path)])
    assert code == 2


def test_a_missing_config_file_is_an_error(tmp_path):
    args = ["--config", str(tmp_path / "nope.yaml"), "--dry-run", *paths(tmp_path)]
    assert main(args) == 2


def test_an_unusable_config_is_an_error(tmp_path, config_file):
    bad = config_file("hyperliquid:\n  network: devnet\n")
    assert main(["--config", bad, "--dry-run", *paths(tmp_path)]) == 2


def test_an_unknown_strategy_is_an_error(tmp_path, config_file):
    code = main(
        [
            "--config",
            config_file(TESTNET_CONFIG),
            "--dry-run",
            "--strategy",
            "telepathy",
            *paths(tmp_path),
        ]
    )
    assert code == 2


def test_doctor_reports_on_the_paper_broker(tmp_path, config_file, capsys):
    code = main(
        ["--config", config_file(TESTNET_CONFIG), "--dry-run", "--doctor", *paths(tmp_path)]
    )
    out = capsys.readouterr().out
    assert code == 0
    assert "all good" in out
    assert "instrument" in out


def test_close_requires_confirmation(tmp_path, config_file, capsys):
    code = main(["--config", config_file(TESTNET_CONFIG), "--dry-run", "--close", *paths(tmp_path)])
    assert code == 2
    assert "--close --yes" in capsys.readouterr().out


def test_close_with_confirmation_runs(tmp_path, config_file):
    code = main(
        [
            "--config",
            config_file(TESTNET_CONFIG),
            "--dry-run",
            "--close",
            "--yes",
            *paths(tmp_path),
        ]
    )
    assert code == 0


def test_resume_clears_a_halt(tmp_path, config_file):
    state = tmp_path / "state.json"
    state.write_text('{"halted": true, "halt_reason": "test"}', encoding="utf-8")
    code = main(
        [
            "--config",
            config_file(TESTNET_CONFIG),
            "--dry-run",
            "--resume",
            "--log-file",
            str(tmp_path / "bot.log"),
            "--state-file",
            str(state),
            "--audit-file",
            str(tmp_path / "audit.jsonl"),
        ]
    )
    assert code == 0
    assert '"halted": false' in state.read_text()


def test_the_configuration_is_logged_without_secrets(tmp_path, config_file, capsys):
    main(
        [
            "--config",
            config_file(SECRET_CONFIG),
            "--dry-run",
            "--iterations",
            "1",
            *paths(tmp_path),
        ]
    )
    out = capsys.readouterr().out
    # The config block really was printed, and the key was not in it.
    assert "configuration:" in out
    assert "SUPERSECRET" not in out


def test_the_exchange_is_reported_and_mainnet_still_needs_live(tmp_path, config_file, capsys):
    """สลับตลาดได้ด้วย config ช่องเดียว และด่าน --live ยังทำงานทั้งสองฝั่ง"""
    code = main(
        ["--config", config_file(HYPERLIQUID_CONFIG), "--iterations", "1", *paths(tmp_path)]
    )
    out = capsys.readouterr().out
    assert code == 2
    assert "--live was not passed" in out
    assert "hyperliquid" in out


def test_a_grvt_config_runs_on_paper(tmp_path, config_file, capsys):
    code = main(
        [
            "--config",
            config_file(TESTNET_CONFIG),
            "--dry-run",
            "--iterations",
            "2",
            *paths(tmp_path),
        ]
    )
    assert code == 0
    assert "BTC_USDT_Perp" in capsys.readouterr().out
