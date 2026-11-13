//! Proves the board can reach GRVT and that the frames decode.
//!
//!     cargo run -p board --example probe              # testnet
//!     cargo run -p board --example probe -- prod ETH_USDT_Perp
//!
//! Prints the first few ticks and exits. This is the one part of the board
//! that cannot be fixed by reading the code: either the exchange speaks what
//! we think it speaks, or it does not.

use std::time::{Duration, Instant};

use board::market::{decode, subscribe_frame, Env, Frame, RATE_MS};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let env = match args.next().as_deref() {
        Some("prod") => Env::Prod,
        _ => Env::Testnet,
    };
    let instrument = args.next().unwrap_or_else(|| "BTC_USDT_Perp".to_string());

    let url = env.market_data_url();
    println!("connecting to {url} ({})", env.label());

    let (mut sender, receiver) = ewebsock::connect(url, ewebsock::Options::default())
        .map_err(|err| anyhow::anyhow!("connect failed: {err}"))?;

    let mut subscribed = false;
    let mut ticks = 0usize;
    // A rate can be overridden for probing which ones the exchange takes:
    //   RATE=200 cargo run -p board --example probe
    let rate: u32 = std::env::var("RATE")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(RATE_MS);
    let deadline = Instant::now() + Duration::from_secs(30);

    while Instant::now() < deadline && ticks < 5 {
        while let Some(event) = receiver.try_recv() {
            match event {
                ewebsock::WsEvent::Opened => {
                    println!("open -> subscribing to {instrument}");
                    sender.send(ewebsock::WsMessage::Text(subscribe_frame(
                        1,
                        &instrument,
                        rate,
                    )));
                    subscribed = true;
                }
                ewebsock::WsEvent::Message(ewebsock::WsMessage::Text(text)) => {
                    match decode(&text) {
                        Frame::Subscribed { stream, subs } => {
                            println!("subscribed  stream={stream} subs={subs:?}");
                        }
                        Frame::Tick(ticker) => {
                            ticks += 1;
                            println!(
                                "tick {ticks}  {}  mark={}  last={}  bid={}  ask={}  funding={}",
                                ticker.instrument,
                                ticker.mark_price,
                                ticker.last_price,
                                ticker.best_bid_price.clone().unwrap_or_default(),
                                ticker.best_ask_price.clone().unwrap_or_default(),
                                ticker.funding_rate.clone().unwrap_or_default(),
                            );
                        }
                        Frame::Refused { code, message } => {
                            println!("refused  code={code}  {message}");
                        }
                        Frame::Unknown(what) => println!("unknown  {what}"),
                    }
                }
                ewebsock::WsEvent::Message(other) => println!("non-text frame: {other:?}"),
                ewebsock::WsEvent::Error(err) => println!("error  {err}"),
                ewebsock::WsEvent::Closed => {
                    println!("closed by the exchange");
                    return Ok(());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    if !subscribed {
        anyhow::bail!("never opened the socket");
    }
    if ticks == 0 {
        anyhow::bail!("opened and subscribed, but no tick arrived in 30s");
    }
    println!("\nok: {ticks} ticks decoded from {}", env.label());
    Ok(())
}
