//! The board itself.

use std::collections::{BTreeMap, BTreeSet};

use egui::{Rect, Sense, Vec2};

use crate::account::{self, Account, Kind as Acc};
use crate::bot::{self, Bot, Kind, Winner};
use crate::candles::{self, History, Load};
use crate::card::{self, Slot};
use crate::code::{self, Code};
use crate::depth;
use crate::equity;
use crate::exchange::{self, Exchange, Kind as Market};
use crate::feed::{Feed, Health};
use crate::history::{self, Past};
use crate::journey::{self, Mark, Phase, Trip};
use crate::market::{parse, Env};
use crate::shape;
use crate::theme;
use crate::traces::Traces;
use crate::wire;
use crate::zoom::Zoom;

/// The timeframes on the top bar. Every one of these is a name the
/// board's own `/api/kline` route knows how to translate.
const TIMEFRAMES: [&str; 6] = ["1m", "5m", "15m", "1h", "4h", "1d"];

const INSTRUMENTS: [&str; 4] = [
    "BTC_USDT_Perp",
    "ETH_USDT_Perp",
    "SOL_USDT_Perp",
    "XRP_USDT_Perp",
];

/// The three places worth jumping to. An infinite canvas is only useful if
/// a person can get back to where the cards are, and a student who scrolls
/// into empty space has lost the board.
const VIEWS: [(&str, [f32; 5]); 5] = [
    ("ตลาด", [0.0, 0.0, 1450.0, 1060.0, 0.0]),
    // Wide enough for the order's trip, which sits past the last stage.
    ("บอท", [350.0, 1130.0, 2270.0, 1950.0, 0.0]),
    ("ทดสอบ", [20.0, 1980.0, 1330.0, 2550.0, 0.0]),
    // The account as the exchange holds it, which is the zone that settles
    // an argument between the bot's own figure and the website's.
    ("บัญชีจริง", [20.0, 2590.0, 2110.0, 3240.0, 0.0]),
    // What happened before now: every run the bot has made, and what each
    // day came to.
    ("ประวัติ", [20.0, 3290.0, 1430.0, 3750.0, 0.0]),
];

/// The five stages of the pipeline, as card ids. One card each, so the road
/// a decision walks down is a road on the screen and not a list inside a
/// box.
const STAGES: [&str; 5] = ["s1", "s2", "s3", "s4", "s5"];

/// Which card feeds which. Drawn under the cards, so a wire leaves an edge
/// and arrives at an edge rather than crossing a face.
const WIRES: [(&str, &str); 7] = [
    ("s1", "s2"),
    // The strategy, and the picture of what it is looking at, under it.
    ("s2", "shape"),
    ("s2", "s3"),
    ("s3", "s4"),
    ("s4", "s5"),
    // The gate's answer depends on the account as much as on the signal:
    // a day's loss limit is about money, not about the market.
    ("account", "s3"),
    // Whatever the broker did ends up in the trail.
    ("s4", "log"),
];

/// The cards that moved down to make room for the strategy picture.
const MOVED_FOR_SHAPE: [&str; 14] = [
    "code",
    "backtest",
    "quality",
    "equity",
    "notes",
    "balance",
    "positions",
    "open_orders",
    "orders",
    "paid_funding",
    "closed",
    "fills",
    "runs",
    "calendar",
];

/// ASCII names for the zones, in the same order as `VIEWS`, for a link or
/// an environment variable - places where typing Thai is a nuisance.
const VIEW_ALIASES: [&str; 5] = ["market", "bot", "test", "account", "history"];

/// `%E0%B8%9A` back to the bytes it stands for. Anything that is not a
/// well-formed escape is kept as it was, and bytes that do not make UTF-8
/// are replaced rather than allowed to fail the whole name.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let pair = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(byte) = pair.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn view(index: usize) -> Rect {
    let [x0, y0, x1, y1, _] = VIEWS[index].1;
    Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1))
}

pub struct Board {
    scene_rect: Rect,
    slots: BTreeMap<&'static str, Slot>,
    feed: Feed,
    history: History,
    bot: Bot,
    /// Balance, positions, orders and fees - the account as GRVT holds it.
    /// Empty and harmless when the server has no key.
    account_data: Account,
    /// The book, the tape and the contract itself, from the exchange's own
    /// public endpoints.
    exchange: Exchange,
    /// Every run the bot has made, and what each day came to.
    past: Past,
    /// The traces of the ticks the cards have shown, fetched one at a time.
    traces: Traces,
    /// What part of each chart is on screen. Kept here rather than in the
    /// cards, because it is what the person looking wants, not a property of
    /// the data.
    candle_zoom: Zoom,
    equity_zoom: Zoom,
    /// The bot's own source: the five stage cards show the function that
    /// ran, and the code card shows all of it.
    code: Code,
    /// Which stage the code card is reading, and which of that stage's
    /// functions.
    ///
    /// The person picks it and it stays picked. Following the bot around
    /// would mean the panel jumped to another file halfway through a line
    /// somebody was reading, which is the one thing a code panel must not do.
    code_stage: usize,
    code_pick: usize,
    /// Which function the code card was showing last frame, so it can scroll
    /// to the code that ran when - and only when - that changes.
    code_at: Option<(String, usize)>,
    /// The tick being replayed, by its timestamp, or `None` to follow the
    /// newest one.
    ///
    /// By timestamp rather than by position: the trail is re-read every two
    /// seconds and its first rows fall off the front, so a position would
    /// quietly start pointing at a different tick. A timestamp is the tick.
    replay: Option<String>,
    /// Replay the newest order as soon as the trail arrives. Set by the
    /// `order` link, which is asked for before there is a trail to look in.
    replay_order_on_load: bool,
    /// Which order the trip card is following, by the tick that sent it.
    /// `None` follows the newest. A replayed tick that sent one wins over
    /// both, so stepping the pipeline onto an order shows that order's trip.
    trip_pick: Option<String>,
    /// The trip being watched for the moment it lands: its tick, whether it
    /// was still moving last frame, and when the board saw it fill. The glow
    /// is timed from *seeing* the fill, not from the fill's own stamp - the
    /// board learns of a fill a few seconds late, and a glow timed from the
    /// stamp had already faded by then (seen live on testnet, 14 Nov).
    trip_watch: Option<(String, bool, Option<f64>)>,
    /// Why pressing a run did not open it, until the next press.
    run_note: Option<String>,
    /// When the walk was paused, if it is.
    ///
    /// The head's position is worked out from the clock rather than stored,
    /// so five cards walk their own functions in step without five counters
    /// to keep honest. Pausing is therefore just freezing which clock
    /// reading they are all asked about.
    paused_at: Option<f64>,
    /// Lines per second the walk moves at: one of `WALK_RATES`.
    walk_rate: f64,
    /// The timestamp on the newest audit row, and when this board first saw
    /// it. A line that lit up a moment ago is drawn brighter for a moment,
    /// which is what makes a bot that is working *look* like one.
    seen_tick: String,
    seen_at: f64,
    /// Which timeframe the candles are drawn on.
    /// The cards picked out with a rubber band, by id.
    ///
    /// Arranging twenty cards one at a time is the kind of work that makes a
    /// person stop arranging them, so a group moves together.
    selected: BTreeSet<&'static str>,
    /// The rubber band being dragged right now, in scene coordinates: where
    /// it started and where the pointer is.
    marquee: Option<(egui::Pos2, egui::Pos2)>,
    /// Where the left button went down on the canvas, in scene
    /// coordinates.
    ///
    /// Kept because egui reports a drag only once the pointer has moved far
    /// enough to be sure it is a drag, and by then it reports where the
    /// pointer *is* - which, on a quick drag, is already over a card. The
    /// band has to begin where the button went down or it refuses to start
    /// at all. `press_origin` in egui would answer this, but in screen
    /// coordinates, and inside the scene those are not the same thing.
    press: Option<egui::Pos2>,
    /// The layout as it was last written, and when the next write is due.
    /// See `remember`.
    saved_layout: String,
    save_due: f64,
    /// Was the pointer over a card last frame?
    ///
    /// The scene is told which buttons pan **before** the cards are drawn,
    /// so like `hover` this is last frame's answer. One frame late is
    /// invisible to a hand holding a mouse, and it is what lets the left
    /// button mean two things: a band on the bare canvas, the old pan
    /// everywhere else.
    on_card: bool,
    timeframe: &'static str,
    /// Which chart the pointer was over last frame, and how many points that
    /// chart holds.
    ///
    /// Last frame, because egui hands the scene the wheel **before** the
    /// cards are drawn: a card cannot take the scroll for itself during its
    /// own frame, however early it asks. One frame of lag is invisible; a
    /// board that slides out from under the chart being zoomed is not.
    hover: Option<(Chart, usize)>,
    started: bool,
}

/// The two charts that can be zoomed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chart {
    Candles,
    Equity,
}

impl Default for Board {
    fn default() -> Self {
        let mut slots = BTreeMap::new();
        // The market zone, laid out like the exchange's own screen: price
        // and its two explanations on top, the chart with the book and the
        // tape beside it, the depth underneath.
        slots.insert("market", Slot::new(40.0, 40.0, 330.0, 250.0));
        slots.insert("funding", Slot::new(390.0, 40.0, 330.0, 250.0));
        slots.insert("info", Slot::new(740.0, 40.0, 380.0, 250.0));
        slots.insert("chart", Slot::new(40.0, 310.0, 680.0, 430.0));
        slots.insert("book", Slot::new(740.0, 310.0, 330.0, 430.0));
        slots.insert("trades", Slot::new(1090.0, 310.0, 330.0, 430.0));
        slots.insert("depth", Slot::new(40.0, 760.0, 680.0, 260.0));
        slots.insert("feed", Slot::new(740.0, 760.0, 680.0, 260.0));
        // The bot's own cards, on their own rows below the market.
        for (index, id) in STAGES.iter().enumerate() {
            // The gaps are wide on purpose: a wire needs room for a dot to
            // visibly travel down it, and two cards 26 pixels apart do not
            // have it.
            slots.insert(
                id,
                Slot::new(390.0 + index as f32 * 290.0, 1160.0, 240.0, 200.0),
            );
        }
        // Under the row of stages and to the left of the account, in the
        // space their own wires do not cross. Wide, because code read
        // through a slot is not read.
        // Straight under the strategy: what it is looking at, tick by tick.
        // A strategy is a rule about lines on a chart, and a rule read
        // without its chart is the part students stay confused by.
        slots.insert("shape", Slot::new(390.0, 1390.0, 560.0, 285.0));
        slots.insert("code", Slot::new(390.0, 1695.0, 560.0, 240.0));
        // These two sit under the stage they belong to, so their wires are
        // short verticals instead of long lines passing behind other cards.
        slots.insert("account", Slot::new(970.0, 1380.0, 300.0, 250.0));
        slots.insert("log", Slot::new(1390.0, 1380.0, 430.0, 250.0));
        // Past the last stage, as tall as the stage and the row under it:
        // an order's trip is five steps and a list of fills, read top down.
        slots.insert("trip", Slot::new(1840.0, 1160.0, 400.0, 470.0));
        slots.insert("backtest", Slot::new(40.0, 2000.0, 390.0, 320.0));
        slots.insert("quality", Slot::new(450.0, 2000.0, 390.0, 320.0));
        slots.insert("equity", Slot::new(870.0, 2000.0, 420.0, 320.0));
        slots.insert("notes", Slot::new(40.0, 2340.0, 800.0, 180.0));
        // The account zone. Money first and widest, because it is the
        // number a person looks for; the histories below it, because they
        // explain how the money got that way.
        slots.insert("balance", Slot::new(40.0, 2620.0, 380.0, 260.0));
        slots.insert("positions", Slot::new(440.0, 2620.0, 540.0, 260.0));
        slots.insert("open_orders", Slot::new(1000.0, 2620.0, 540.0, 260.0));
        // As tall as its row now, to make room under it for the funding
        // the account actually paid; it scrolls, so nothing is lost.
        slots.insert("orders", Slot::new(1560.0, 2620.0, 540.0, 260.0));
        slots.insert("paid_funding", Slot::new(1560.0, 2900.0, 540.0, 320.0));
        slots.insert("closed", Slot::new(40.0, 2900.0, 700.0, 320.0));
        slots.insert("fills", Slot::new(760.0, 2900.0, 780.0, 320.0));
        // The history zone. The list of runs is wide because a run is a
        // sentence; the calendar is square because a month is.
        slots.insert("runs", Slot::new(40.0, 3320.0, 790.0, 390.0));
        slots.insert("calendar", Slot::new(860.0, 3320.0, 570.0, 390.0));
        Self {
            // Opens on the market, not zoomed out to fit everything: eleven
            // cards at once is a map, not a screen.
            scene_rect: view(0),
            slots,
            feed: Feed::new(Env::Testnet, INSTRUMENTS[0]),
            past: Past::default(),
            traces: Traces::default(),
            history: History::default(),
            bot: Bot::default(),
            account_data: Account::default(),
            exchange: Exchange::default(),
            candle_zoom: Zoom::default(),
            equity_zoom: Zoom::default(),
            code: Code::default(),
            // The strategy, because it is the stage a person is most often
            // trying to understand and the only one they wrote themselves.
            code_stage: 1,
            code_pick: 0,
            code_at: None,
            paused_at: None,
            walk_rate: WALK_RATES[DEFAULT_RATE].0,
            replay: None,
            replay_order_on_load: false,
            trip_pick: None,
            trip_watch: None,
            run_note: None,
            seen_tick: String::new(),
            seen_at: 0.0,
            selected: BTreeSet::new(),
            marquee: None,
            press: None,
            saved_layout: String::new(),
            save_due: 0.0,
            on_card: false,
            timeframe: "5m",
            hover: None,
            started: false,
        }
    }
}

impl Board {
    /// Restore the layout somebody left behind, if there is one and it
    /// still makes sense.
    ///
    /// Positions only, and only for cards this version of the board has.
    /// A saved layout from an older build will be missing the cards added
    /// since and may name ones that are gone; both are normal, and neither
    /// is a reason to throw away the rest. Anything unknown is dropped and
    /// anything missing keeps its place from the fresh layout.
    pub fn restore(&mut self, saved: &str) {
        let Ok(saved) = serde_json::from_str::<BTreeMap<String, Slot>>(saved) else {
            return;
        };
        // A layout saved before the strategy picture existed has the code
        // card, and everything below the bot, where the picture and the
        // shifted rows now go. Those keep this version's places; the rest of
        // the person's arrangement is kept.
        let before_shape = !saved.contains_key("shape");
        let ids: Vec<&'static str> = self.slots.keys().copied().collect();
        for id in ids {
            if before_shape && MOVED_FOR_SHAPE.contains(&id) {
                continue;
            }
            if let Some(slot) = saved.get(id) {
                // The size is this build's business, the position is the
                // person's: a card that grew a row should still be the size
                // this version draws it, where they left it.
                let mut here = self.slots[id];
                here.x = slot.x;
                here.y = slot.y;
                self.slots.insert(id, here);
            }
        }
    }

    /// Open on a named zone - `bot`, `history`, or the Thai label itself.
    ///
    /// So a lesson can link straight to the part of the board it is about,
    /// instead of telling a room of students to find the right button.
    /// Unknown names are ignored and the board opens where it always does.
    pub fn open_at(&mut self, name: &str) -> bool {
        // A URL fragment arrives percent-encoded, so `#บอท` reaches the
        // page as `#%E0%B8%9A...`; decoded here rather than in the browser
        // glue so the native tests can see it happen.
        let wanted = percent_decode(name.trim().trim_start_matches('#')).to_lowercase();
        // `order` is a moment rather than a place: the pipeline, replaying
        // the newest tick that sent an order. The one link a worksheet
        // needs for "watch the bot place a trade".
        if wanted == "order" || wanted == "ออเดอร์" {
            self.scene_rect = view(1);
            self.replay_order_on_load = true;
            return true;
        }
        // `trip` is the send stage and the order's trip beside it, close
        // enough to read: framed from where those two cards are now, so it
        // still works after somebody has moved them.
        if wanted == "trip" || wanted == "ส่งคำสั่ง" {
            self.scene_rect = self
                .slot("s4")
                .rect()
                .union(self.slot("trip").rect())
                .expand(24.0);
            return true;
        }
        let found = VIEWS
            .iter()
            .zip(VIEW_ALIASES)
            .position(|((label, _), alias)| wanted == *label || wanted == alias);
        match found {
            Some(index) => {
                self.scene_rect = view(index);
                true
            }
            None => false,
        }
    }

    /// The layout, for the next time the board opens.
    pub fn layout(&self) -> String {
        serde_json::to_string(&self.slots).unwrap_or_default()
    }

    /// Save the layout, but not sixty times a second.
    ///
    /// Dragging a card changes the layout on every frame of the drag, and
    /// writing a file (or touching localStorage) that often is a way to
    /// make a smooth board stutter. Half a second after the last change is
    /// soon enough; the only way to lose anything is to close the window
    /// mid-drag.
    fn remember(&mut self, now: f64) {
        let layout = self.layout();
        if layout == self.saved_layout {
            return;
        }
        if self.save_due == 0.0 {
            self.save_due = now + 0.5;
            return;
        }
        if now >= self.save_due {
            crate::store::save(&layout);
            self.saved_layout = layout;
            self.save_due = 0.0;
        }
    }

    /// Put every card back where this version of the board puts it.
    ///
    /// The escape hatch for a layout dragged into a mess, which is what
    /// happens to every board that can be dragged.
    pub fn reset_layout(&mut self) {
        let fresh = Board::default();
        self.slots = fresh.slots;
        self.selected.clear();
        self.scene_rect = view(0);
        crate::store::clear();
        self.saved_layout = self.layout();
        self.save_due = 0.0;
    }

    fn slot(&mut self, id: &'static str) -> Slot {
        *self.slots.get(id).expect("every card has a slot")
    }

    /// Store a card, and move the rest of the selection with it.
    ///
    /// Every card already ends its draw by handing its slot back here, and
    /// `card::show` has already applied this frame's drag to it. So the
    /// difference between the slot going in and the slot that was there is
    /// exactly what the pointer did - which makes this the one place a group
    /// move belongs. The alternative is passing the selection into twenty
    /// call sites.
    fn put(&mut self, id: &'static str, slot: Slot) {
        let moved = match self.slots.insert(id, slot) {
            Some(before) => Vec2::new(slot.x - before.x, slot.y - before.y),
            None => Vec2::ZERO,
        };
        if moved == Vec2::ZERO || !self.selected.contains(id) {
            return;
        }
        // The dragged card has already moved; the others catch up.
        let others: Vec<&'static str> = self
            .selected
            .iter()
            .copied()
            .filter(|other| *other != id)
            .collect();
        for other in others {
            if let Some(slot) = self.slots.get_mut(other) {
                slot.translate(moved);
            }
        }
    }

    /// The rectangle every card sits inside.
    fn bounds(&self) -> Rect {
        let mut bounds: Option<Rect> = None;
        for slot in self.slots.values() {
            bounds = Some(match bounds {
                Some(bounds) => bounds.union(slot.rect()),
                None => slot.rect(),
            });
        }
        bounds.unwrap_or(Rect::ZERO)
    }

    /// Which cards a rubber band has caught.
    ///
    /// Touching counts, the way it does in a drawing program: demanding that
    /// a card be *wholly* inside means dragging a band across the whole
    /// board to pick up the one at the edge.
    fn cards_in(&self, band: Rect) -> BTreeSet<&'static str> {
        self.slots
            .iter()
            .filter(|(_, slot)| band.intersects(slot.rect()))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Is this point on a card, rather than on the canvas between them?
    fn on_a_card(&self, at: egui::Pos2) -> bool {
        self.slots.values().any(|slot| slot.rect().contains(at))
    }
}

impl eframe::App for Board {
    // eframe 0.37 hands the app a `Ui`, not a `Context`; panels are shown
    // inside it, and `TopBottomPanel` is now the unified `Panel::top`.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);

        if !self.started {
            self.started = true;
            self.feed.connect(now);
        }
        self.feed.poll(now);
        self.remember(now);
        self.history.poll();
        // Asked for again on a clock rather than once at start-up.
        let instrument = self.feed.instrument.clone();
        self.history.tick(now, &instrument, self.timeframe, 400);
        self.bot.poll(now);
        self.code.poll(now);
        self.traces.poll(now);
        // The trail arrives without traces; fetch the one being shown.
        if let Some((_, row)) = self.shown() {
            if row.trace.is_none() && row.traced {
                let at = row.at.clone();
                self.traces.want(&at, now);
            }
        }
        if self.replay_order_on_load && !self.bot.rows.is_empty() {
            self.replay_order_on_load = false;
            self.replay_at(self.last_order_tick());
        }
        self.mark_tick(now);
        self.exchange.watch(&self.feed.instrument.clone());
        self.exchange.poll(now);
        // An order still on its way: ask the exchange about it every couple
        // of seconds instead of every twenty, so "filled" shows up when it
        // happens rather than a card-refresh later.
        if self.newest_trip_is_live() {
            self.account_data.hurry(now);
        }
        self.account_data.poll(now);
        self.past.poll(now);

        // A live market never stops moving, so the board repaints steadily
        // rather than only on input - and faster while there are dots
        // travelling down a wire, because ten frames a second turns a moving
        // dot into a stuttering one.
        // A head that moves eight times a second cannot be drawn ten times
        // a second without stuttering.
        let busy = self.any_wire_flowing() || self.walking();
        ctx.request_repaint_after(std::time::Duration::from_millis(if busy {
            33
        } else {
            100
        }));

        egui::Panel::top("top")
            .frame(
                egui::Frame::NONE
                    .fill(theme::CANVAS)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| self.top_bar(ui, now));

        // Before the scene: see `Board::hover`.
        self.take_wheel(&ctx);
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.selected.clear();
        }

        let drag_the_chart = self.chart_takes_the_drag();
        // The left button belongs to the band only while Shift is held,
        // and only on bare canvas.
        let band = ctx.input(|input| input.modifiers.shift) && !self.on_card && !drag_the_chart;

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::CANVAS))
            .show(ui, |ui| {
                self.dot_grid(ui);
                self.show_scene(ui, now, band || drag_the_chart);
            });
    }
}

impl Board {
    fn top_bar(&mut self, ui: &mut egui::Ui, now: f64) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("tradebot board")
                    .size(14.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.add_space(14.0);
            let looking = self.looking_at();
            for (index, (label, _)) in VIEWS.iter().enumerate() {
                let here = looking == Some(index);
                let button =
                    egui::Button::new(egui::RichText::new(*label).size(11.5).color(if here {
                        theme::ACCENT
                    } else {
                        theme::MUTED
                    }))
                    .fill(if here {
                        theme::ACCENT.gamma_multiply(0.14)
                    } else {
                        egui::Color32::TRANSPARENT
                    })
                    .corner_radius(theme::chip_radius());
                if ui.add(button).clicked() {
                    self.scene_rect = view(index);
                }
            }

            // What the selection is for, only while there is one. A hint
            // that is always on screen is furniture; a hint that appears the
            // moment it applies is an answer.
            // A replay is the one state of the board that makes every
            // card lie about "now", so it is announced where "now" lives,
            // with the way back next to it.
            if self.replaying() {
                if let Some((_, row)) = self.shown() {
                    let at = bot::clock(&row.at);
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new(format!("กำลังเล่นซ้ำ tick {at}"))
                            .size(11.0)
                            .color(theme::ACCENT),
                    );
                    if ui
                        .button(egui::RichText::new("กลับไปล่าสุด").size(11.0))
                        .clicked()
                    {
                        self.replay_at(None);
                    }
                }
            }
            if !self.selected.is_empty() {
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(format!(
                        "เลือกไว้ {} ใบ · ลากหัวการ์ดใบไหนก็ได้ ไปทั้งกลุ่ม · Esc เพื่อยกเลิก",
                        self.selected.len()
                    ))
                    .size(11.0)
                    .color(theme::ACCENT),
                );
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (colour, text) = connection(self.feed.health(now));
                ui.label(egui::RichText::new(text).size(11.5).color(colour));
                // The clock every time on the board is read against, and the
                // name of the zone it is in: a column of times means nothing
                // until you know whose times they are.
                if let Some(wall) = crate::time::now_local() {
                    ui.label(
                        egui::RichText::new(format!("{} {}", crate::time::ZONE, wall.clock()))
                            .size(11.5)
                            .monospace()
                            .color(theme::MUTED),
                    );
                }
                ui.label(
                    egui::RichText::new(self.feed.env.label())
                        .size(11.5)
                        .color(theme::DIM),
                );
                if ui.button(egui::RichText::new("พอดีจอ").size(11.5)).clicked() {
                    self.scene_rect = Rect::ZERO; // the scene refits itself
                }
                if ui
                    .button(egui::RichText::new("จัดใหม่").size(11.5))
                    .on_hover_text("คืนตำแหน่งการ์ดทุกใบเป็นค่าตั้งต้น")
                    .clicked()
                {
                    self.reset_layout();
                }
            });
        });
    }

    /// The board itself: the infinite canvas and everything on it.
    ///
    /// Split out of `ui` so a test can drive the real scene rather than
    /// only the cards inside it. Panning lives in the scene, so a test that
    /// skips the scene cannot tell whether the board still moves - which is
    /// exactly the bug this split was written to catch.
    ///
    /// `hands_off_the_left_button` is true when something else has a claim
    /// on it this frame: a chart that is zoomed in, or Shift held down over
    /// bare canvas.
    fn show_scene(&mut self, ui: &mut egui::Ui, now: f64, hands_off_the_left_button: bool) {
        let scene = egui::Scene::new()
            .zoom_range(0.2..=3.0)
            .max_inner_size(Vec2::splat(20_000.0))
            // The scene decides this before the cards are drawn, so the
            // answer comes from what the pointer was on last frame.
            //
            //   a zoomed chart under the pointer -> the chart scrolls
            //   bare canvas with Shift held      -> a selection band
            //   anything else                    -> pan, as always
            .drag_pan_buttons(if hands_off_the_left_button {
                egui::DragPanButtons::MIDDLE | egui::DragPanButtons::SECONDARY
            } else {
                egui::DragPanButtons::all()
            });
        // Rect is Copy, so lift it out rather than borrowing `self` twice:
        // the scene needs it mutably and so do the cards.
        let mut scene_rect = self.scene_rect;
        scene.show(ui, &mut scene_rect, |ui| self.cards(ui, now));
        self.scene_rect = scene_rect;
    }

    /// The dotted canvas behind everything.
    fn dot_grid(&self, ui: &egui::Ui) {
        let rect = ui.max_rect();
        let painter = ui.painter();
        painter.rect_filled(rect, 0, theme::CANVAS);
        let step = 24.0;
        let mut y = rect.min.y;
        while y < rect.max.y {
            let mut x = rect.min.x;
            while x < rect.max.x {
                painter.circle_filled(egui::pos2(x, y), 1.0, theme::GRID);
                x += step;
            }
            y += step;
        }
    }

    /// What the canvas listens for.
    ///
    /// **This is the whole of why the board can still be panned.** The
    /// scene underneath moves the board by watching for a drag; a widget
    /// sitting on top of it that senses drags takes that drag away, and the
    /// board freezes - with the left button, the middle one and the right
    /// one all equally dead, because a drag sense catches every button.
    ///
    /// So the canvas only listens for a drag while Shift is held, which is
    /// the only time it has anything to do with one. The rest of the time
    /// it listens for a click, to clear the selection, and the scene gets
    /// every drag exactly as it did before any of this existed.
    fn canvas_sense(shift: bool) -> Sense {
        if shift {
            Sense::click_and_drag()
        } else {
            Sense::click()
        }
    }

    /// The empty canvas between the cards: drag it to pick several up.
    ///
    /// Registered before any card so that a card wins the pointer when they
    /// overlap, and refused outright when the drag starts on top of a card -
    /// otherwise dragging a zoomed chart would also drag a rubber band
    /// across the board behind it.
    fn canvas(&mut self, ui: &mut egui::Ui) {
        // `interact` does not allocate space, so this huge rectangle cannot
        // push the board's own bounds around - "พอดีจอ" still fits the cards.
        let area = self.bounds().expand(4000.0);
        // A fixed id, not a generated one: the band is drawn over several
        // frames and egui only knows it is the same drag because the id is
        // the same.
        let id = egui::Id::unique("board-canvas");
        // Shift is the whole rule. No mode to be in, nothing to switch
        // back: let go of the key and the board is the board again.
        let selecting = ui.input(|input| input.modifiers.shift);
        let response = ui.interact(area, id, Self::canvas_sense(selecting));
        // Only the left button draws a band; the middle and right ones
        // belong to the scene, which pans with them.
        let primary = ui.input(|input| input.pointer.primary_down());
        if !primary {
            self.press = None;
        } else if self.press.is_none() {
            // The frame the button went down. `interact_pointer_pos` is
            // Some only while this widget is the one being pressed, so this
            // is also the check that the press landed on bare canvas and
            // not on a card header.
            self.press = response.interact_pointer_pos();
        }

        // Remembered for the next frame, when the scene asks.
        self.on_card = response
            .hover_pos()
            .map(|at| self.on_a_card(at))
            .unwrap_or(false);

        if response.drag_started() {
            self.marquee = match self.press {
                Some(at) if selecting && !self.on_a_card(at) => Some((at, at)),
                _ => None,
            };
        }
        if response.dragged() {
            if let (Some((_, to)), Some(at)) =
                (self.marquee.as_mut(), response.interact_pointer_pos())
            {
                *to = at;
            }
        }
        if response.drag_stopped() {
            if let Some((from, to)) = self.marquee.take() {
                let band = Rect::from_two_pos(from, to);
                // A band so small it was really a click selects nothing,
                // rather than whatever happened to be under the pointer.
                self.selected = if band.width() > 4.0 || band.height() > 4.0 {
                    self.cards_in(band)
                } else {
                    BTreeSet::new()
                };
            }
        }
        // A click on bare canvas is how every other program says "never
        // mind".
        if response.clicked() {
            self.selected.clear();
        }
    }

    /// The accent outline on a chosen card, and the band itself.
    ///
    /// Drawn after the cards: an outline under a card is an outline nobody
    /// sees.
    fn draw_selection(&self, ui: &egui::Ui) {
        let painter = ui.painter();
        for id in &self.selected {
            let Some(slot) = self.slots.get(id) else {
                continue;
            };
            painter.rect_stroke(
                slot.rect().expand(3.0),
                theme::card_radius(),
                egui::Stroke::new(1.5, theme::ACCENT),
                egui::StrokeKind::Outside,
            );
        }
        if let Some((from, to)) = self.marquee {
            let band = Rect::from_two_pos(from, to);
            painter.rect_filled(band, 2, theme::ACCENT.gamma_multiply(0.10));
            painter.rect_stroke(
                band,
                2,
                egui::Stroke::new(1.0, theme::ACCENT),
                egui::StrokeKind::Inside,
            );
        }
    }

    fn cards(&mut self, ui: &mut egui::Ui, now: f64) {
        // First, so every card is above it.
        self.canvas(ui);
        self.market_card(ui, now);
        self.funding_card(ui);
        self.info_card(ui);
        self.book_card(ui);
        self.trades_card(ui);
        self.depth_card(ui);
        self.chart_card(ui, now);
        self.feed_card(ui);
        // Wires first: they belong under the cards they join.
        self.draw_wires(ui);
        self.account_card(ui);
        for (index, id) in STAGES.iter().enumerate() {
            self.stage_card(ui, index, id, now);
        }
        self.shape_card(ui);
        self.code_card(ui, now);
        self.log_card(ui);
        self.trip_card(ui);
        self.backtest_card(ui);
        self.quality_card(ui);
        self.equity_card(ui);
        self.notes_card(ui);
        self.balance_card(ui);
        self.positions_card(ui);
        self.open_orders_card(ui);
        self.orders_card(ui);
        self.closed_card(ui);
        self.fills_card(ui);
        self.paid_funding_card(ui);
        self.runs_card(ui);
        self.calendar_card(ui);
        // Last, so the outlines sit on top of what they outline.
        self.draw_selection(ui);
    }

    fn market_card(&mut self, ui: &mut egui::Ui, now: f64) {
        let mut slot = self.slot("market");
        let health = self.feed.health(now);
        let last = self.feed.last.clone();
        let state = if last.is_some() {
            theme::State::Ok
        } else {
            theme::State::Idle
        };
        let status = match health {
            Health::Live => format!("{} ครั้ง", self.feed.ticks),
            _ => connection(health).1,
        };
        let state = match health {
            Health::Live => state,
            Health::Connecting | Health::Quiet(_) => theme::State::Wip,
            Health::Down(_) => theme::State::Bad,
        };
        let instrument = self.feed.instrument.clone();
        let ticker = self.exchange.ticker.clone();
        card::show(
            ui,
            &mut slot,
            "ราคาตลาด",
            &status,
            state,
            |ui| match last {
                Some(t) => {
                    ui.label(
                        egui::RichText::new(&t.mark_price)
                            .size(24.0)
                            .monospace()
                            .color(theme::TEXT),
                    );
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{instrument} · mark"))
                                .size(10.5)
                                .color(theme::DIM),
                        );
                        // The move since the session opened, the same number the
                        // exchange prints beside its own big price.
                        if let Some(change) = ticker.as_ref().and_then(|t| t.change_pct()) {
                            ui.label(
                                egui::RichText::new(format!("{change:+.2}%"))
                                    .size(10.5)
                                    .color(bot::pnl_colour(change)),
                            );
                        }
                    });
                    // A price that stopped arriving must say so where the
                    // price is, not only in a corner of the top bar.
                    if !matches!(health, Health::Live) {
                        ui.label(egui::RichText::new(stale_line(health)).size(10.5).color(
                            match health {
                                Health::Down(_) => theme::BAD,
                                _ => theme::WARN,
                            },
                        ));
                    }
                    ui.add_space(6.0);
                    card::kv(ui, "ราคาล่าสุด", &t.last_price, theme::MUTED);
                    card::kv(ui, "ราคาอ้างอิง (index)", &t.index_price, theme::MUTED);
                    if let Some(ticker) = &ticker {
                        if let (Some(high), Some(low)) = (&ticker.high_24h, &ticker.low_24h) {
                            card::kv(
                                ui,
                                "24 ชม. สูง / ต่ำ",
                                &format!("{} / {}", high.text, low.text),
                                theme::MUTED,
                            );
                        }
                        if let Some(volume) = ticker.volume_24h() {
                            card::kv(ui, "ปริมาณ 24 ชม.", &format!("{volume:.1}"), theme::MUTED);
                        }
                        if let Some(oi) = &ticker.open_interest {
                            card::kv(ui, "สัญญาคงค้าง", &oi.text, theme::MUTED);
                        }
                    }
                }
                None => {
                    ui.label(
                        egui::RichText::new("รอราคาแรกจากตลาด")
                            .size(12.0)
                            .color(theme::MUTED),
                    );
                }
            },
        );
        self.put("market", slot);
    }

    /// The order book, as deep as the card is tall.
    ///
    /// Asks above, bids below, the spread between them - the arrangement
    /// every exchange uses, so a student reading this one can read theirs.
    /// The bar behind each row is **everything up to that price**, not the
    /// size at that price alone, and both sides share one scale.
    fn book_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("book");
        let book = self.exchange.book.clone();
        let load = self.exchange.load_of(Market::Book).clone();
        let spread = book.as_ref().and_then(|book| book.spread());
        let state = if book.is_some() {
            theme::State::Ok
        } else {
            theme::State::Idle
        };
        let status = match spread {
            Some((gap, pct)) => format!("ห่าง {gap:.1} ({pct:.4}%)"),
            None => "ยังไม่มีสมุด".to_string(),
        };
        let (state, status) = stale_if_failed(&load, state, status);
        card::show(
            ui,
            &mut slot,
            "สมุดคำสั่งซื้อขาย",
            &status,
            state,
            |ui| {
                let Some(book) = book else {
                    waiting(ui, &load, "");
                    return;
                };
                // Rows of a book touch each other. egui's default gap of a few
                // pixels between widgets adds up over twenty rows to more than
                // the card has, and the last thing on the card - the pressure
                // bar - was being pushed out past the clip and drawn nowhere.
                ui.spacing_mut().item_spacing.y = 0.0;
                // How many rows fit, split evenly between the two sides, with
                // the pressure bar's own space taken out first.
                let rows =
                    (((ui.available_height() - 34.0 - PRESSURE) / 2.0 / ROW) as usize).clamp(1, 20);
                let deepest = book.deepest(rows);

                columns(ui, "ราคา", "จำนวน", "สะสม");
                // Asks furthest from the spread first, so the best ask sits on
                // the line that divides the two sides.
                let asks: Vec<_> = book.asks.iter().take(rows).collect();
                let ask_totals = exchange::Book::totals(&book.asks);
                for (index, level) in asks.iter().enumerate().rev() {
                    book_row(ui, level, ask_totals[index], deepest, theme::BAD);
                }

                ui.add_space(2.0);
                match spread {
                    Some((gap, pct)) => ui.label(
                        egui::RichText::new(format!("ส่วนต่าง {gap:.1} · {pct:.4}%"))
                            .size(10.5)
                            .monospace()
                            .color(theme::MUTED),
                    ),
                    None => ui.label(egui::RichText::new("มีด้านเดียว").size(10.5).color(theme::DIM)),
                };
                ui.add_space(2.0);

                let bid_totals = exchange::Book::totals(&book.bids);
                for (index, level) in book.bids.iter().take(rows).enumerate() {
                    book_row(ui, level, bid_totals[index], deepest, theme::OK);
                }

                // Over the whole book, which is what the exchange's own screen
                // means by this number - not over the rows above, which would
                // leave the board disagreeing with the exchange by ten points
                // whenever somebody parks size away from the touch.
                if let Some((buy, sell)) = book.pressure() {
                    pressure_bar(ui, buy, sell, book.bids.len() + book.asks.len());
                }
            },
        );
        self.put("book", slot);
    }

    /// The tape: what actually traded, newest first.
    fn trades_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("trades");
        let trades = self.exchange.trades.clone();
        let load = self.exchange.load_of(Market::Trades).clone();
        let state = if trades.is_empty() {
            theme::State::Idle
        } else {
            theme::State::Ok
        };
        let status = format!("{} รายการ", trades.len());
        let (state, status) = stale_if_failed(&load, state, status);
        card::show(
            ui,
            &mut slot,
            "ซื้อขายล่าสุด",
            &status,
            state,
            |ui| {
                if trades.is_empty() {
                    waiting(ui, &load, "");
                    return;
                }
                columns(ui, "เวลา", "ราคา", "จำนวน");
                ui.spacing_mut().item_spacing.y = 0.0;
                egui::ScrollArea::vertical()
                    .id_salt("trades")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for trade in &trades {
                            // Green when the buyer crossed the spread, red when
                            // the seller did: the colour is about who was in a
                            // hurry, not about whether the price went up.
                            let colour = if trade.taker_buy {
                                theme::OK
                            } else {
                                theme::BAD
                            };
                            trade_row(
                                ui,
                                &exchange::clock(&trade.at),
                                &trade.price.text,
                                &trade.size.text,
                                colour,
                            );
                        }
                    });
            },
        );
        self.put("trades", slot);
    }

    /// How much is on offer between here and a given price.
    fn depth_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("depth");
        let book = self.exchange.book.clone();
        let load = self.exchange.load_of(Market::Book).clone();
        let state = if book.is_some() {
            theme::State::Ok
        } else {
            theme::State::Idle
        };
        let status = match &book {
            // The band is on the chip because a depth chart that quietly
            // dropped the far side would be a chart about a book nobody has.
            Some(_) => format!("±{:.1}% รอบกลางราคา", depth::BAND * 100.0),
            None => "ยังไม่มีสมุด".to_string(),
        };
        let (state, status) = stale_if_failed(&load, state, status);
        card::show(
            ui,
            &mut slot,
            "ความลึกของตลาด",
            &status,
            state,
            |ui| {
                let Some(book) = book else {
                    waiting(ui, &load, "");
                    return;
                };
                let area = ui.max_rect();
                if depth::draw(ui, area, &book) {
                    return;
                }
                ui.label(
                    egui::RichText::new("ชั้นราคายังน้อยเกินกว่าจะวาดความลึก")
                        .size(11.5)
                        .color(theme::MUTED),
                );
            },
        );
        self.put("depth", slot);
    }

    /// Funding: the rent a perpetual charges for holding a position, which
    /// is the cost the backtester openly does not model.
    fn funding_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("funding");
        let ticker = self.exchange.ticker.clone();
        let history = self.exchange.funding.clone();
        let spec = self.exchange.spec.clone();
        let load = self.exchange.load_of(Market::Ticker).clone();
        let hours = spec
            .as_ref()
            .map(|spec| spec.funding_interval_hours)
            .or_else(|| history.first().map(|row| row.interval_hours))
            .filter(|hours| *hours > 0)
            .unwrap_or(8);
        let now = ticker.as_ref().and_then(|t| t.funding_now.clone());
        let state = match &now {
            Some(_) => theme::State::Ok,
            None => theme::State::Idle,
        };
        let status = format!("ทุก {hours} ชม.");
        let (state, status) = stale_if_failed(&load, state, status);
        card::show(
            ui,
            &mut slot,
            "ค่าถือข้ามคืน (funding)",
            &status,
            state,
            |ui| {
                let (Some(ticker), Some(rate)) = (ticker, now) else {
                    waiting(ui, &load, "");
                    return;
                };
                egui::ScrollArea::vertical()
                    .id_salt("funding")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(format!("{:+.4}%", rate.value))
                                .size(22.0)
                                .monospace()
                                .color(bot::pnl_colour(rate.value)),
                        );
                        ui.label(
                            egui::RichText::new(exchange::who_pays(rate.value))
                                .size(10.5)
                                .color(theme::DIM),
                        );
                        ui.add_space(6.0);
                        if let Some(year) = exchange::annualised(rate.value, hours) {
                            card::kv(
                                ui,
                                "คิดเป็นต่อปี",
                                &format!("{year:+.2}%"),
                                bot::pnl_colour(year),
                            );
                        }
                        if let Some(avg) = &ticker.funding_avg {
                            card::kv(
                                ui,
                                "เฉลี่ย 8 ชม.",
                                &format!("{:+.4}%", avg.value),
                                theme::MUTED,
                            );
                        }
                        match ticker.until_funding() {
                            Some(seconds) => {
                                card::kv(ui, "รอบถัดไปอีก", &countdown(seconds), theme::WARN)
                            }
                            None => card::kv(ui, "รอบถัดไป", bot::UNMEASURED, theme::DIM),
                        }
                        if let Some(spec) = &spec {
                            if let (Some(cap), Some(floor)) =
                                (&spec.funding_cap, &spec.funding_floor)
                            {
                                card::kv(
                                    ui,
                                    "เพดาน / พื้น",
                                    &format!("{}% / {}%", cap.text, floor.text),
                                    theme::DIM,
                                );
                            }
                        }

                        ui.add_space(8.0);
                        section(ui, "ที่เก็บไปแล้ว");
                        if history.is_empty() {
                            ui.label(
                                egui::RichText::new("ยังไม่มีประวัติ")
                                    .size(10.5)
                                    .color(theme::DIM),
                            );
                        }
                        for row in history.iter().take(8) {
                            let (text, colour) = match &row.rate {
                                Some(rate) => {
                                    (format!("{:+.4}%", rate.value), bot::pnl_colour(rate.value))
                                }
                                None => (bot::UNMEASURED.to_string(), theme::DIM),
                            };
                            card::kv(ui, &exchange::clock(&row.at), &text, colour);
                        }
                    });
            },
        );
        self.put("funding", slot);
    }

    /// What the contract is: the numbers an order has to satisfy, and what
    /// the market did in the last day.
    fn info_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("info");
        let spec = self.exchange.spec.clone();
        let ticker = self.exchange.ticker.clone();
        let load = self.exchange.load_of(Market::Spec).clone();
        let state = match spec.as_ref().map(|spec| spec.status.as_str()) {
            Some("ACTIVE") => theme::State::Ok,
            Some(_) => theme::State::Bad,
            None => theme::State::Idle,
        };
        let status = spec
            .as_ref()
            .map(|spec| spec.status.clone())
            .unwrap_or_else(|| "ยังไม่มีข้อมูล".to_string());
        let (state, status) = stale_if_failed(&load, state, status);
        card::show(
            ui,
            &mut slot,
            "สเปคสัญญากับสถิติ 24 ชม.",
            &status,
            state,
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("info")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        match &spec {
                            Some(spec) => {
                                section(ui, "ออเดอร์ต้องผ่านเกณฑ์นี้");
                                if let Some(tick) = &spec.tick_size {
                                    card::kv(ui, "ราคาขยับทีละ", &tick.text, theme::MUTED);
                                }
                                if let Some(min) = &spec.min_size {
                                    card::kv(
                                        ui,
                                        "ขนาดขั้นต่ำ",
                                        &format!("{} {}", min.text, spec.base),
                                        theme::MUTED,
                                    );
                                }
                                if let Some(min) = &spec.min_notional {
                                    card::kv(
                                        ui,
                                        "มูลค่าขั้นต่ำ",
                                        &format!("{} {}", min.text, spec.quote),
                                        theme::WARN,
                                    );
                                }
                                if let Some(max) = &spec.max_position {
                                    card::kv(
                                        ui,
                                        "ถือได้มากสุด",
                                        &format!("{} {}", max.text, spec.base),
                                        theme::MUTED,
                                    );
                                }
                                card::kv(
                                    ui,
                                    "ชนิดสัญญา",
                                    &format!(
                                        "{} · funding ทุก {} ชม.",
                                        spec.kind, spec.funding_interval_hours
                                    ),
                                    theme::DIM,
                                );
                            }
                            None => waiting(ui, &load, ""),
                        }

                        ui.add_space(8.0);
                        section(ui, "24 ชั่วโมงที่ผ่านมา");
                        match &ticker {
                            Some(ticker) => {
                                if let Some(high) = &ticker.high_24h {
                                    card::kv(ui, "สูงสุด", &high.text, theme::OK);
                                }
                                if let Some(low) = &ticker.low_24h {
                                    card::kv(ui, "ต่ำสุด", &low.text, theme::BAD);
                                }
                                if let Some(open) = &ticker.open_24h {
                                    card::kv(ui, "ราคาเปิด", &open.text, theme::MUTED);
                                }
                                match ticker.volume_24h() {
                                    Some(volume) => card::kv(
                                        ui,
                                        "ปริมาณซื้อขาย",
                                        &format!("{volume:.3}"),
                                        theme::MUTED,
                                    ),
                                    None => card::kv(ui, "ปริมาณซื้อขาย", bot::UNMEASURED, theme::DIM),
                                }
                                if let Some(oi) = &ticker.open_interest {
                                    card::kv(ui, "สัญญาคงค้าง", &oi.text, theme::MUTED);
                                }
                                if let Some(ratio) = &ticker.long_short {
                                    card::kv(ui, "long ต่อ short", &ratio.text, theme::MUTED);
                                }
                            }
                            None => {
                                ui.label(
                                    egui::RichText::new("ยังไม่ได้ตัวเลขจากตลาด")
                                        .size(10.5)
                                        .color(theme::DIM),
                                );
                            }
                        }
                    });
            },
        );
        self.put("info", slot);
    }

    /// Candles from the store, with the live price folded into the newest
    /// bar. Falls back to a plain line of ticks while the store is empty, so
    /// the card still says something on a machine where the bot has never
    /// downloaded anything.
    fn chart_card(&mut self, ui: &mut egui::Ui, now: f64) {
        let mut slot = self.slot("chart");
        // The traded price, because the candles are trades. The mark is
        // shown on the price card, where it is labelled as the mark.
        //
        // **Only while it is still arriving.** A stale price stretched into
        // the newest bar is a candle nobody traded; the bars themselves keep
        // coming over HTTP every ten seconds, so a dead stream costs the
        // last few seconds of movement and not the chart.
        let live = if self.feed.stale(now) {
            None
        } else {
            self.feed
                .last
                .as_ref()
                .and_then(|t| t.last().or_else(|| t.mark()))
        };
        let all = self.history.with_live(live);
        let total = all.len();
        let (start, end) = self.candle_zoom.window(total);
        let bars = all[start..end].to_vec();
        // The live price belongs on the chart only while the newest bar is
        // on it. Scrolled back into last week, a dashed line at today's
        // price would be a line through history that never happened.
        let live = if end == total { live } else { None };
        let mut zoom = self.candle_zoom;
        let mut on_chart = false;
        let ticks: Vec<(f64, f64)> = self.feed.history.iter().copied().collect();
        let tick_range = self.feed.range();
        // The account's lines and trades, on the contract this chart shows.
        let over = crate::marks::overlay(
            &self.feed.instrument,
            &self.account_data,
            &self.bot.rows,
            now,
        );

        let (state, status) = match &self.history.status {
            Load::Ready if bars.len() >= 2 => (
                theme::State::Ok,
                format!(
                    "{} · {}",
                    if zoom.is_whole() {
                        format!("{total} แท่ง")
                    } else {
                        format!("{} จาก {total} แท่ง", bars.len())
                    },
                    self.history
                        .source
                        .map(|s| s.label())
                        .unwrap_or("ไม่ทราบที่มา")
                ),
            ),
            Load::Loading => (theme::State::Wip, "กำลังโหลด".to_string()),
            Load::Missing(_) => (theme::State::Idle, "ไม่มีแท่งเทียน".to_string()),
            Load::Failed(_) => (theme::State::Bad, "โหลดไม่ได้".to_string()),
            _ => (theme::State::Idle, format!("{} จุด", ticks.len())),
        };
        // The timeframe is not repeated here: its chip is two centimetres
        // to the right and lit up.
        let title = self.feed.instrument.clone();
        let missing = match &self.history.status {
            Load::Missing(reason) => Some(reason.clone()),
            Load::Failed(reason) => Some(reason.clone()),
            _ => None,
        };

        let instrument = self.feed.instrument.clone();
        let timeframe = self.timeframe;
        let mut pick_symbol: Option<&'static str> = None;
        let mut pick_timeframe: Option<&'static str> = None;
        card::show_with_tools(
            ui,
            &mut slot,
            &title,
            &status,
            state,
            |ui| {
                // The pickers for this chart, on this chart.
                for name in INSTRUMENTS {
                    if chip(ui, name.trim_end_matches("_USDT_Perp"), instrument == name) {
                        pick_symbol = Some(name);
                    }
                }
                ui.add_space(8.0);
                for name in TIMEFRAMES {
                    if chip(ui, name, timeframe == name) {
                        pick_timeframe = Some(name);
                    }
                }
            },
            |ui| {
                let area = ui.max_rect();
                if candles::draw(ui, area, &bars, live, &over) {
                    on_chart = chart_input(ui, area, "candles", &mut zoom, total, bars.len());
                    return;
                }

                // No candles: say why, then draw whatever ticks have arrived so
                // the card is not blank while the student reads the fix.
                if let Some(reason) = &missing {
                    ui.label(egui::RichText::new(reason).size(11.5).color(theme::MUTED));
                    ui.label(
                        egui::RichText::new("tradebot --fetch-candles --days 90")
                            .size(11.0)
                            .monospace()
                            .color(theme::ACCENT),
                    );
                    ui.add_space(6.0);
                }

                if ticks.len() < 2 {
                    ui.label(
                        egui::RichText::new("กำลังสะสมราคาสด")
                            .size(11.5)
                            .color(theme::DIM),
                    );
                    return;
                }
                let Some((lo, hi)) = tick_range else { return };
                let span = (hi - lo).max(f64::EPSILON);
                let plot = ui.max_rect().intersect(Rect::from_min_max(
                    egui::pos2(area.min.x, ui.cursor().min.y),
                    area.max,
                ));
                if plot.height() < 10.0 {
                    return;
                }
                let t0 = ticks[0].0;
                let t1 = ticks[ticks.len() - 1].0.max(t0 + 1e-9);
                let line: Vec<egui::Pos2> = ticks
                    .iter()
                    .map(|(t, p)| {
                        egui::pos2(
                            plot.min.x + ((t - t0) / (t1 - t0)) as f32 * plot.width(),
                            plot.max.y - ((p - lo) / span) as f32 * plot.height(),
                        )
                    })
                    .collect();
                ui.painter().add(egui::Shape::line(
                    line,
                    egui::Stroke::new(1.5, theme::MUTED),
                ));
            },
        );
        if let Some(symbol) = pick_symbol {
            self.feed.watch(symbol);
        }
        if let Some(name) = pick_timeframe {
            self.timeframe = name;
        }
        if pick_symbol.is_some() || pick_timeframe.is_some() {
            // The window into the old timeframe or the old contract means
            // nothing here: 120 bars of 1m is two hours, of 1d is a third
            // of a year.
            self.candle_zoom.reset();
        }
        self.candle_zoom = zoom;
        if on_chart {
            self.hover = Some((Chart::Candles, total));
        } else if matches!(self.hover, Some((Chart::Candles, _))) {
            self.hover = None;
        }
        self.put("chart", slot);
    }

    /// What the strategy looks like: the price, the lines it compares the
    /// price with, and where it acted.
    fn shape_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("shape");
        let newest = self.bot.rows.last();
        let run = newest.and_then(|row| self.past.runs.iter().find(|run| run.covers(&row.at)));
        let since = run.and_then(|run| crate::time::unix_of(&run.started_at));
        let name = run.map(|run| run.strategy.clone()).unwrap_or_default();
        let halted = self.shape_stopped();
        let ticks = shape::ticks(&self.bot.rows, shape::SHOWN, since);
        let banded = shape::has_band(&ticks);
        let settings = shape::settings(&self.bot.rows);
        // A replayed tick is marked on the picture, so stepping the
        // pipeline shows where on the chart that decision was made.
        let focus = if self.replaying() {
            self.shown().map(|(_, row)| row.at.clone())
        } else {
            None
        };
        let sentence = newest.and_then(shape::waiting_for);
        let (state, status) = if ticks.len() < 2 {
            (theme::State::Idle, "ยังไม่มี tick".to_string())
        } else if halted.is_some() {
            (theme::State::Idle, "หยุดแล้ว · ภาพรอบล่าสุด".to_string())
        } else if banded {
            (
                theme::State::Ok,
                if name.is_empty() {
                    format!("{} tick ล่าสุด", ticks.len())
                } else {
                    format!("{name} · {} tick", ticks.len())
                },
            )
        } else {
            (theme::State::Idle, "กลยุทธ์ไม่ได้บอกเส้น".to_string())
        };
        let load = self.bot.audit_load.clone();
        let (state, status) = stale_if_failed(&load, state, status);
        card::show(
            ui,
            &mut slot,
            "หน้าตากลยุทธ์ · มันดูเส้นอะไรอยู่",
            &status,
            state,
            |ui| {
                if ticks.len() < 2 {
                    waiting(ui, &load, "tradebot --strategy ping_pong --dry-run");
                    return;
                }
                // First, before anything that reads as live: a picture that
                // has stopped moving must say why.
                if let Some(why) = &halted {
                    ui.label(
                        egui::RichText::new(format!(
                            "{why} · รันต่อ: tradebot --config config/testnet.yaml --live"
                        ))
                        .size(11.0)
                        .color(theme::WARN),
                    );
                }
                if banded {
                    // The rule, in the words a person would use, with the
                    // strategy's own numbers in it.
                    let window = settings
                        .window
                        .map(|w| format!("เฉลี่ย {w:.0} ราคาล่าสุด"))
                        .unwrap_or_else(|| "ค่าเฉลี่ย".to_string());
                    let edge = settings
                        .edge_pct
                        .map(|e| format!("ห่างเส้นกลาง {e}%"))
                        .unwrap_or_else(|| "แถบ".to_string());
                    ui.label(
                        egui::RichText::new(format!(
                            "ราคาหลุดใต้แถบ = ซื้อ · ทะลุเหนือแถบ = ขาย · กลับมาแตะเส้นกลาง = ปิด \
                             (เส้นกลาง = {window} · ขอบแถบ = {edge})"
                        ))
                        .size(11.0)
                        .color(theme::TEXT),
                    );
                    ui.horizontal(|ui| {
                        shape::legend(ui, theme::TEXT, false, "ราคา");
                        shape::legend(ui, theme::INFO, false, "เส้นกลาง");
                        shape::legend(ui, theme::INFO.gamma_multiply(0.3), true, "แถบที่ไม่ทำอะไร");
                        shape::legend(ui, theme::BAD, false, "stop");
                        shape::legend(ui, theme::OK, true, "ซื้อ");
                        shape::legend(ui, theme::BAD, true, "ขาย/ปิด");
                    });
                } else {
                    ui.label(
                        egui::RichText::new(
                            "กลยุทธ์ที่รันอยู่ไม่ได้บอกว่ามันดูเส้นไหน (ต้องมี chart() ในคลาส) \
                             · เห็นแค่ราคากับจุดที่มันซื้อขาย · ping_pong บอกได้",
                        )
                        .size(11.0)
                        .color(theme::MUTED),
                    );
                }
                // Above the chart and wrapped: it is the sentence the
                // student reads first, and a long one painted on a single
                // line ran off the card (seen on the board, 16 Nov).
                if let Some(sentence) = &sentence {
                    ui.label(
                        egui::RichText::new(if halted.is_some() {
                            format!("ตอนจบรอบ: {sentence}")
                        } else {
                            format!("ตอนนี้: {sentence}")
                        })
                        .size(10.5)
                        .color(theme::ACCENT),
                    );
                }
                ui.add_space(4.0);
                let area = ui.available_rect_before_wrap();
                shape::draw(ui, area, &ticks, focus.as_deref());
            },
        );
        self.put("shape", slot);
    }

    /// Why the strategy picture has stopped moving, if it has.
    fn shape_stopped(&self) -> Option<String> {
        let newest = self.bot.rows.last()?;
        let run = self.past.runs.iter().find(|run| run.covers(&newest.at));
        shape::stopped(
            run.map(|run| run.ended_at.as_str()),
            &newest.at,
            crate::time::now_unix(),
        )
    }

    fn feed_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("feed");
        let notes: Vec<String> = self.feed.notes.iter().cloned().collect();
        let state = if notes.is_empty() {
            theme::State::Idle
        } else {
            theme::State::Wip
        };
        let status = format!("{} บรรทัด", notes.len());
        card::show(
            ui,
            &mut slot,
            "สิ่งที่ตลาดบอกมา",
            &status,
            state,
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("feed")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for note in &notes {
                            ui.label(
                                egui::RichText::new(note)
                                    .size(11.0)
                                    .monospace()
                                    .color(theme::MUTED),
                            );
                            ui.add_space(2.0);
                        }
                    });
            },
        );
        self.put("feed", slot);
    }
}

impl Board {
    /// Money, position and whether the bot has stopped itself.
    fn account_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("account");
        let account = self.bot.account.clone();
        let has = self.bot.has_live();
        let load = self.bot.state_load.clone();
        let state = if account.halted {
            theme::State::Bad
        } else if has {
            theme::State::Ok
        } else {
            theme::State::Idle
        };
        let status = if !has {
            "ยังไม่มีข้อมูล".to_string()
        } else if account.day.is_empty() {
            "มีข้อมูล".to_string()
        } else {
            // The bot's trading day: its loss limit resets at midnight in
            // Bangkok, the same day the calendar uses. Said on the chip so a
            // bot still running on yesterday's budget (no tick since
            // midnight) is visible rather than assumed.
            format!("วันของบอท {}", account.day)
        };
        let (state, status) = stale_if_failed(&load, state, status);
        // The exchange figure, when the server has a key. It leads the card
        // when it is there: the bot figure is an opinion, this one is the
        // balance.
        let real = self
            .account_data
            .balance
            .as_ref()
            .and_then(|balance| balance.equity.clone());
        card::show(
            ui,
            &mut slot,
            "ทุนกับสถานะบอท",
            &status,
            state,
            |ui| {
                if !has {
                    waiting(ui, &load, "tradebot --dry-run");
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("ถ้าบอทรันอยู่ที่อื่น ให้ชี้ BOARD_LOGS ไปที่โฟลเดอร์ logs ของมัน")
                            .size(10.5)
                            .color(theme::DIM),
                    );
                    return;
                }
                // Halted first and loudest: every number under it is history.
                if account.halted {
                    ui.label(
                        egui::RichText::new(format!(
                            "หยุดอยู่: {}",
                            if account.halt_reason.is_empty() {
                                "ไม่ได้บันทึกเหตุผล"
                            } else {
                                &account.halt_reason
                            }
                        ))
                        .size(11.5)
                        .color(theme::BAD),
                    );
                    ui.add_space(6.0);
                }
                let headline = real
                    .as_ref()
                    .map(|num| num.text.clone())
                    .unwrap_or_else(|| bot::money(account.equity));
                ui.label(
                    egui::RichText::new(headline)
                        .size(24.0)
                        .monospace()
                        .color(theme::TEXT),
                );
                ui.label(
                    egui::RichText::new(match (&real, account.symbol.is_empty()) {
                        // Saying where the number came from is the whole point:
                        // one of these can be checked against the website and
                        // the other cannot.
                        (Some(_), _) => "ทุนจริงที่ตลาด".to_string(),
                        (None, true) => "ทุนที่บอทบันทึกไว้".to_string(),
                        (None, false) => format!("ทุนที่บอทบันทึกไว้ · {}", account.symbol),
                    })
                    .size(10.5)
                    .color(theme::DIM),
                );
                ui.add_space(6.0);
                if let Some(real) = &real {
                    card::kv(ui, "ที่บอทบันทึกไว้", &bot::money(account.equity), theme::MUTED);
                    if let Some(gap) = account::drift(real.value, account.equity) {
                        card::kv(ui, "ต่างกัน", &bot::signed(gap), theme::WARN);
                    }
                }
                card::kv(
                    ui,
                    "กำไรขาดทุนวันนี้ (นับจากเที่ยงคืน)",
                    &format!(
                        "{} ({})",
                        bot::signed(account.day_pnl),
                        bot::pct(account.day_pnl_pct)
                    ),
                    bot::pnl_colour(account.day_pnl),
                );
                card::kv(ui, "ทุนต้นวัน", &bot::money(account.day_start), theme::MUTED);
                card::kv(ui, "จุดสูงสุด", &bot::money(account.peak), theme::MUTED);
                card::kv(
                    ui,
                    "ต่ำกว่าจุดสูงสุด",
                    &format!("{:.2}%", account.drawdown_pct),
                    if account.drawdown_pct > 0.0 {
                        theme::WARN
                    } else {
                        theme::MUTED
                    },
                );
                card::kv(
                    ui,
                    "ออเดอร์ที่ส่งแล้ว",
                    &account.sent_orders.to_string(),
                    theme::MUTED,
                );
                if !account.updated_at.is_empty() {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "บรรทัดล่าสุด {} {}",
                            bot::clock(&account.updated_at),
                            crate::time::ZONE
                        ))
                        .size(10.0)
                        .color(theme::FAINT),
                    );
                }
            },
        );
        self.put("account", slot);
    }

    /// Give the wheel to whichever chart the pointer is on, and hide it
    /// from the scene so the board does not zoom out from under it.
    fn take_wheel(&mut self, ctx: &egui::Context) {
        let Some((chart, total)) = self.hover else {
            return;
        };
        let scroll = ctx.input_mut(|input| {
            let delta = input.smooth_scroll_delta.y;
            if delta != 0.0 {
                input.smooth_scroll_delta = egui::Vec2::ZERO;
            }
            delta
        });
        if scroll == 0.0 {
            return;
        }
        let factor = zoom_factor(scroll);
        match chart {
            Chart::Candles => self.candle_zoom.zoom_by(factor, total),
            Chart::Equity => self.equity_zoom.zoom_by(factor, total),
        }
    }

    /// Should a left-drag scroll the chart instead of the board?
    ///
    /// Only when there is something to scroll **to**. A chart showing every
    /// bar it has cannot be panned, and swallowing the drag there would
    /// leave the gesture doing nothing at all, which reads as a broken
    /// board rather than as a full one.
    fn chart_takes_the_drag(&self) -> bool {
        match self.hover {
            Some((Chart::Candles, _)) => !self.candle_zoom.is_whole(),
            Some((Chart::Equity, _)) => !self.equity_zoom.is_whole(),
            None => false,
        }
    }

    /// Which zone the board is looking at, if it is looking at one.
    ///
    /// Worked out from where the camera is rather than remembered from the
    /// last button pressed: somebody who drags from one zone to the next has
    /// moved, and a bar still lighting the button they pressed five minutes
    /// ago would be telling them where they used to be.
    fn looking_at(&self) -> Option<usize> {
        let middle = self.scene_rect.center();
        (0..VIEWS.len()).find(|index| view(*index).contains(middle))
    }

    /// The state of every card a wire touches.
    ///
    /// Worked out before anything is drawn, because the wires go down first
    /// and they have to know what they are joining.
    fn card_states(&self) -> Vec<(&'static str, theme::State)> {
        let has = self.bot.has_live();
        let mut out: Vec<(&'static str, theme::State)> = Vec::with_capacity(7);
        for (index, id) in STAGES.iter().enumerate() {
            let state = match self.stages_now().get(index) {
                Some(stage) if has => stage.state,
                _ => theme::State::Idle,
            };
            out.push((id, state));
        }
        out.push((
            "account",
            if self.bot.account.halted {
                theme::State::Bad
            } else if has {
                theme::State::Ok
            } else {
                theme::State::Idle
            },
        ));
        out.push((
            "shape",
            if has
                && self.shape_stopped().is_none()
                && self
                    .bot
                    .rows
                    .iter()
                    .rev()
                    .take(5)
                    .any(|row| row.view.is_some())
            {
                theme::State::Ok
            } else {
                theme::State::Idle
            },
        ));
        out.push((
            "log",
            if !has || self.bot.log.is_empty() {
                theme::State::Idle
            } else if self.bot.log.iter().any(|entry| entry.kind == Kind::Error) {
                theme::State::Bad
            } else {
                theme::State::Wip
            },
        ));
        out
    }

    fn state_of(states: &[(&'static str, theme::State)], id: &str) -> theme::State {
        states
            .iter()
            .find(|(key, _)| *key == id)
            .map(|(_, state)| *state)
            .unwrap_or(theme::State::Idle)
    }

    /// True when anything is moving down a wire right now.
    fn any_wire_flowing(&self) -> bool {
        let states = self.card_states();
        WIRES.iter().any(|(from, to)| {
            wire::flowing(Self::state_of(&states, from), Self::state_of(&states, to))
        })
    }

    fn draw_wires(&mut self, ui: &egui::Ui) {
        let states = self.card_states();
        let now = ui.input(|i| i.time);
        let painter = ui.painter();
        for (from, to) in WIRES {
            let (Some(a), Some(b)) = (self.slots.get(from), self.slots.get(to)) else {
                continue;
            };
            let flowing = wire::flowing(Self::state_of(&states, from), Self::state_of(&states, to));
            wire::draw(painter, a.rect(), b.rect(), flowing, now);
        }
    }

    // ------------------------------------------------------- the bot's code

    /// The trace on the newest audit row, if the bot recorded one.
    ///
    /// Older rows, and rows from a run started with `--no-trace`, simply do
    /// not have one. Every card that reads this must cope with that, because
    /// a student's first audit trail will often be one of those.
    fn trace(&self) -> Option<&bot::Trace> {
        let (_, row) = self.shown()?;
        // On the row itself when it came that way (an older server, or a
        // test); otherwise fetched on its own and kept.
        row.trace.as_ref().or_else(|| self.traces.get(&row.at))
    }

    /// The row the five cards are showing, and where it is in the trail:
    /// the one being replayed if it is still in the trail, else the newest.
    fn shown(&self) -> Option<(usize, &bot::Row)> {
        if let Some(at) = &self.replay {
            if let Some(index) = self.bot.rows.iter().rposition(|row| &row.at == at) {
                return Some((index, &self.bot.rows[index]));
            }
        }
        let index = self.bot.rows.len().checked_sub(1)?;
        Some((index, &self.bot.rows[index]))
    }

    /// Is a past tick on the cards, rather than the newest?
    fn replaying(&self) -> bool {
        match (&self.replay, self.bot.rows.last()) {
            (Some(at), Some(last)) => {
                at != &last.at && self.bot.rows.iter().any(|row| &row.at == at)
            }
            _ => false,
        }
    }

    // ------------------------------------------------------------ the trip
    // ออกแบบโดย Claude (คืน 14 พ.ย. 2026) — รอรีวิว

    /// The trip on screen, the exchange's view of it, where it has got to,
    /// and which of all the trips it is (0-based, and how many).
    fn trip_now(&self) -> Option<(Trip, journey::Seen, Phase, usize, usize)> {
        let trips = journey::trips(&self.bot.rows);
        if trips.is_empty() {
            return None;
        }
        let replayed = self
            .replay
            .as_ref()
            .and_then(|at| trips.iter().rposition(|trip| &trip.tick_at == at));
        let picked = self
            .trip_pick
            .as_ref()
            .and_then(|at| trips.iter().rposition(|trip| &trip.tick_at == at));
        let index = replayed.or(picked).unwrap_or(trips.len() - 1);
        let trip = trips[index].clone();
        let account = &self.account_data;
        let seen = journey::find(&trip, &account.open_orders, &account.orders, &account.fills);
        let phase = journey::phase(&trip, &seen, account.has_key(), unix_now());
        Some((trip, seen, phase, index, trips.len()))
    }

    fn newest_trip_is_live(&self) -> bool {
        let Some(trip) = self.bot.rows.iter().rev().find_map(Trip::from_row) else {
            return false;
        };
        let account = &self.account_data;
        let seen = journey::find(&trip, &account.open_orders, &account.orders, &account.fills);
        journey::phase(&trip, &seen, account.has_key(), unix_now()).is_live()
    }

    /// One order, followed from the tick that decided it to the trade that
    /// filled it: how long each leg took, whether it is still waiting in
    /// the book, how much of it has traded and at what price.
    fn trip_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("trip");
        let now = unix_now();
        let current = self.trip_now();
        let trips = journey::trips(&self.bot.rows);
        let mut pick: Option<Option<String>> = None;
        let mut replay: Option<String> = None;

        let (state, status) = match &current {
            None => (theme::State::Idle, "ยังไม่ได้ส่ง".to_string()),
            Some((trip, seen, phase, ..)) => {
                let t = journey::times(trip, seen, phase, now);
                let state = if phase.is_bad() {
                    theme::State::Bad
                } else if phase.is_live() {
                    theme::State::Wip
                } else if *phase == Phase::Filled {
                    theme::State::Ok
                } else {
                    theme::State::Idle
                };
                let status = match (phase, t.total) {
                    (Phase::Waiting { .. }, Some(total)) => format!("รอ {}", journey::span(total)),
                    (Phase::Looking, _) => "กำลังหา".to_string(),
                    (Phase::Filled, Some(total)) => format!("ครบใน {}", journey::span(total)),
                    (Phase::Filled, None) => "ครบ".to_string(),
                    (Phase::Simulated, _) => "จำลอง".to_string(),
                    (Phase::Blind, _) => "ตามต่อไม่ได้".to_string(),
                    (Phase::Refused(_), _) | (Phase::Rejected(_), _) => "ไม่รับ".to_string(),
                    (Phase::Cancelled { .. }, _) => "ยกเลิก".to_string(),
                    _ => "ไม่เจอ".to_string(),
                };
                (state, status)
            }
        };
        if current
            .as_ref()
            .is_some_and(|(_, _, phase, ..)| phase.is_live())
        {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }

        card::show(
            ui,
            &mut slot,
            "เส้นทางออเดอร์ · ตามจนจับคู่",
            &status,
            state,
            |ui| {
                let Some((trip, seen, phase, index, count)) = &current else {
                    ui.label(
                        egui::RichText::new(
                            "ยังไม่มีออเดอร์ในบันทึกของบอท · พอกลยุทธ์ออกสัญญาณและด่านความเสี่ยง\
                             อนุมัติ การ์ดนี้จะตามคำสั่งตั้งแต่ส่งจนจับคู่",
                        )
                        .size(10.5)
                        .color(theme::FAINT),
                    );
                    return;
                };

                // What it was, in one line.
                let price = if trip.limit.is_empty() {
                    "ราคาตลาด".to_string()
                } else {
                    format!("@ {}", bot::tidy(&trip.limit, 2))
                };
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(format!("{} {}", trip.side(), bot::tidy(&trip.qty, 6)))
                            .size(13.0)
                            .strong()
                            .color(if trip.is_buy { theme::OK } else { theme::BAD }),
                    );
                    ui.label(egui::RichText::new(price).size(11.5).color(theme::MUTED));
                    ui.label(
                        egui::RichText::new(trip.kind())
                            .size(10.0)
                            .color(theme::FAINT),
                    );
                });
                ui.label(egui::RichText::new(phase.headline()).size(11.5).color(
                    if phase.is_bad() {
                        theme::BAD
                    } else if phase.is_live() {
                        theme::WARN
                    } else {
                        theme::TEXT
                    },
                ));
                ui.add_space(6.0);

                // The five steps, as a timeline down the left edge.
                let pulse = (now * 4.0).sin() as f32 * 0.5 + 0.5;
                let mut dots: Vec<(egui::Pos2, Mark)> = Vec::with_capacity(5);
                ui.spacing_mut().item_spacing.y = 3.0;
                for step in journey::steps(trip, seen, phase, now) {
                    ui.horizontal_top(|ui| {
                        let (gutter, _) =
                            ui.allocate_exact_size(egui::vec2(18.0, 15.0), Sense::hover());
                        let centre = gutter.center();
                        let colour = mark_colour(step.mark);
                        let painter = ui.painter();
                        match step.mark {
                            Mark::Done | Mark::Failed => {
                                painter.circle_filled(centre, 5.0, colour);
                            }
                            Mark::Now => {
                                painter.circle_filled(centre, 4.0, colour);
                                painter.circle_stroke(
                                    centre,
                                    5.5 + pulse * 3.0,
                                    egui::Stroke::new(1.5, colour.gamma_multiply(1.0 - pulse)),
                                );
                            }
                            Mark::Todo => {
                                painter.circle_stroke(centre, 4.5, egui::Stroke::new(1.2, colour));
                            }
                        }
                        dots.push((centre, step.mark));
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.label(egui::RichText::new(step.name).size(11.0).strong().color(
                                if step.mark == Mark::Todo {
                                    theme::FAINT
                                } else {
                                    theme::TEXT
                                },
                            ));
                            if !step.detail.is_empty() {
                                ui.label(
                                    egui::RichText::new(&step.detail)
                                        .size(9.5)
                                        .color(theme::DIM),
                                );
                            }
                        });
                    });
                }
                // The line joining the dots, drawn after them so it knows
                // where they landed; it stops short of each dot.
                for pair in dots.windows(2) {
                    let (from, _) = pair[0];
                    let (to, mark) = pair[1];
                    let colour = if mark == Mark::Todo {
                        theme::FAINT.gamma_multiply(0.5)
                    } else {
                        theme::WIRE
                    };
                    ui.painter().line_segment(
                        [from + egui::vec2(0.0, 7.0), to - egui::vec2(0.0, 7.0)],
                        egui::Stroke::new(1.5, colour),
                    );
                }

                // How much has traded, as a bar.
                if let Some(size) = seen.size(trip).filter(|size| *size > 0.0) {
                    if seen.order.is_some() || !seen.fills.is_empty() {
                        ui.add_space(6.0);
                        let done = (seen.filled_qty() / size).clamp(0.0, 1.0) as f32;
                        let (bar, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 6.0),
                            Sense::hover(),
                        );
                        ui.painter().rect_filled(bar, 3.0, theme::HEAD);
                        let mut filled = bar;
                        filled.set_width(bar.width() * done);
                        ui.painter().rect_filled(filled, 3.0, theme::OK);
                        ui.label(
                            egui::RichText::new(format!(
                                "จับคู่แล้ว {} จาก {} ({:.0}%)",
                                journey::trim(seen.filled_qty()),
                                journey::trim(size),
                                done * 100.0
                            ))
                            .size(9.5)
                            .color(theme::FAINT),
                        );
                    }
                }

                // Each fill, timed from the send - on this computer's clock.
                if !seen.fills.is_empty() {
                    let skew = journey::skew_of(trip, seen);
                    ui.add_space(4.0);
                    for fill in seen.fills.iter().take(4) {
                        let after = match (fill.when, trip.start()) {
                            (Some(when), Some(start)) => {
                                format!("+{}", journey::span(when - skew - start))
                            }
                            _ => fill.at.clone(),
                        };
                        ui.label(
                            egui::RichText::new(format!(
                                "{after} · {} @ {} · {}",
                                opt_text(&fill.size),
                                opt_text(&fill.price),
                                if fill.is_taker { "taker" } else { "maker" }
                            ))
                            .size(9.5)
                            .color(theme::MUTED),
                        );
                    }
                    if seen.fills.len() > 4 {
                        ui.label(
                            egui::RichText::new(format!("และอีก {} ไม้", seen.fills.len() - 4))
                                .size(9.0)
                                .color(theme::FAINT),
                        );
                    }
                }

                // Which order, and a way to the others.
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let small =
                        |text: &str| egui::Button::new(egui::RichText::new(text).size(9.5)).small();
                    if ui.add_enabled(*index > 0, small("‹")).clicked() {
                        pick = Some(Some(trips[index - 1].tick_at.clone()));
                    }
                    ui.label(
                        egui::RichText::new(format!("ใบที่ {} / {}", index + 1, count))
                            .size(9.5)
                            .color(theme::DIM),
                    );
                    if ui.add_enabled(index + 1 < *count, small("›")).clicked() {
                        pick = Some(Some(trips[index + 1].tick_at.clone()));
                    }
                    if ui.add_enabled(index + 1 < *count, small("ล่าสุด")).clicked() {
                        pick = Some(None);
                    }
                    if ui.add(small("ดูโค้ด tick นี้")).clicked() {
                        replay = Some(trip.tick_at.clone());
                    }
                });
            },
        );
        self.put("trip", slot);
        // Where the eye should go: while the order is still moving, the trip
        // and the stage that sent it pulse together; for a few seconds after
        // it fills, they glow green and fade, so the moment it lands is seen
        // even by someone who was looking at the code.
        if let Some((trip, _, phase, ..)) = &current {
            watch_landing(&mut self.trip_watch, trip, phase, now);
            let landed = self.trip_watch.as_ref().and_then(|watch| watch.2);
            if let Some((colour, strength)) = trip_glow(phase, landed, now) {
                let painter = ui.painter();
                for id in ["s4", "trip"] {
                    painter.rect_stroke(
                        self.slot(id).rect().expand(4.0),
                        theme::card_radius(),
                        egui::Stroke::new(2.0, colour.gamma_multiply(strength)),
                        egui::StrokeKind::Outside,
                    );
                }
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(60));
            }
        }
        if let Some(choice) = pick {
            // Stepping the trips leaves a replay of some other tick behind;
            // otherwise the replay would keep overriding the pick.
            self.trip_pick = choice;
            if self.replaying() {
                self.replay_at(None);
            }
        }
        if let Some(at) = replay {
            self.trip_pick = Some(at.clone());
            self.replay_at(Some(at));
        }
    }

    /// The five stages as they stood on the tick being shown.
    ///
    /// Worked out from the trail up to and including that tick, so a
    /// replayed order shows its signal, its approval and its receipt on the
    /// cards that walk its code - not the verdicts of whatever tick came
    /// last. The state file is the current one either way: it holds only the
    /// halt flag and the day's figures, and has no history to rewind.
    fn stages_now(&self) -> Vec<bot::Stage> {
        match self.shown() {
            Some((index, _)) if self.replaying() => {
                bot::stages_of(&self.bot.rows[..=index], self.bot.state.as_ref())
            }
            _ => self.bot.stages.clone(),
        }
    }

    /// Show a past tick, or the newest with `None`. Starts the walk again
    /// from its first step, because a replay that opens halfway through is
    /// a replay of half a tick.
    fn replay_at(&mut self, at: Option<String>) {
        let newest = self.bot.rows.last().map(|row| row.at.clone());
        self.replay = match at {
            Some(at) if Some(&at) == newest.as_ref() => None,
            other => other,
        };
        self.paused_at = None;
    }

    /// The ticks a stepper walks through: the ones with a trace when there
    /// are any, because stepping onto a tick with no code to show is a step
    /// that shows nothing.
    fn steppable(&self) -> Vec<usize> {
        let traced: Vec<usize> = self
            .bot
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.has_trace())
            .map(|(index, _)| index)
            .collect();
        if traced.is_empty() {
            (0..self.bot.rows.len()).collect()
        } else {
            traced
        }
    }

    /// One tick back (`-1`) or forward (`+1`) from the one being shown.
    fn step_tick(&mut self, direction: i32) {
        let Some((here, _)) = self.shown() else {
            return;
        };
        let ticks = self.steppable();
        let next = if direction < 0 {
            ticks.iter().rev().find(|index| **index < here)
        } else {
            ticks.iter().find(|index| **index > here)
        };
        if let Some(index) = next.copied() {
            let at = self.bot.rows[index].at.clone();
            self.replay_at(Some(at));
        }
    }

    /// The newest tick that sent an order, preferring one with a trace so
    /// there is code to watch it happen in.
    fn last_order_tick(&self) -> Option<String> {
        let orders = || self.bot.rows.iter().rev().filter(|row| row.order.is_some());
        orders()
            .find(|row| row.has_trace())
            .or_else(|| orders().next())
            .map(|row| row.at.clone())
    }

    /// The functions a stage card could show, best first, and whether the
    /// bot was seen running them.
    ///
    /// A trace is the truth and wins whenever there is one. Without it the
    /// card still has something to show - the function the config says this
    /// stage uses - because "here is the code, nothing has run yet" is a
    /// useful screen and "nothing to show" is not.
    fn calls_for(&self, stage: usize) -> (Vec<bot::Call>, bool) {
        let ran = self
            .trace()
            .map(|trace| code::candidates(stage, &trace.calls))
            .unwrap_or_default();
        if !ran.is_empty() {
            return (ran, true);
        }
        (
            self.code.planned(stage).cloned().into_iter().collect(),
            false,
        )
    }

    /// What a stage card shows of one function: a window travelling with
    /// the walk, or the top of it when nothing has run yet.
    fn strip_of(&self, call: &bot::Call, max: usize, ran: bool, now: f64) -> Strip {
        let where_at = format!("{} · {}", leaf(&call.file), call.func);
        let Some(source) = self.code.source(&code::key(call)) else {
            return Strip {
                where_at,
                lines: Vec::new(),
                ran: 0,
                total: 0,
                live: ran,
                glow: BTreeMap::new(),
                indent: 0,
            };
        };
        let hits = if ran {
            self.hits(&call.file)
        } else {
            BTreeMap::new()
        };
        let (walked, total) = source.covered(&hits);
        let walk = if ran {
            self.walk_of(call, source)
        } else {
            Walk::default()
        };
        // A window around the line being walked, so the five lines on a
        // stage card keep up with the code instead of showing the same
        // five for ever.
        let at = self.head(now, walk.steps.len());
        let here = walk.steps.get(at).copied();
        let glow = glow_of(&walk.steps, at);
        let lines = match (ran, here) {
            (true, Some(line)) => source.around(line, max, &hits),
            (true, None) => source.ran(&hits, max),
            (false, _) => source.head(max),
        };
        Strip {
            where_at,
            lines: lines
                .into_iter()
                .map(|(number, text, count)| (number, text.to_owned(), count))
                .collect(),
            ran: walked,
            total,
            live: ran,
            glow,
            indent: source.indent(),
        }
    }

    /// The steps this tick took inside one function.
    ///
    /// In the order the interpreter took them when the bot recorded that
    /// order. When it did not - a trail written before it could - the lines
    /// that ran are walked top to bottom instead, and the walk says so: the
    /// head still shows which code ran, and the label stops it being read
    /// as the order it ran in.
    fn walk_of(&self, call: &bot::Call, source: &code::Source) -> Walk {
        let Some(trace) = self.trace() else {
            return Walk::default();
        };
        let ordered = source.inside(&trace.steps_in(&call.file));
        if !ordered.is_empty() {
            return Walk {
                steps: ordered,
                ordered: true,
            };
        }
        let (first, last) = (source.first, source.last());
        Walk {
            steps: trace
                .hits(&call.file)
                .keys()
                .copied()
                .filter(|number| *number >= first && *number <= last)
                .collect(),
            ordered: false,
        }
    }

    /// Which step of a walk is lit right now.
    ///
    /// A pure function of the clock: no state, no counter to reset, and
    /// every card on the board is at the same moment of the same tick. It
    /// loops, because a tick that has finished is the last thing that
    /// happened and watching it again is how a person reads it.
    fn head(&self, now: f64, steps: usize) -> usize {
        if steps == 0 {
            return 0;
        }
        let at = self.paused_at.unwrap_or(now);
        let since = (at - self.seen_at).max(0.0);
        ((since * self.walk_rate) as usize) % steps
    }

    /// Step to the next walking speed, keeping the head on the line it is on.
    ///
    /// The head is worked out from the clock, so changing the rate alone
    /// would throw it to a different line. Moving the start of the clock by
    /// the same proportion keeps it where it was: the walk carries on from
    /// here at the new pace instead of jumping.
    fn next_speed(&mut self, now: f64) {
        let index = WALK_RATES
            .iter()
            .position(|(rate, _)| *rate == self.walk_rate)
            .unwrap_or(DEFAULT_RATE);
        let next = WALK_RATES[(index + 1) % WALK_RATES.len()].0;
        let at = self.paused_at.unwrap_or(now);
        let walked = (at - self.seen_at).max(0.0) * self.walk_rate;
        self.seen_at = at - walked / next;
        self.walk_rate = next;
    }

    /// Is any card walking? The board repaints faster while one is.
    fn walking(&self) -> bool {
        self.paused_at.is_none()
            && self
                .trace()
                .map(|trace| !trace.path.is_empty() || !trace.lines.is_empty())
                .unwrap_or(false)
    }

    /// Why nothing is lit, in enough detail for the person to act on it.
    fn why_quiet(&self) -> Quiet {
        if self.bot.rows.is_empty() {
            return Quiet::NeverRan;
        }
        // The trace is on its way: a few milliseconds of "loading" is
        // honest, where a flash of "not reached" would not be.
        if let Some((_, row)) = self.shown() {
            if row.trace.is_none() && row.traced && self.traces.get(&row.at).is_none() {
                // Asked and refused is not the same as still asking: a
                // trace rolled out of the file is never coming.
                if self.traces.missing(&row.at) {
                    return Quiet::TraceGone;
                }
                return Quiet::Loading;
            }
        }
        // The bot recorded this tick; this stage's code simply was not part
        // of it. A tick with no signal never reaches the gate, and that is
        // the lesson the card should be teaching - not a complaint that the
        // bot forgot to write something down.
        if self.trace().is_some() {
            return Quiet::NotReached;
        }
        if self.code.from_bot() {
            Quiet::OldBot
        } else {
            Quiet::OldBotGuessedFile
        }
    }

    /// Which lines of one file the last tick ran, and how often.
    fn hits(&self, file: &str) -> BTreeMap<usize, u64> {
        self.trace()
            .map(|trace| trace.hits(file))
            .unwrap_or_default()
    }

    /// Remember when the newest audit row first appeared.
    ///
    /// The row carries the bot's clock; this carries the board's. Comparing
    /// the two would mean trusting that two machines agree about the time,
    /// and all this needs to know is how long ago it saw the row change.
    fn mark_tick(&mut self, now: f64) {
        // The tick on the cards, which is the newest unless one is being
        // replayed: a fresh tick arriving mid-replay must not yank the head
        // back to the start of the tick a person is watching.
        let latest = self
            .shown()
            .map(|(_, row)| row.at.clone())
            .unwrap_or_default();
        if latest != self.seen_tick {
            self.seen_tick = latest;
            self.seen_at = now;
        }
    }

    /// How fresh the last tick is: 1.0 the moment it lands, falling to 0.0.
    ///
    /// The lines that ran are drawn brighter by this much. Without it a bot
    /// that stopped an hour ago and a bot that is working look exactly the
    /// same, which is the thing these cards exist to tell apart.
    fn flash(&self, now: f64) -> f32 {
        if self.seen_tick.is_empty() {
            return 0.0;
        }
        let age = now - self.seen_at;
        if age < 0.0 || age >= FLASH {
            return 0.0;
        }
        (1.0 - age / FLASH) as f32
    }

    /// One stage of the pipeline, as its own card.
    ///
    /// Five small cards joined by wires rather than five rows in one box:
    /// the point of the picture is that a decision *travels*, and that it
    /// only ever gets as far as the stage that stopped it.
    fn stage_card(&mut self, ui: &mut egui::Ui, index: usize, id: &'static str, now: f64) {
        let mut slot = self.slot(id);
        let mut picked = false;
        let has = self.bot.has_live();
        let stage = self.stages_now().get(index).cloned();
        let (label, sub, state, metric, detail) = match &stage {
            Some(stage) if has => (
                stage.label,
                stage.sub,
                stage.state,
                stage.metric.clone(),
                stage.detail.clone(),
            ),
            Some(stage) => (
                stage.label,
                stage.sub,
                theme::State::Idle,
                String::new(),
                "ยังไม่เริ่ม".to_string(),
            ),
            None => ("", "", theme::State::Idle, String::new(), String::new()),
        };
        // The send stage ends where the exchange says it ends: once the bot
        // has sent something, this card says how far it has got.
        let detail = match (index, self.trip_now()) {
            (3, Some((trip, seen, phase, ..))) if has => {
                let t = journey::times(&trip, &seen, &phase, unix_now());
                match t.total {
                    Some(total) => format!("{} · {}", phase.headline(), journey::span(total)),
                    None => phase.headline(),
                }
            }
            _ => detail,
        };
        let title = format!("{}. {}", index + 1, label);
        // A mark price carries twenty digits of Decimal; a chip has room for
        // about ten. Cut it rather than let it push the header apart.
        let status = short(&metric, 11);
        let (state, status) = stale_if_failed(&self.bot.audit_load.clone(), state, status);

        // The code this stage ran on the last tick. Asked for here rather
        // than in the body closure, because the closure cannot borrow the
        // board again while the card holds it.
        let (calls, ran) = self.calls_for(index);
        let call = calls.into_iter().next();
        if let Some(call) = &call {
            self.code.want(&code::key(call), now);
        }
        let strip = call.as_ref().map(|call| self.strip_of(call, 5, ran, now));
        let waiting = call
            .as_ref()
            .map(|call| self.code.load(&code::key(call)))
            .unwrap_or(Load::Idle);
        let flash = if ran { self.flash(now) } else { 0.0 };
        let quiet = self.why_quiet();
        let reading = self.code_stage == index;

        card::show_with_tools(
            ui,
            &mut slot,
            &title,
            &status,
            state,
            |ui| {
                // The button is what joins the five small cards to the one
                // wide one: pressing it on any stage sends that stage's code
                // to the panel below.
                let colour = if reading { theme::ACCENT } else { theme::DIM };
                let button = egui::Button::new(egui::RichText::new("โค้ด").size(9.5).color(colour))
                    .fill(if reading {
                        theme::ACCENT.gamma_multiply(0.16)
                    } else {
                        theme::HEAD
                    })
                    .small();
                if ui.add(button).clicked() {
                    picked = true;
                }
            },
            |ui| {
                ui.label(egui::RichText::new(sub).size(10.0).color(theme::FAINT));
                ui.add_space(3.0);
                ui.label(egui::RichText::new(detail).size(10.5).color(
                    if state == theme::State::Bad {
                        theme::BAD
                    } else {
                        theme::DIM
                    },
                ));
                ui.add_space(5.0);
                Self::code_strip(ui, strip, &waiting, flash, quiet);
            },
        );
        self.put(id, slot);
        if picked {
            self.code_stage = index;
            self.code_pick = 0;
        }
    }

    /// The few lines of real code a stage card has room for.
    ///
    /// Five lines of Python is not enough to follow a function, and it is
    /// more than enough to see that one is running: the point of the strip
    /// is the difference between "this card is a number" and "this card is a
    /// program that just did something".
    fn code_strip(
        ui: &mut egui::Ui,
        strip: Option<Strip>,
        waiting: &Load,
        flash: f32,
        quiet: Quiet,
    ) {
        let Some(strip) = strip else {
            ui.label(
                egui::RichText::new("ยังไม่รู้ว่าขั้นนี้ใช้ไฟล์ไหน")
                    .size(9.5)
                    .color(theme::FAINT),
            );
            return;
        };
        ui.label(
            egui::RichText::new(format!("{}()", strip.where_at))
                .size(9.0)
                .color(theme::FAINT),
        );
        if strip.lines.is_empty() {
            let reason = match waiting {
                Load::Missing(reason) | Load::Failed(reason) => short(reason, 34),
                _ => "กำลังเปิดไฟล์โค้ด".to_string(),
            };
            ui.label(egui::RichText::new(reason).size(9.5).color(theme::FAINT));
            return;
        }
        ui.spacing_mut().item_spacing.y = 0.0;
        for (number, text, hits) in &strip.lines {
            // The indentation every line of the function shares says
            // nothing, and on a card this narrow it is the difference
            // between reading `return None` and reading `retur`.
            let text = text.get(strip.indent.min(text.len())..).unwrap_or(text);
            code_line(
                ui,
                Shown {
                    number: *number,
                    text,
                    hits: *hits,
                    glow: strip.glow.get(number).copied().unwrap_or(0.0),
                },
                flash,
                9.0,
                24.0,
            );
        }
        ui.add_space(2.0);
        // Two different sentences on purpose: one is a measurement of the
        // last tick, the other is a statement about which file this is.
        let note = if strip.live {
            format!("เดิน {}/{} บรรทัด", strip.ran, strip.total)
        } else {
            format!("{} บรรทัด · {}", strip.total, quiet.short())
        };
        ui.label(egui::RichText::new(note).size(9.0).color(if strip.live {
            theme::FAINT
        } else {
            theme::WARN
        }));
    }

    /// The whole of the function a stage ran, with the lines that ran lit up.
    ///
    /// The five stage cards each show five lines, which answers "is it
    /// moving". This one is wide enough to answer "what does it do" - the
    /// same question a person would open the file to answer, except that
    /// here the answer has the last tick drawn on it.
    fn code_card(&mut self, ui: &mut egui::Ui, now: f64) {
        let mut slot = self.slot("code");
        let (calls, ran) = self.calls_for(self.code_stage);
        if self.code_pick >= calls.len() {
            self.code_pick = 0;
        }
        let call = calls.get(self.code_pick).cloned();
        if let Some(call) = &call {
            self.code.want(&code::key(call), now);
        }
        let source = call
            .as_ref()
            .and_then(|call| self.code.source(&code::key(call)).cloned());
        let hits = match (&call, ran) {
            (Some(call), true) => self.hits(&call.file),
            _ => BTreeMap::new(),
        };
        let load = call
            .as_ref()
            .map(|call| self.code.load(&code::key(call)))
            .unwrap_or(Load::Idle);
        let flash = if ran { self.flash(now) } else { 0.0 };
        let quiet = self.why_quiet();
        let label = self
            .bot
            .stages
            .get(self.code_stage)
            .map(|stage| stage.label)
            .unwrap_or("");
        let title = match self.shown() {
            // Short: in a replay the header also carries the buttons for
            // the stage, the pause and the speed, and a long title pushes
            // the last of them off the card.
            Some((_, row)) if self.replaying() => format!(
                "tick {} · {}. {}",
                bot::clock(&row.at),
                self.code_stage + 1,
                label
            ),
            _ => format!("โค้ดที่เพิ่งเดิน · {}. {}", self.code_stage + 1, label),
        };
        let walk = match (&call, &source, ran) {
            (Some(call), Some(source), true) => self.walk_of(call, source),
            _ => Walk::default(),
        };
        let steps = walk.steps.clone();
        let at = self.head(now, steps.len());
        let walking = steps.get(at).copied();
        let glow = glow_of(&steps, at);
        let paused = self.paused_at.is_some();
        // Follow the head down the page while it walks. Paused, the page is
        // the reader's: they stopped it to read something, and dragging the
        // view back to the head would take that away.
        let follow = !paused;
        let mut toggle = false;
        let mut faster = false;
        let rate_now = self.walk_rate;
        let (state, status) = match (&source, ran) {
            (Some(source), true) => {
                let (walked, total) = source.covered(&hits);
                let where_at = match (walking, steps.len(), walk.ordered) {
                    (Some(line), count, true) if count > 0 => {
                        format!("บรรทัด {line} · {}/{count} ก้าว", at + 1)
                    }
                    // Not the order it ran in, and the chip says so: a
                    // reader who took this for the real path would learn a
                    // control flow the code does not have.
                    (Some(line), count, false) if count > 0 => {
                        format!("บรรทัด {line} · บนลงล่าง {}/{count}", at + 1)
                    }
                    _ => format!("เดิน {walked}/{total} บรรทัด"),
                };
                (
                    if walked > 0 {
                        theme::State::Ok
                    } else {
                        theme::State::Idle
                    },
                    where_at,
                )
            }
            (Some(source), false) => (
                theme::State::Idle,
                format!("{} บรรทัด · {}", source.lines.len(), quiet.short()),
            ),
            (None, _) => (theme::State::Idle, "ยังไม่รู้ว่าใช้ไฟล์ไหน".to_string()),
        };

        // Scroll to the code that ran, but only when the card changes which
        // function it is showing. Doing it whenever the lit lines move would
        // yank the page out from under anyone reading a branch that the next
        // tick did not take.
        let here = call.as_ref().map(|call| (call.file.clone(), call.line));
        let jump = here != self.code_at;
        let entered = source.as_ref().and_then(|source| source.entered(&hits));
        let picks = calls.len();
        let stage_now = self.code_stage;
        let pick_now = self.code_pick;
        let mut want_stage: Option<usize> = None;
        let mut want_pick: Option<usize> = None;

        card::show_with_tools(
            ui,
            &mut slot,
            &title,
            &status,
            state,
            |ui| {
                // Five numbers, one per stage, in the same order as the row
                // of cards above: the link between them is then something
                // you can see rather than something you have to be told.
                for index in 0..STAGES.len() {
                    let on = index == stage_now;
                    let button = egui::Button::new(
                        egui::RichText::new(format!("{}", index + 1))
                            .size(9.5)
                            .color(if on { theme::ACCENT } else { theme::DIM }),
                    )
                    .fill(if on {
                        theme::ACCENT.gamma_multiply(0.16)
                    } else {
                        theme::HEAD
                    })
                    .small();
                    if ui.add(button).clicked() {
                        want_stage = Some(index);
                    }
                }
                // Stop the replay to read a branch, start it to watch
                // the tick again. Not a mode: it changes nothing but the
                // clock the head is asked about.
                if !steps.is_empty() {
                    ui.add_space(6.0);
                    let label = if paused {
                        "เดิน"
                    } else {
                        "หยุด"
                    };
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new(label).size(9.5).color(theme::ACCENT),
                            )
                            .fill(theme::ACCENT.gamma_multiply(0.16))
                            .small(),
                        )
                        .clicked()
                    {
                        toggle = true;
                    }
                    // How fast it walks: slow enough to talk over, fast
                    // enough to see the shape of a tick at a glance.
                    let speed = WALK_RATES
                        .iter()
                        .find(|(rate, _)| *rate == rate_now)
                        .map(|(_, word)| *word)
                        .unwrap_or("ปกติ");
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new(speed).size(9.5).color(theme::DIM),
                            )
                            .fill(theme::HEAD)
                            .small(),
                        )
                        .on_hover_text("ความเร็วที่หัวอ่านเดิน: ช้า / ปกติ / เร็ว")
                        .clicked()
                    {
                        faster = true;
                    }
                }
                // One stage can run several functions - a broker reads the
                // price and the equity with two of them - so the card says
                // which of them this is, and lets you step through.
                if picks > 1 {
                    ui.add_space(6.0);
                    if ui
                        .add(egui::Button::new(egui::RichText::new("‹").size(10.0)).small())
                        .clicked()
                    {
                        want_pick = Some((pick_now + picks - 1) % picks);
                    }
                    ui.label(
                        egui::RichText::new(format!("{}/{}", pick_now + 1, picks))
                            .size(9.0)
                            .color(theme::DIM),
                    );
                    if ui
                        .add(egui::Button::new(egui::RichText::new("›").size(10.0)).small())
                        .clicked()
                    {
                        want_pick = Some((pick_now + 1) % picks);
                    }
                }
            },
            |ui| {
                let Some(source) = source else {
                    let reason = match &load {
                        Load::Missing(reason) | Load::Failed(reason) => reason.clone(),
                        _ => NO_FILE.to_string(),
                    };
                    ui.label(egui::RichText::new(reason).size(10.5).color(theme::DIM));
                    return;
                };
                if !ran {
                    ui.label(
                        egui::RichText::new(quiet.long())
                            .size(9.5)
                            .color(theme::WARN)
                            .italics(),
                    );
                } else if !walk.ordered && !walk.steps.is_empty() {
                    ui.label(
                        egui::RichText::new(UNORDERED)
                            .size(9.5)
                            .color(theme::WARN)
                            .italics(),
                    );
                }
                ui.label(
                    egui::RichText::new(format!(
                        "{} · {}()  บรรทัด {}-{}",
                        source.file,
                        source.func,
                        source.first,
                        source.last()
                    ))
                    .size(9.5)
                    .color(theme::FAINT),
                );
                ui.add_space(3.0);
                ui.spacing_mut().item_spacing.y = 0.0;
                // Where the scroll should be, worked out rather than asked
                // of egui. `scroll_to_rect` inside a zoomed Scene measured
                // the line in the wrong space and parked the view ten lines
                // below the head - so the one card wide enough to read was
                // the one card where the highlight could not be seen.
                let viewport = ui.available_height();
                let target = match (walking, follow) {
                    (Some(line), true) => Some(line),
                    _ if jump => entered,
                    _ => None,
                };
                let mut area = egui::ScrollArea::vertical()
                    .id_salt("code")
                    .auto_shrink([false, false]);
                if let Some(line) = target {
                    area = area.vertical_scroll_offset(follow_offset(
                        line.saturating_sub(source.first),
                        CODE_ROW,
                        viewport,
                    ));
                }
                let dedent = source.indent();
                area.show(ui, |ui| {
                    for (number, text, count) in source.walk(&hits) {
                        let text = text.get(dedent.min(text.len())..).unwrap_or(text);
                        code_line(
                            ui,
                            Shown {
                                number,
                                text,
                                hits: count,
                                glow: glow.get(&number).copied().unwrap_or(0.0),
                            },
                            flash,
                            CODE_SIZE,
                            34.0,
                        );
                    }
                });
            },
        );
        self.put("code", slot);
        self.code_at = here;
        if faster {
            self.next_speed(now);
        }
        if toggle {
            self.paused_at = match self.paused_at {
                Some(_) => None,
                // Freeze the reading the head is taken from, not the wall
                // clock: starting again must carry on from here rather than
                // jump to wherever the tick would have got to by now.
                None => Some(now),
            };
        }
        if let Some(index) = want_stage {
            self.code_stage = index;
            self.code_pick = 0;
        }
        if let Some(index) = want_pick {
            self.code_pick = index;
        }
    }

    /// The funding the account was really charged, payment by payment.
    ///
    /// The market zone shows the funding *rate*; this shows what that rate
    /// cost, which is a different thing for anyone who never holds across a
    /// funding time - and the card says so, because an empty list here is a
    /// lesson (short holds pay no funding) rather than a missing feature.
    fn paid_funding_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("paid_funding");
        let rows = self.account_data.funding.clone();
        let load = self.account_data.load_of(Acc::Funding).clone();
        let net: f64 = rows.iter().filter_map(|row| row.effect()).sum();
        let state = if rows.is_empty() {
            theme::State::Idle
        } else if net < 0.0 {
            theme::State::Bad
        } else {
            theme::State::Ok
        };
        let status = if rows.is_empty() {
            "ยังไม่เคยถูกเก็บ".to_string()
        } else {
            format!("{} ครั้ง · สุทธิ {}", rows.len(), money(net))
        };
        let (state, status) = stale_if_failed(&load, state, status);
        card::show(
            ui,
            &mut slot,
            "ค่าถือข้ามคืนที่ถูกเก็บจริง · จากตลาด",
            &status,
            state,
            |ui| {
                if rows.is_empty() {
                    if matches!(load, Load::Ready) {
                        ui.label(
                            egui::RichText::new(FUNDING_NONE)
                                .size(10.5)
                                .color(theme::DIM),
                        );
                    } else {
                        Self::no_account(ui, &load);
                    }
                    return;
                }
                columns(ui, "เวลา", "สัญญา", "ผลต่อบัญชี");
                ui.spacing_mut().item_spacing.y = 0.0;
                egui::ScrollArea::vertical()
                    .id_salt("paid_funding")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for row in &rows {
                            let effect = row.effect();
                            let shown = match effect {
                                Some(value) if value < 0.0 => {
                                    format!("จ่าย {} {}", money(value), row.currency)
                                }
                                Some(value) => format!("ได้รับ {} {}", money(value), row.currency),
                                None => "-".to_string(),
                            };
                            trade_row(ui, &row.at, &row.instrument, &shown, colour_of(effect));
                        }
                    });
            },
        );
        self.put("paid_funding", slot);
    }

    /// Every run the bot has made, newest first.
    ///
    /// A run is the unit a person thinks in - "that time I left it on over
    /// lunch" - and the audit trail has no idea where one ended and the next
    /// began. This is the only card that can answer "what has this bot
    /// actually done", as opposed to "what is it doing".
    fn runs_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("runs");
        let runs = self.past.runs.clone();
        let load = self.past.runs_load.clone();
        let state = if runs.iter().any(|run| run.halted) {
            theme::State::Bad
        } else if runs.is_empty() {
            theme::State::Idle
        } else {
            theme::State::Ok
        };
        let status = if runs.is_empty() {
            "ยังไม่มี".to_string()
        } else {
            format!("{} รอบ", runs.len())
        };
        let shown_at = self.shown().map(|(_, row)| row.at.clone());
        let note = self.run_note.clone();
        let mut chosen: Option<usize> = None;
        card::show(
            ui,
            &mut slot,
            "ประวัติการรัน",
            &status,
            state,
            |ui| {
                if runs.is_empty() {
                    let reason = match &load {
                        Load::Missing(reason) | Load::Failed(reason) => reason.clone(),
                        _ => "กำลังอ่านสมุดรอบการรัน".to_string(),
                    };
                    ui.label(egui::RichText::new(reason).size(10.5).color(theme::DIM));
                    return;
                }
                if let Some(note) = &note {
                    ui.label(egui::RichText::new(note).size(10.0).color(theme::WARN));
                }
                columns(ui, "เริ่ม · นาน", "กลยุทธ์ · ที่ไหน", "ผล");
                ui.spacing_mut().item_spacing.y = 0.0;
                egui::ScrollArea::vertical()
                    .id_salt("runs")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (index, run) in runs.iter().enumerate() {
                            let here = shown_at
                                .as_deref()
                                .map(|at| run.covers(at))
                                .unwrap_or(false);
                            if run_row(ui, run, here)
                                .on_hover_text("กดเพื่อไปดูรอบนี้ที่โซนบอท")
                                .clicked()
                            {
                                chosen = Some(index);
                            }
                        }
                    });
            },
        );
        self.put("runs", slot);
        if let Some(index) = chosen {
            self.open_run(&runs[index]);
        }
    }

    /// Go and look at one run: the pipeline, replaying its most telling tick.
    ///
    /// The newest tick of the run that sent an order and has code to show,
    /// failing that the newest with code, failing that the newest at all -
    /// so pressing a run that traded shows the trade, and pressing one that
    /// did not shows what it was doing instead. A run older than the trail
    /// the board holds has no ticks here, and the board says so rather than
    /// replaying something else.
    fn open_run(&mut self, run: &history::Run) {
        let inside: Vec<&bot::Row> = self
            .bot
            .rows
            .iter()
            .filter(|row| run.covers(&row.at))
            .collect();
        let pick = inside
            .iter()
            .rev()
            .find(|row| row.order.is_some() && row.has_trace())
            .or_else(|| inside.iter().rev().find(|row| row.has_trace()))
            .or_else(|| inside.last())
            .map(|row| row.at.clone());
        self.run_note = match &pick {
            Some(_) => None,
            None => Some(format!(
                "รอบ {} เก่ากว่า {} tick ที่บอร์ดถืออยู่",
                history::Run::clock(&run.started_at),
                self.bot.rows.len()
            )),
        };
        if pick.is_some() {
            self.scene_rect = view(1);
            self.replay_at(pick);
        }
    }

    /// A month of days, each square coloured by what the account did.
    ///
    /// The squares answer a question the live cards never can: not "how is
    /// it doing" but "how has it been doing", which is the question that
    /// decides whether a strategy stays switched on.
    fn calendar_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("calendar");
        let days = self.past.days.clone();
        let load = self.past.days_load.clone();
        let newest = days.last().cloned();
        let (year, month) = match newest.as_ref().and_then(|day| day.parts()) {
            Some((year, month, _)) => (year, month),
            None => (0, 0),
        };
        let title = if month == 0 {
            "ปฏิทินรายวัน".to_string()
        } else {
            format!(
                "ปฏิทินรายวัน · {} {}",
                history::MONTHS[(month - 1) as usize],
                year
            )
        };
        let state = match newest.as_ref().and_then(|day| day.change) {
            Some(change) if change > 0.0 => theme::State::Ok,
            Some(change) if change < 0.0 => theme::State::Bad,
            _ => theme::State::Idle,
        };
        let status = match &newest {
            Some(day) => format!("{} tick", day.ticks),
            None => "ยังไม่มี".to_string(),
        };
        card::show(ui, &mut slot, &title, &status, state, |ui| {
            if month == 0 {
                let reason = match &load {
                    Load::Missing(reason) | Load::Failed(reason) => reason.clone(),
                    _ => "กำลังอ่านสมุดรายวัน".to_string(),
                };
                ui.label(egui::RichText::new(reason).size(10.5).color(theme::DIM));
                return;
            }
            let today = newest
                .as_ref()
                .map(|day| day.date.clone())
                .unwrap_or_default();
            month_grid(ui, year, month, &days, &today);
            ui.add_space(6.0);
            if let Some(day) = &newest {
                let change = match day.change {
                    Some(change) => money(change),
                    None => "วัดไม่ได้".to_string(),
                };
                card::kv(
                    ui,
                    &format!("วันล่าสุด {}", day.date),
                    &format!("{} ออเดอร์ · {}", day.orders, change),
                    colour_of(day.change),
                );
                card::kv(
                    ui,
                    "ทุนต้นวัน -> ท้ายวัน",
                    &format!(
                        "{} -> {}",
                        short(&day.first_equity, 10),
                        short(&day.last_equity, 10)
                    ),
                    theme::MUTED,
                );
            }
        });
        self.put("calendar", slot);
    }

    /// Only the ticks where the bot did something, newest first.
    fn log_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("log");
        let log = self.bot.log.clone();
        let has = self.bot.has_live();
        let load = self.bot.audit_load.clone();
        let unreadable = self.bot.unreadable;
        let state = if !has {
            theme::State::Idle
        } else if log.iter().any(|entry| entry.kind == Kind::Error) {
            theme::State::Bad
        } else if log.is_empty() {
            theme::State::Idle
        } else {
            theme::State::Wip
        };
        let status = format!("{} เรื่อง", log.len());
        let (state, status) = stale_if_failed(&load, state, status);
        let shown_at = self.shown().map(|(_, row)| row.at.clone());
        let replaying = self.replaying();
        let order_tick = self.last_order_tick();
        // Two closures, two answers: the buttons in the header and the
        // entries in the body are drawn by different closures, and each
        // needs somewhere of its own to write which tick was chosen.
        let mut pick: Option<Option<String>> = None;
        let mut chosen: Option<String> = None;
        let mut step = 0;
        card::show_with_tools(
            ui,
            &mut slot,
            "สิ่งที่บอทตัดสินใจ",
            &status,
            state,
            |ui| {
                // The four ways to choose a tick, on the list of ticks: the
                // newest, the newest that sent an order, and one either side
                // of the tick on the cards now.
                let small = |text: &str, lit: bool| {
                    egui::Button::new(egui::RichText::new(text).size(9.5).color(if lit {
                        theme::ACCENT
                    } else {
                        theme::DIM
                    }))
                    .fill(if lit {
                        theme::ACCENT.gamma_multiply(0.16)
                    } else {
                        theme::HEAD
                    })
                    .small()
                };
                if ui.add(small("ล่าสุด", !replaying)).clicked() {
                    pick = Some(None);
                }
                let can_order = order_tick.is_some();
                if ui
                    .add_enabled(can_order, small("ออเดอร์ล่าสุด", false))
                    .on_hover_text("เล่นซ้ำ tick ล่าสุดที่บอทส่งออเดอร์")
                    .clicked()
                {
                    pick = Some(order_tick.clone());
                }
                if ui
                    .add(small("‹", false))
                    .on_hover_text("tick ก่อนหน้า")
                    .clicked()
                {
                    step = -1;
                }
                if ui
                    .add(small("›", false))
                    .on_hover_text("tick ถัดไป")
                    .clicked()
                {
                    step = 1;
                }
            },
            |ui| {
                if !has {
                    waiting(ui, &load, "tradebot --dry-run");
                    return;
                }
                if log.is_empty() {
                    ui.label(
                        egui::RichText::new("บอทดูตลาดอยู่ แต่ยังไม่ได้ทำอะไรเลย ซึ่งเป็นเรื่องปกติ")
                            .size(11.5)
                            .color(theme::MUTED),
                    );
                    return;
                }
                egui::ScrollArea::vertical()
                    .id_salt("log")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if unreadable > 0 {
                            ui.label(
                                egui::RichText::new(format!("{unreadable} บรรทัดอ่านไม่ออก ข้ามไป"))
                                    .size(10.0)
                                    .color(theme::WARN),
                            );
                        }
                        for entry in &log {
                            // Each entry is a tick: press it and the five
                            // cards replay that tick, the way an execution
                            // opens in n8n.
                            let on_it = shown_at.as_deref() == Some(entry.at.as_str());
                            let response = ui
                                .scope(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(bot::clock(&entry.at))
                                                .size(10.0)
                                                .monospace()
                                                .color(if on_it {
                                                    theme::ACCENT
                                                } else {
                                                    theme::FAINT
                                                }),
                                        );
                                        ui.label(
                                            egui::RichText::new(&entry.title)
                                                .size(11.5)
                                                .color(entry.kind.colour()),
                                        );
                                    });
                                    if !entry.text.is_empty() {
                                        ui.label(
                                            egui::RichText::new(&entry.text)
                                                .size(10.5)
                                                .color(theme::MUTED),
                                        );
                                    }
                                })
                                .response;
                            let area = response.rect.expand2(egui::vec2(4.0, 2.0));
                            let click = ui.interact(
                                area,
                                ui.scope_id().with(("log-entry", &entry.at)),
                                Sense::click(),
                            );
                            if on_it {
                                ui.painter().rect_stroke(
                                    area,
                                    3,
                                    egui::Stroke::new(1.0, theme::ACCENT),
                                    egui::StrokeKind::Inside,
                                );
                            } else if click.hovered() {
                                ui.painter().rect_filled(
                                    area,
                                    3,
                                    theme::ACCENT.gamma_multiply(0.06),
                                );
                            }
                            if click.on_hover_text("กดเพื่อเล่นซ้ำ tick นี้").clicked()
                            {
                                chosen = Some(entry.at.clone());
                            }
                            ui.add_space(5.0);
                        }
                    });
            },
        );
        self.put("log", slot);
        if let Some(at) = chosen {
            self.replay_at(Some(at));
        } else if let Some(at) = pick {
            self.replay_at(at);
        }
        if step != 0 {
            self.step_tick(step);
        }
    }

    /// What a backtest earned, and whether doing nothing would have done
    /// better. The second question is the one people skip.
    fn backtest_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("backtest");
        let summary = self.bot.summary.clone();
        let load = self.bot.back_load.clone();
        let state = if summary.is_some() {
            // Simulated, not good or bad: the colours inside the card are
            // where the money gets a verdict.
            theme::State::Simulated
        } else {
            theme::State::Idle
        };
        let status = match &summary {
            Some(view) => format!("{} ไม้", view.trades),
            None => "ยังไม่ได้ทดสอบ".to_string(),
        };
        card::show(
            ui,
            &mut slot,
            "ผลการทดสอบย้อนหลัง",
            &status,
            state,
            |ui| {
                let Some(view) = summary else {
                    waiting(ui, &load, "tradebot --backtest --days 730 --trades");
                    return;
                };
                egui::ScrollArea::vertical()
                    .id_salt("backtest")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} · {} · {}",
                                view.symbol, view.interval, view.strategy
                            ))
                            .size(11.5)
                            .color(theme::TEXT),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "{} แท่ง · {} ถึง {}",
                                view.bars,
                                day_of(&view.start),
                                day_of(&view.end)
                            ))
                            .size(10.0)
                            .color(theme::FAINT),
                        );
                        ui.add_space(6.0);

                        card::kv(
                            ui,
                            "กำไรสุทธิ",
                            &bot::signed(view.net_pnl),
                            bot::pnl_colour(view.net_pnl),
                        );
                        card::kv(
                            ui,
                            "เป็นกี่ %",
                            &bot::pct(view.return_pct),
                            bot::pnl_colour(view.return_pct),
                        );
                        card::kv(
                            ui,
                            "ต่อปี (CAGR)",
                            &bot::opt_pct(view.cagr_pct),
                            bot::pnl_colour(view.cagr_pct.unwrap_or(0.0)),
                        );
                        card::kv(
                            ui,
                            "ทุนต้น / ทุนปลาย",
                            &format!(
                                "{} / {}",
                                bot::money(view.start_equity),
                                bot::money(view.final_equity)
                            ),
                            theme::MUTED,
                        );
                        card::kv(
                            ui,
                            "ค่าธรรมเนียมที่จ่าย",
                            &bot::money(view.fees_paid),
                            theme::WARN,
                        );

                        ui.add_space(8.0);
                        section(ui, "เทียบกับการอยู่เฉย");
                        card::kv(ui, "บอท", &bot::pct(view.return_pct), theme::MUTED);
                        card::kv(ui, "ซื้อแล้วถือ", &bot::opt_pct(view.hold_pct), theme::MUTED);
                        let (verdict, colour) = match view.winner {
                            None => ("ไม่มีราคาเทียบ จึงบอกไม่ได้", theme::DIM),
                            Some(Winner::Bot) => ("บอทชนะการอยู่เฉย", theme::OK),
                            // A draw goes to doing nothing: it costs no fees and
                            // no attention, and it cannot break.
                            Some(Winner::Hold) => ("การอยู่เฉยชนะบอท", theme::BAD),
                        };
                        ui.label(egui::RichText::new(verdict).size(11.5).color(colour));

                        if !view.no_trades.is_empty() {
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new(format!("ไม่มีไม้เลย: {}", view.no_trades))
                                    .size(11.0)
                                    .color(theme::WARN),
                            );
                        }
                        if view.rests_on_two {
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "ตัดไม้ดีสุดสองไม้ออกแล้วเหลือ {} — ผลนี้ยืนบนโชคสองครั้ง",
                                    bot::signed(view.without_best_two)
                                ))
                                .size(11.0)
                                .color(theme::WARN),
                            );
                        }
                    });
            },
        );
        self.put("backtest", slot);
    }

    /// What the run would have felt like to sit through, and whether the
    /// trades were any good.
    fn quality_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("quality");
        let summary = self.bot.summary.clone();
        let load = self.bot.back_load.clone();
        let state = if summary.is_some() {
            theme::State::Simulated
        } else {
            theme::State::Idle
        };
        let status = match &summary {
            Some(view) => format!("ลึกสุด {:.2}%", view.max_drawdown_pct),
            None => "ยังไม่ได้ทดสอบ".to_string(),
        };
        card::show(
            ui,
            &mut slot,
            "ความเสี่ยงกับคุณภาพไม้",
            &status,
            state,
            |ui| {
                let Some(view) = summary else {
                    waiting(ui, &load, "tradebot --backtest --days 730 --trades");
                    return;
                };
                egui::ScrollArea::vertical()
                    .id_salt("quality")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        section(ui, "ความเสี่ยงที่ต้องทน");
                        card::kv(
                            ui,
                            "ขาดทุนจากพีคลึกสุด",
                            &format!(
                                "{} ({:.2}%)",
                                bot::money(view.max_drawdown),
                                view.max_drawdown_pct
                            ),
                            theme::BAD,
                        );
                        card::kv(
                            ui,
                            "เวลาที่อยู่ใต้พีค",
                            &format!("{:.1} วัน", view.days_underwater),
                            theme::WARN,
                        );
                        match &view.worst_month {
                            Some((month, value)) => card::kv(
                                ui,
                                "เดือนแย่สุด",
                                &format!("{month} {}", bot::pct(*value)),
                                bot::pnl_colour(*value),
                            ),
                            None => card::kv(ui, "เดือนแย่สุด", bot::UNMEASURED, theme::DIM),
                        }
                        card::kv(
                            ui,
                            "แพ้ติดกันยาวสุด",
                            &format!("{} ไม้", view.losing_streak),
                            theme::MUTED,
                        );
                        card::kv(
                            ui,
                            "เวลาที่ถือของอยู่",
                            &format!("{:.1}%", view.exposure_pct),
                            theme::MUTED,
                        );

                        ui.add_space(8.0);
                        section(ui, "คุณภาพของไม้");
                        card::kv(
                            ui,
                            "จำนวนไม้",
                            &format!("{} (ชนะ {} แพ้ {})", view.trades, view.wins, view.losses),
                            theme::MUTED,
                        );
                        card::kv(
                            ui,
                            "อัตราชนะ",
                            &format!("{:.1}%", view.win_rate_pct),
                            theme::MUTED,
                        );
                        card::kv(
                            ui,
                            "ชนะเฉลี่ย ÷ แพ้เฉลี่ย",
                            &bot::opt(view.win_loss_ratio, 2),
                            theme::MUTED,
                        );
                        card::kv(
                            ui,
                            "ได้เปรียบต่อไม้ (R)",
                            &bot::opt(Some(view.expectancy_r), 3),
                            bot::pnl_colour(view.expectancy_r),
                        );
                        card::kv(
                            ui,
                            "profit factor",
                            &bot::opt(view.profit_factor, 2),
                            theme::MUTED,
                        );
                        card::kv(
                            ui,
                            "sharpe / sortino",
                            &format!(
                                "{} / {}",
                                bot::opt(view.sharpe, 2),
                                bot::opt(view.sortino, 2)
                            ),
                            theme::MUTED,
                        );

                        ui.add_space(8.0);
                        section(ui, "ด่านความเสี่ยงปฏิเสธอะไร");
                        if view.rejects.is_empty() {
                            ui.label(
                                egui::RichText::new(format!(
                                    "ไม่ปฏิเสธอะไรเลย จากสัญญาณ {} ครั้ง",
                                    view.signals
                                ))
                                .size(10.5)
                                .color(theme::DIM),
                            );
                        } else {
                            for (_, label, count) in view.rejects.iter().take(4) {
                                card::kv(ui, label, &format!("{count} ครั้ง"), theme::WARN);
                            }
                            if view.rejects.len() > 4 {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "และอีก {} เหตุผล รวมถูกปฏิเสธ {} ครั้ง",
                                        view.rejects.len() - 4,
                                        view.reject_total
                                    ))
                                    .size(10.0)
                                    .color(theme::FAINT),
                                );
                            }
                        }
                        if view.unfilled > 0 {
                            card::kv(
                                ui,
                                "ผ่านด่านแต่ไม่ได้ไม้",
                                &format!("{} ออเดอร์", view.unfilled),
                                theme::WARN,
                            );
                        }
                        if let Some((at, reason)) = &view.halted {
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "บอทจะหยุดตัวเองที่ {} เพราะ {}",
                                    day_of(at),
                                    if reason.is_empty() {
                                        "ไม่ได้บันทึกเหตุผล"
                                    } else {
                                        reason
                                    }
                                ))
                                .size(11.0)
                                .color(theme::BAD),
                            );
                        }
                    });
            },
        );
        self.put("quality", slot);
    }

    /// The equity curve, with every stretch below the previous peak shaded.
    fn equity_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("equity");
        let summary = self.bot.summary.clone();
        let load = self.bot.back_load.clone();
        let state = if summary.is_some() {
            theme::State::Simulated
        } else {
            theme::State::Idle
        };
        let mut zoom = self.equity_zoom;
        let mut on_chart = None;
        let status = match &summary {
            Some(view) if !zoom.is_whole() => format!(
                "{} จาก {} จุด",
                zoom.shown(view.curve.len()),
                view.curve.len()
            ),
            Some(view) if view.curve_total > view.curve.len() => {
                format!("{} จุด จาก {}", view.curve.len(), view.curve_total)
            }
            Some(view) => format!("{} จุด", view.curve.len()),
            None => "ยังไม่ได้ทดสอบ".to_string(),
        };
        card::show(
            ui,
            &mut slot,
            "เส้นทุน · แรเงาคือช่วงใต้พีค",
            &status,
            state,
            |ui| {
                let Some(view) = summary else {
                    waiting(ui, &load, "tradebot --backtest --days 730 --trades");
                    return;
                };
                let area = ui.max_rect();
                let total = view.curve.len();
                let (start, end) = zoom.window(total);
                if equity::draw(ui, area, &view.curve[start..end], view.start_equity) {
                    on_chart = Some((
                        chart_input(ui, area, "equity", &mut zoom, total, end - start),
                        total,
                    ));
                    return;
                }
                ui.label(
                    egui::RichText::new("จุดบนเส้นทุนน้อยเกินกว่าจะวาดเป็นเส้น")
                        .size(11.5)
                        .color(theme::MUTED),
                );
            },
        );
        self.equity_zoom = zoom;
        match on_chart {
            Some((true, total)) => self.hover = Some((Chart::Equity, total)),
            _ if matches!(self.hover, Some((Chart::Equity, _))) => self.hover = None,
            _ => {}
        }
        self.put("equity", slot);
    }

    // ------------------------------------------------------- the account
    //
    // Six cards about one account, all read-only, all of them the exchange
    // own numbers rather than the bot own. The bot cards say what it
    // believed; these say what is true. When they disagree, the
    // disagreement is the lesson.

    /// What a card says when the server has no key: once, in words that say
    /// what to do, rather than an error shape repeated six times.
    fn no_account(ui: &mut egui::Ui, load: &Load) {
        let reason = match load {
            Load::Missing(reason) | Load::Failed(reason) => reason.clone(),
            _ => "กำลังถามตลาด".to_string(),
        };
        ui.label(egui::RichText::new(reason).size(11.0).color(theme::MUTED));
    }

    /// Money, and how much of it is already committed as margin.
    fn balance_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("balance");
        let balance = self.account_data.balance.clone();
        let load = self.account_data.load_of(Acc::Balance).clone();
        let state = if balance.is_some() {
            theme::State::Ok
        } else {
            theme::State::Idle
        };
        let status = match &balance {
            Some(balance) if !balance.at.is_empty() => balance.at.clone(),
            Some(_) => "จากตลาด".to_string(),
            None => "ยังไม่มี".to_string(),
        };
        let (state, status) = stale_if_failed(&load, state, status);
        card::show(
            ui,
            &mut slot,
            "ยอดเงินในบัญชี · จากตลาด",
            &status,
            state,
            |ui| {
                let Some(balance) = balance else {
                    Self::no_account(ui, &load);
                    return;
                };
                ui.label(
                    egui::RichText::new(opt_text(&balance.equity))
                        .size(23.0)
                        .monospace()
                        .color(theme::TEXT),
                );
                ui.label(
                    egui::RichText::new(format!("ทุนรวม · {}", balance.currency))
                        .size(10.5)
                        .color(theme::DIM),
                );
                ui.add_space(6.0);
                card::kv(ui, "เงินที่ใช้ได้", &opt_text(&balance.available), theme::MUTED);
                // Unrealised is the one number here that moves on its own, so it
                // carries the colour money carries everywhere else on the board.
                let unrealised = balance
                    .unrealized
                    .as_ref()
                    .map(|num| num.value)
                    .unwrap_or(0.0);
                card::kv(
                    ui,
                    "กำไรขาดทุนที่ยังไม่ปิด",
                    &opt_text(&balance.unrealized),
                    bot::pnl_colour(unrealised),
                );
                card::kv(
                    ui,
                    "หลักประกันที่วางไว้",
                    &opt_text(&balance.initial_margin),
                    theme::MUTED,
                );
                if let Some(used) = balance.used() {
                    card::kv(
                        ui,
                        "ใช้ไปของทุนทั้งหมด",
                        &format!("{:.1}%", used * 100.0),
                        if used > 0.5 {
                            theme::WARN
                        } else {
                            theme::MUTED
                        },
                    );
                }
                card::kv(
                    ui,
                    "หลักประกันขั้นต่ำ",
                    &opt_text(&balance.maintenance_margin),
                    theme::MUTED,
                );
            },
        );
        self.put("balance", slot);
    }

    /// What is open right now, with the loss or gain it is sitting on.
    fn positions_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("positions");
        let positions = self.account_data.positions.clone();
        let load = self.account_data.load_of(Acc::Positions).clone();
        let state = if positions.is_empty() {
            theme::State::Idle
        } else {
            theme::State::Ok
        };
        let status = if positions.is_empty() {
            "ไม่มีไม้ค้าง".to_string()
        } else {
            format!("{} ไม้", positions.len())
        };
        let (state, status) = stale_if_failed(&load, state, status);
        card::show(
            ui,
            &mut slot,
            "ไม้ที่ถืออยู่ · จากตลาด",
            &status,
            state,
            |ui| {
                if positions.is_empty() {
                    if matches!(load, Load::Ready) {
                        ui.label(
                            egui::RichText::new("บัญชีว่าง ไม่มีอะไรเปิดค้างไว้")
                                .size(11.5)
                                .color(theme::MUTED),
                        );
                    } else {
                        Self::no_account(ui, &load);
                    }
                    return;
                }
                egui::ScrollArea::vertical()
                    .id_salt("positions")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for position in &positions {
                            let colour = if position.is_long() {
                                theme::OK
                            } else {
                                theme::BAD
                            };
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} {} {}",
                                    position.side(),
                                    opt_text(&position.size),
                                    position.instrument
                                ))
                                .size(12.0)
                                .monospace()
                                .color(colour),
                            );
                            card::kv(ui, "ราคาเข้า", &opt_text(&position.entry), theme::MUTED);
                            card::kv(ui, "ราคาตลาดตอนนี้", &opt_text(&position.mark), theme::MUTED);
                            let unrealised = position
                                .unrealized
                                .as_ref()
                                .map(|num| num.value)
                                .unwrap_or(0.0);
                            card::kv(
                                ui,
                                "กำไรขาดทุนที่ยังไม่ปิด",
                                &opt_text(&position.unrealized),
                                bot::pnl_colour(unrealised),
                            );
                            if position.liquidation.is_some() {
                                card::kv(
                                    ui,
                                    "ราคาที่จะโดนบังคับปิด",
                                    &opt_text(&position.liquidation),
                                    theme::WARN,
                                );
                            }
                            ui.add_space(6.0);
                        }
                    });
            },
        );
        self.put("positions", slot);
    }

    /// Orders sitting on the book right now.
    ///
    /// Worth its own card because an order that never filled is invisible
    /// everywhere else: the bot logged that it sent one, and the money has
    /// not moved, and both of those are true at once.
    fn open_orders_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("open_orders");
        let orders = self.account_data.open_orders.clone();
        let load = self.account_data.load_of(Acc::OpenOrders).clone();
        let state = if orders.is_empty() {
            theme::State::Idle
        } else {
            theme::State::Wip
        };
        let status = if orders.is_empty() {
            "ไม่มีออเดอร์ค้าง".to_string()
        } else {
            format!("{} ใบ", orders.len())
        };
        let (state, status) = stale_if_failed(&load, state, status);
        card::show(
            ui,
            &mut slot,
            "ออเดอร์ที่ยังค้างในตลาด",
            &status,
            state,
            |ui| {
                if orders.is_empty() {
                    if matches!(load, Load::Ready) {
                        ui.label(
                            egui::RichText::new("ไม่มีใบไหนค้างอยู่บนสมุดคำสั่ง")
                                .size(11.5)
                                .color(theme::MUTED),
                        );
                    } else {
                        Self::no_account(ui, &load);
                    }
                    return;
                }
                columns(ui, "เวลา", "ราคา", "ได้ไปแล้ว");
                ui.spacing_mut().item_spacing.y = 0.0;
                egui::ScrollArea::vertical()
                    .id_salt("open_orders")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for order in &orders {
                            let colour = if order.is_buy { theme::OK } else { theme::BAD };
                            trade_row(
                                ui,
                                &format!("{} {}", order.at, order.side()),
                                &opt_text(&order.price),
                                &format!("{} / {}", opt_text(&order.filled), opt_text(&order.size)),
                                colour,
                            );
                        }
                    });
            },
        );
        self.put("open_orders", slot);
    }

    /// Every order the account sent, filled or not.
    fn orders_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("orders");
        let orders = self.account_data.orders.clone();
        let load = self.account_data.load_of(Acc::Orders).clone();
        let state = if orders.is_empty() {
            theme::State::Idle
        } else {
            theme::State::Ok
        };
        let status = if orders.is_empty() {
            "ยังไม่มี".to_string()
        } else {
            format!("{} ใบล่าสุด", orders.len())
        };
        let (state, status) = stale_if_failed(&load, state, status);
        card::show(
            ui,
            &mut slot,
            "ประวัติออเดอร์ · จากตลาด",
            &status,
            state,
            |ui| {
                if orders.is_empty() {
                    Self::no_account(ui, &load);
                    return;
                }
                columns(ui, "เวลา", "ราคา", "สถานะ");
                ui.spacing_mut().item_spacing.y = 0.0;
                egui::ScrollArea::vertical()
                    .id_salt("orders")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for order in &orders {
                            // The status is what this card exists for: an order
                            // the exchange refused looks exactly like one it
                            // filled, right up until you read this column.
                            let colour = match order.status.as_str() {
                                "FILLED" => theme::OK,
                                "REJECTED" | "CANCELLED" => theme::BAD,
                                _ => theme::MUTED,
                            };
                            let price = if order.is_market {
                                format!("ตลาด {}", opt_text(&order.avg_price))
                            } else {
                                opt_text(&order.price)
                            };
                            trade_row(
                                ui,
                                &format!("{} {}", order.at, order.side()),
                                &price,
                                &status_label(order),
                                colour,
                            );
                        }
                    });
            },
        );
        self.put("orders", slot);
    }

    /// Positions that are over, with what they made and what they cost.
    fn closed_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("closed");
        let rows = self.account_data.closed.clone();
        let load = self.account_data.load_of(Acc::Closed).clone();
        let total: f64 = rows.iter().filter_map(|row| row.after_costs()).sum();
        let state = if rows.is_empty() {
            theme::State::Idle
        } else {
            theme::State::Ok
        };
        let status = if rows.is_empty() {
            "ยังไม่มี".to_string()
        } else {
            format!("{} ไม้ · รวม {}", rows.len(), bot::signed(total))
        };
        let (state, status) = stale_if_failed(&load, state, status);
        card::show(
            ui,
            &mut slot,
            "ไม้ที่ปิดไปแล้ว · กำไรขาดทุนจริง",
            &status,
            state,
            |ui| {
                if rows.is_empty() {
                    Self::no_account(ui, &load);
                    return;
                }
                columns(ui, "ปิดเมื่อ", "เข้า / ออก", "ได้เท่าไร");
                ui.spacing_mut().item_spacing.y = 0.0;
                egui::ScrollArea::vertical()
                    .id_salt("closed")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for row in &rows {
                            let pnl = row.after_costs().unwrap_or(0.0);
                            let side = if row.is_long {
                                "ซื้อ"
                            } else {
                                "ขาย"
                            };
                            trade_row(
                                ui,
                                &format!("{} {}", row.closed_at, side),
                                &format!("{} / {}", opt_text(&row.entry), opt_text(&row.exit)),
                                &bot::signed(pnl),
                                bot::pnl_colour(pnl),
                            );
                        }
                    });
            },
        );
        self.put("closed", slot);
    }

    /// What the trading cost, fill by fill.
    ///
    /// The demo strategy loses to fees by design, and this is the card that
    /// proves it rather than asserting it: the fee column against the money
    /// column, in the same currency.
    fn fills_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("fills");
        let fills = self.account_data.fills.clone();
        let load = self.account_data.load_of(Acc::Fills).clone();
        let fees: f64 = fills
            .iter()
            .filter_map(|fill| fill.fee.as_ref().map(|fee| fee.value))
            .sum();
        let state = if fills.is_empty() {
            theme::State::Idle
        } else {
            theme::State::Ok
        };
        let status = if fills.is_empty() {
            "ยังไม่มี".to_string()
        } else {
            format!("{} ครั้ง · รวม {:.4}", fills.len(), fees)
        };
        let (state, status) = stale_if_failed(&load, state, status);
        card::show(
            ui,
            &mut slot,
            "ค่าธรรมเนียมที่จ่ายจริง",
            &status,
            state,
            |ui| {
                if fills.is_empty() {
                    Self::no_account(ui, &load);
                    return;
                }
                columns(ui, "เวลา", "ราคา x จำนวน", "ค่าธรรมเนียม");
                ui.spacing_mut().item_spacing.y = 0.0;
                egui::ScrollArea::vertical()
                    .id_salt("fills")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for fill in &fills {
                            // Taker pays more than maker, and on this account
                            // every fill is a taker fill, which is itself worth
                            // noticing.
                            let taker = if fill.is_taker {
                                "จ่ายสเปรด"
                            } else {
                                "ตั้งรอ"
                            };
                            let colour = if fill.is_buy { theme::OK } else { theme::BAD };
                            trade_row(
                                ui,
                                &format!("{} {}", fill.at, taker),
                                &format!("{} x {}", opt_text(&fill.price), opt_text(&fill.size)),
                                &format!("{} ({}%)", opt_text(&fill.fee), opt_text(&fill.fee_rate)),
                                colour,
                            );
                        }
                    });
            },
        );
        self.put("fills", slot);
    }

    /// The limits of the number above. On the screen, not in a footnote.
    fn notes_card(&mut self, ui: &mut egui::Ui) {
        let mut slot = self.slot("notes");
        let summary = self.bot.summary.clone();
        let notes: Vec<String> = summary
            .as_ref()
            .map(|view| view.notes.clone())
            .unwrap_or_else(|| bot::NOTES.iter().map(|note| (*note).to_string()).collect());
        let warnings: Vec<String> = summary
            .as_ref()
            .map(|view| view.warnings.clone())
            .unwrap_or_default();
        let status = format!("{} ข้อ", notes.len() + warnings.len());
        card::show(
            ui,
            &mut slot,
            "อ่านก่อนเชื่อผลนี้",
            &status,
            theme::State::Wip,
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("notes")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // The runner's own warnings first: they are about this
                        // run, not about backtesting in general.
                        for warning in &warnings {
                            ui.label(
                                egui::RichText::new(format!("· {warning}"))
                                    .size(11.0)
                                    .color(theme::WARN),
                            );
                            ui.add_space(3.0);
                        }
                        for note in &notes {
                            ui.label(
                                egui::RichText::new(format!("· {note}"))
                                    .size(11.0)
                                    .color(theme::MUTED),
                            );
                            ui.add_space(3.0);
                        }
                    });
            },
        );
        self.put("notes", slot);
    }
}

/// What a card says while the file behind it does not exist yet.
///
/// A student who has not run the bot should read a command they can run, not
/// the word "error": nothing is broken, there is simply nothing there yet.
fn waiting(ui: &mut egui::Ui, load: &Load, command: &str) {
    let reason = match load {
        Load::Missing(reason) | Load::Failed(reason) => reason.clone(),
        _ => "กำลังอ่านไฟล์ที่บอทเขียน".to_string(),
    };
    ui.label(egui::RichText::new(reason).size(11.5).color(theme::MUTED));
    if !command.is_empty() {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(command)
                .size(11.0)
                .monospace()
                .color(theme::ACCENT),
        );
    }
}

/// Drag a chart to slide it, double click to see all of it again, and say
/// whether the pointer is on it.
///
/// Zooming is not here: the wheel has to be taken before the scene is built,
/// which is a frame earlier than any card exists. See `Board::take_wheel`.
/// What a stage card shows of the code behind it.
struct Strip {
    /// `ping_pong.py · get_signal`, short enough for a narrow card.
    where_at: String,
    /// `(line number, the line itself, times it ran)`.
    lines: Vec<(usize, String, u64)>,
    ran: usize,
    total: usize,
    /// True when these are lines the bot was seen running, false when they
    /// are the top of the function it would run.
    live: bool,
    /// How brightly each visible line is lit by the replay: 1.0 under the
    /// head, less for the few lines behind it, absent for the rest.
    glow: BTreeMap<usize, f32>,
    /// Leading spaces every line of the function shares.
    indent: usize,
}

/// Said by the paid-funding card when the account has never been charged.
const FUNDING_NONE: &str = "ยังไม่เคยถูกเก็บ funding · ตลาดเก็บเฉพาะไม้ที่ถือข้ามเวลาเก็บ \
     (ดูเวลาถัดไปในการ์ด funding โซนตลาด) บอทที่ถือไม้ไม่กี่นาทีจึงมักไม่โดนเลย";

/// How long a line stays lit after the tick that ran it, in seconds.
const FLASH: f64 = 0.9;

/// How brightly the head lights the line it is on, and the lines it has
/// just left. Four steps of tail is enough to show direction and short
/// enough not to read as a second highlight.
const TRAIL: [f32; 4] = [1.0, 0.55, 0.32, 0.15];

/// Said over a walk whose order is not the order the code ran in.
const UNORDERED: &str = "เรียงบนลงล่างตามบรรทัดที่เดินจริง · รอบนี้บอทยังไม่ได้จดลำดับ \
     รันบอทใหม่แล้วจะเดินตามลำดับจริง";

/// The steps a head walks through one function, and whether that order is
/// the order the interpreter took.
#[derive(Debug, Clone, Default)]
struct Walk {
    steps: Vec<usize>,
    ordered: bool,
}

/// How brightly each line is lit when the head is on step `head`.
///
/// A line that appears twice in the tail - a tight loop - keeps the
/// brighter of the two. The tail never wraps round to the end of the walk:
/// the first step of a replay has nothing behind it, and drawing the last
/// lines of the previous pass there would light code that has not run yet
/// in this one.
fn glow_of(steps: &[usize], head: usize) -> BTreeMap<usize, f32> {
    let mut out = BTreeMap::new();
    if steps.is_empty() {
        return out;
    }
    for (back, weight) in TRAIL.iter().enumerate() {
        if back > head {
            break;
        }
        let line = steps[head - back];
        let entry = out.entry(line).or_insert(0.0_f32);
        if *weight > *entry {
            *entry = *weight;
        }
    }
    out
}

/// The text size of the wide code card, and the height of one of its
/// lines. `code_line` allocates `size + 3` and the card sets no spacing
/// between rows, so the two must change together.
const CODE_SIZE: f32 = 10.0;
const CODE_ROW: f32 = CODE_SIZE + 3.0;

/// The scroll offset that puts row `row` in the middle of a viewport.
///
/// Plain arithmetic, because the rows are all one height and egui's own
/// "scroll to this rectangle" measured it in the wrong space inside a
/// zoomed scene. Never negative: the first rows sit at the top rather than
/// in the middle of empty space above the function.
fn follow_offset(row: usize, row_height: f32, viewport: f32) -> f32 {
    (row as f32 * row_height + row_height / 2.0 - viewport / 2.0).max(0.0)
}

/// The speeds the walk can go at, in lines per second, with the word on
/// the button for each.
///
/// Eight is about as fast as a line can be read as it goes past; two is the
/// pace of someone explaining each line aloud to a room, which is what a
/// teacher at a projector is doing. Four sits between and is where it
/// starts.
const WALK_RATES: [(f64, &str); 3] = [(2.0, "ช้า"), (4.0, "ปกติ"), (8.0, "เร็ว")];
const DEFAULT_RATE: usize = 1;

/// The default pace, for the tests that count steps against the clock.
#[cfg(test)]
const WALK_RATE: f64 = WALK_RATES[DEFAULT_RATE].0;

/// Said when the bot recorded a tick but this stage was not part of it.
const NO_FILE: &str = "ยังไม่รู้ว่าขั้นนี้ใช้ไฟล์ไหน · ตรวจ BOARD_CODE กับ BOARD_CONFIG ของเซิร์ฟเวอร์";

/// Why no line is lit, which is three different situations wearing the
/// same blank face.
///
/// Telling them apart matters more than it looks: "the bot has not run" and
/// "the bot is running but this board cannot see what it did" send a person
/// to completely different places, and a card that says the first while the
/// card next to it counts eighteen signals is a card nobody will trust again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quiet {
    /// Nothing has run here at all.
    NeverRan,
    /// The bot has been running, but the version that wrote this trail did
    /// not record which lines it took.
    OldBot,
    /// As above, and the file on screen is the config file's guess rather
    /// than the running bot's own answer.
    OldBotGuessedFile,
    /// The bot recorded the tick, and the tick stopped before this stage:
    /// no signal means no gate, no approval means no order.
    NotReached,
    /// The bot recorded the tick and its trace is being fetched.
    Loading,
    /// The bot recorded the tick, but its trace has since been rolled out
    /// of the trace file.
    TraceGone,
}

impl Quiet {
    /// For the chip on a narrow card.
    fn short(self) -> &'static str {
        match self {
            Quiet::NeverRan => "ยังไม่ได้รัน",
            Quiet::OldBot | Quiet::OldBotGuessedFile => "บอทยังไม่ได้จด",
            Quiet::NotReached => "รอบนี้ไม่ได้มาถึง",
            Quiet::Loading => "กำลังเปิด trace",
            Quiet::TraceGone => "trace ถูกหมุนทิ้งแล้ว",
        }
    }

    /// For the wide card, where there is room to say what to do about it.
    fn long(self) -> &'static str {
        match self {
            Quiet::NeverRan => "นี่คือโค้ดที่ขั้นนี้ใช้ · รันบอทแล้วบรรทัดที่เดินจริงจะสว่างขึ้นมา",
            Quiet::OldBot => "บอทที่รันอยู่ยังไม่ได้จดว่าเดินบรรทัดไหน · ปิดแล้วรันใหม่ บรรทัดจะสว่างขึ้นมาเอง",
            Quiet::OldBotGuessedFile => {
                "บอทที่รันอยู่ยังไม่ได้บอกว่าใช้ไฟล์ไหน · ไฟล์นี้เดามาจากคอนฟิก อาจไม่ใช่กลยุทธ์ตัวจริง — ปิดแล้วรันบอทใหม่"
            }
            Quiet::Loading => "กำลังเปิดร่องรอยของ tick นี้",
            Quiet::TraceGone => {
                "ร่องรอยของ tick นี้เก่าเกินกว่าที่ไฟล์ trace เก็บไว้ (หมุนไฟล์ทิ้งเมื่อเกิน 64 MB) · เลือก tick ที่ใหม่กว่า"
            }
            Quiet::NotReached => {
                "tick ล่าสุดหยุดก่อนถึงขั้นนี้ · ดูการ์ดขั้นก่อนหน้าว่าหยุดที่บรรทัดไหน \
                 หรือเลือก tick ที่ส่งออเดอร์มาเล่นซ้ำ"
            }
        }
    }
}

/// The file name without its path, for a card with no room for the path.
fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// One line of the bot's source, with its number and what the last tick did
/// with it.
///
/// A line that ran is lit; a line that did not is dim but still there,
/// because the branch *not* taken is half of what a reader is looking for.
/// A count on the right means a loop went round that many times, which is
/// the one thing a listing of code can never show by itself.
///
/// (The struct below is what one call draws; `code_line` itself follows.)
struct Shown<'a> {
    number: usize,
    text: &'a str,
    /// How many times the last tick ran it.
    hits: u64,
    /// 1.0 for the line under the replay head, less for the few just behind
    /// it, 0.0 for every other line.
    glow: f32,
}

fn code_line(ui: &mut egui::Ui, line: Shown<'_>, flash: f32, size: f32, gutter: f32) -> Rect {
    let Shown {
        number,
        text,
        hits,
        glow,
    } = line;
    let here = glow >= 1.0;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), size + 3.0), Sense::hover());
    let painter = ui.painter();
    if hits > 0 {
        painter.rect_filled(rect, 2, theme::OK.gamma_multiply(0.10 + 0.20 * flash));
    }
    if glow > 0.0 && !here {
        // The trail: the lines the head has just left, fading. One lit line
        // jumping about reads as flicker; a lit line pulling a short tail
        // reads as something moving down the page, which is what it is.
        painter.rect_filled(rect, 0, theme::ACCENT.gamma_multiply(0.30 * glow));
    }
    if here {
        // The line the interpreter is on, drawn like a selection in an
        // editor - a solid bar the width of the card - because that is the
        // shape every reader already knows means "this one".
        painter.rect_filled(rect, 0, theme::ACCENT.gamma_multiply(0.62));
        painter.rect_filled(
            Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
            0,
            theme::ACCENT,
        );
        // Drawn rather than typed: none of the arrow characters a person
        // would reach for exist in the fonts this board carries, and a
        // missing glyph is an empty box in the one place a reader is
        // looking.
        let middle = rect.center().y;
        let left = rect.min.x + 2.0;
        let head = size * 0.42;
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(left, middle - head),
                egui::pos2(left + head * 1.3, middle),
                egui::pos2(left, middle + head),
            ],
            theme::TEXT,
            egui::Stroke::NONE,
        ));
    }
    painter.text(
        egui::pos2(rect.min.x + gutter - 6.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        number,
        egui::FontId::monospace(size - 1.5),
        if here { theme::TEXT } else { theme::FAINT },
    );
    painter.text(
        egui::pos2(rect.min.x + gutter, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::monospace(size),
        // On the bar the text has to be the brightest thing on the card,
        // not the bar's own colour, or the line being pointed at is the one
        // line nobody can read.
        match (here, hits > 0) {
            (true, _) => theme::TEXT,
            (false, true) => theme::TEXT,
            (false, false) => theme::DIM,
        },
    );
    if hits > 1 {
        painter.text(
            egui::pos2(rect.max.x - 2.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            format!("×{hits}"),
            egui::FontId::monospace(size - 1.5),
            theme::ACCENT,
        );
    }
    rect
}

/// One run, as a line in the history card.
fn run_row(ui: &mut egui::Ui, run: &history::Run, here: bool) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), Sense::click());
    let painter = ui.painter();
    if here {
        // The run the cards are showing a tick of, so the list and the
        // pipeline can be read against each other.
        painter.rect_stroke(
            rect.expand2(egui::vec2(2.0, 0.0)),
            3,
            egui::Stroke::new(1.0, theme::ACCENT),
            egui::StrokeKind::Inside,
        );
    } else if response.hovered() {
        painter.rect_filled(rect, 3, theme::ACCENT.gamma_multiply(0.06));
    }
    let when = match run.seconds() {
        Some(seconds) => format!(
            "{} · {}",
            history::Run::clock(&run.started_at),
            length(seconds)
        ),
        None => history::Run::clock(&run.started_at).to_string(),
    };
    // A run the bot did not write down is labelled as worked out, and an
    // unknown strategy is a question mark rather than a blank, which would
    // read as a strategy with no name.
    let strategy = if run.strategy.is_empty() {
        "?"
    } else {
        run.strategy.as_str()
    };
    // No receipt in the trail means nothing says where the orders went - a
    // run whose orders were all refused, usually - and an empty field
    // between two dots reads as a typo rather than as "unknown".
    let mode = if run.mode.is_empty() {
        "ไม่มีใบยืนยัน"
    } else {
        run.mode.as_str()
    };
    let what = if run.inferred {
        format!("{strategy} · {mode} · เดาจาก audit")
    } else {
        format!("{strategy} · {mode}")
    };
    // A run that was killed has no counts at all, and saying "0 ticks"
    // would be a measurement of something that was never measured.
    let result = if !run.finished {
        "ยังไม่จบ".to_string()
    } else if run.halted {
        format!("หยุดเอง · {}", short(&run.reason, 16))
    } else {
        let change = match run.change {
            Some(change) => money(change),
            None => "-".to_string(),
        };
        format!("{} รอบ · {} ออเดอร์ · {}", run.ticks, run.orders, change)
    };
    let colour = if !run.finished {
        theme::WARN
    } else {
        colour_of(run.change)
    };
    let font = egui::FontId::monospace(10.0);
    painter.text(
        egui::pos2(rect.min.x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        when,
        font.clone(),
        theme::FAINT,
    );
    painter.text(
        egui::pos2(rect.min.x + rect.width() * 0.28, rect.center().y),
        egui::Align2::LEFT_CENTER,
        what,
        font.clone(),
        theme::MUTED,
    );
    painter.text(
        egui::pos2(rect.max.x, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        result,
        font,
        colour,
    );
    response
}

/// `61 วิ`, `4 นาที`, `2 ชม. 13 นาที`.
fn length(seconds: u64) -> String {
    if seconds < 90 {
        return format!("{seconds} วิ");
    }
    let minutes = seconds / 60;
    if minutes < 90 {
        return format!("{minutes} นาที");
    }
    format!("{} ชม. {} นาที", minutes / 60, minutes % 60)
}

/// A signed amount, with the sign always there.
///
/// `+12.40` and `12.40` read as different claims when they are side by side
/// in a column, and only one of them is a profit.
fn money(change: f64) -> String {
    if change >= 0.0 {
        format!("+{change:.2}")
    } else {
        format!("{change:.2}")
    }
}

fn colour_of(change: Option<f64>) -> egui::Color32 {
    match change {
        Some(change) if change > 0.0 => theme::OK,
        Some(change) if change < 0.0 => theme::BAD,
        Some(_) => theme::MUTED,
        None => theme::FAINT,
    }
}

/// A month of squares, one per day, coloured by what the account did.
fn month_grid(ui: &mut egui::Ui, year: i32, month: u32, days: &[history::Day], today: &str) {
    let width = ui.available_width();
    let cell = (width / 7.0).floor();
    let height = 38.0;
    let start = history::weekday(year, month, 1);
    let count = history::days_in(year, month);
    let rows = (start as u32 + count).div_ceil(7);

    // The weekday strip, so a square can be read as a day of the week
    // without counting back to the first of the month.
    let (head, _) = ui.allocate_exact_size(egui::vec2(width, 14.0), Sense::hover());
    for (index, name) in history::WEEKDAYS.iter().enumerate() {
        ui.painter().text(
            egui::pos2(
                head.min.x + cell * index as f32 + cell / 2.0,
                head.center().y,
            ),
            egui::Align2::CENTER_CENTER,
            *name,
            egui::FontId::proportional(9.0),
            theme::FAINT,
        );
    }

    let (grid, _) = ui.allocate_exact_size(egui::vec2(width, height * rows as f32), Sense::hover());
    for day in 1..=count {
        let index = start as u32 + day - 1;
        let (row, column) = (index / 7, index % 7);
        let square = Rect::from_min_size(
            egui::pos2(
                grid.min.x + cell * column as f32,
                grid.min.y + height * row as f32,
            ),
            egui::vec2(cell - 3.0, height - 3.0),
        );
        let date = format!("{year:04}-{month:02}-{day:02}");
        let found = days.iter().find(|entry| entry.date == date);
        let painter = ui.painter();
        // A day the bot did not run is not a flat day: it is a day with no
        // measurement, and it is drawn as empty rather than as zero.
        let fill = match found.and_then(|entry| entry.change) {
            Some(change) if change > 0.0 => theme::OK.gamma_multiply(0.22),
            Some(change) if change < 0.0 => theme::BAD.gamma_multiply(0.22),
            Some(_) => theme::MUTED.gamma_multiply(0.14),
            None if found.is_some() => theme::MUTED.gamma_multiply(0.10),
            None => theme::GRID,
        };
        painter.rect_filled(square, 3, fill);
        if date == today {
            painter.rect_stroke(
                square,
                3,
                egui::Stroke::new(1.0, theme::ACCENT),
                egui::StrokeKind::Inside,
            );
        }
        painter.text(
            egui::pos2(square.min.x + 4.0, square.min.y + 3.0),
            egui::Align2::LEFT_TOP,
            format!("{day}"),
            egui::FontId::proportional(9.0),
            if found.is_some() {
                theme::TEXT
            } else {
                theme::FAINT
            },
        );
        if let Some(entry) = found {
            if let Some(change) = entry.change {
                painter.text(
                    egui::pos2(square.max.x - 4.0, square.max.y - 3.0),
                    egui::Align2::RIGHT_BOTTOM,
                    format!("{change:+.0}"),
                    egui::FontId::monospace(9.0),
                    colour_of(Some(change)),
                );
            }
        }
    }
}

fn chart_input(
    ui: &egui::Ui,
    area: Rect,
    name: &str,
    zoom: &mut Zoom,
    total: usize,
    shown: usize,
) -> bool {
    if total < 2 {
        return false;
    }
    let response = ui.interact(
        area,
        ui.scope_id().with(name).with("zoom"),
        Sense::click_and_drag(),
    );
    if response.double_clicked() {
        zoom.reset();
    } else if response.dragged() {
        // One bar is this many pixels wide, so a drag of n pixels is a drag
        // of n/width bars - dragging right walks back into older data.
        let step = area.width() / shown.max(1) as f32;
        let bars = (response.drag_delta().x / step.max(0.5)).round() as i32;
        if bars != 0 {
            zoom.pan_by(bars, total);
        }
    }
    // The response's own test, not a bare rectangle check: it knows about
    // the scene's transform and about any card drawn on top of this one.
    response.contains_pointer()
}

/// Cut a long number down to what a status chip can hold, with a sign that
/// it was cut.
fn short(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit {
        return text.to_owned();
    }
    let kept: String = text.chars().take(limit.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// A card that is still showing the last thing it managed to fetch, while
/// the fetch behind it is failing, has to say so.
///
/// This is the quiet version of the frozen chart: the numbers are all
/// there, the card looks healthy, and none of it has moved for a minute.
/// A board is allowed to show old data; it is not allowed to show old data
/// as if it were current.
/// A number the exchange may not have sent, as the exchange wrote it.
///
/// A dash, never a zero: "0.0" and "the exchange did not say" are different
/// facts, and on a money card the difference matters.
fn opt_text(value: &Option<exchange::Num>) -> String {
    value
        .as_ref()
        .map(|num| num.text.clone())
        .unwrap_or_else(|| "-".to_string())
}

/// An order status in words, with the reason when it was refused.
fn status_label(order: &account::Order) -> String {
    let status = match order.status.as_str() {
        "FILLED" => "ได้ครบ",
        "OPEN" | "PENDING" => "รออยู่",
        "CANCELLED" => "ยกเลิก",
        "REJECTED" => "ถูกปฏิเสธ",
        "" => "-",
        other => other,
    };
    // UNSPECIFIED is the way GRVT says "no reason, because nothing went
    // wrong"; printing it would read like something did.
    if order.reject.is_empty() || order.reject == "UNSPECIFIED" {
        status.to_string()
    } else {
        format!("{status} · {}", order.reject)
    }
}

/// The wall clock as Unix seconds - what the exchange's timestamps are
/// measured against. Zero when the platform will not say, which only makes
/// a live count read as finished.
fn unix_now() -> f64 {
    crate::time::now_unix().unwrap_or(0.0)
}

/// How long a filled order keeps glowing after it lands, in seconds.
const LANDED_GLOW: f64 = 4.0;

/// A trip fresh enough that seeing it already filled still counts as
/// seeing it land, in seconds since it was sent.
const FRESH_TRIP: f64 = 15.0;

/// Update what the board knows about the trip on screen landing.
fn watch_landing(
    watch: &mut Option<(String, bool, Option<f64>)>,
    trip: &Trip,
    phase: &Phase,
    now: f64,
) {
    let live = phase.is_live();
    let filled = *phase == Phase::Filled;
    match watch {
        Some((tick, was_live, landed)) if *tick == trip.tick_at => {
            if *was_live && filled && landed.is_none() {
                *landed = Some(now);
            }
            *was_live = live;
        }
        _ => {
            // First sight of this trip. Already filled and only just sent:
            // the fill beat the board here, and it is still news.
            let fresh = trip.start().is_some_and(|start| now - start < FRESH_TRIP);
            *watch = Some((trip.tick_at.clone(), live, (filled && fresh).then_some(now)));
        }
    }
}

/// The outline the send stage and the trip card wear: pulsing amber while
/// the order is on its way, fading green for a few seconds after the board
/// sees it fill. `None` once there is nothing to draw the eye to.
fn trip_glow(phase: &Phase, landed: Option<f64>, now: f64) -> Option<(egui::Color32, f32)> {
    if phase.is_live() {
        let pulse = (now * 4.0).sin() * 0.5 + 0.5;
        return Some((theme::WARN, 0.35 + 0.65 * pulse as f32));
    }
    let age = now - landed?;
    (0.0..LANDED_GLOW)
        .contains(&age)
        .then(|| (theme::OK, (1.0 - age / LANDED_GLOW) as f32))
}

fn mark_colour(mark: Mark) -> egui::Color32 {
    match mark {
        Mark::Done => theme::OK,
        Mark::Now => theme::WARN,
        Mark::Todo => theme::FAINT,
        Mark::Failed => theme::BAD,
    }
}

fn stale_if_failed(load: &Load, state: theme::State, status: String) -> (theme::State, String) {
    match load {
        Load::Failed(_) => (theme::State::Bad, "ข้อมูลค้าง".to_string()),
        _ => (state, status),
    }
}

/// The connection, as a colour and a sentence.
fn connection(health: Health) -> (egui::Color32, String) {
    match health {
        Health::Live => (theme::OK, "ต่ออยู่".to_string()),
        Health::Quiet(seconds) => (theme::WARN, format!("เงียบ {seconds:.0} วิ")),
        Health::Connecting => (theme::WARN, "กำลังต่อ".to_string()),
        Health::Down(Some(seconds)) => (theme::BAD, format!("หลุด · ต่อใหม่ใน {seconds:.0} วิ")),
        Health::Down(None) => (theme::BAD, "หลุด".to_string()),
    }
}

/// What to print under a price that is no longer arriving.
fn stale_line(health: Health) -> String {
    match health {
        Health::Live => String::new(),
        Health::Quiet(seconds) => format!("ราคาค้างมา {seconds:.0} วินาที"),
        Health::Connecting => "กำลังต่อตลาดใหม่".to_string(),
        Health::Down(Some(seconds)) => {
            format!("ขาดการเชื่อมต่อ · ลองใหม่ในอีก {seconds:.0} วินาที")
        }
        Health::Down(None) => "ขาดการเชื่อมต่อ".to_string(),
    }
}

/// One chip in a card header: on is filled with the accent, off is quiet.
fn chip(ui: &mut egui::Ui, label: &str, on: bool) -> bool {
    let text =
        egui::RichText::new(label)
            .size(11.0)
            .color(if on { theme::CANVAS } else { theme::DIM });
    let button = egui::Button::new(text)
        .fill(if on {
            theme::ACCENT
        } else {
            egui::Color32::TRANSPARENT
        })
        .corner_radius(theme::chip_radius());
    ui.add(button).clicked() && !on
}

/// How much room the pressure bar needs under the book.
const PRESSURE: f32 = 30.0;

/// The split between the two sides of the book, as a bar and two numbers.
///
/// Green from the left is what buyers have put up, red from the right what
/// sellers have, over **the whole book** - the same basis the exchange's
/// own screen uses, which is why the level count is printed next to it
/// rather than the number of rows above.
fn pressure_bar(ui: &mut egui::Ui, buy: f64, sell: f64, rows: usize) {
    ui.add_space(6.0);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), PRESSURE - 8.0),
        Sense::hover(),
    );
    let painter = ui.painter();
    let font = egui::FontId::proportional(10.0);

    let line = rect.min.y + 6.0;
    painter.text(
        egui::pos2(rect.min.x, line),
        egui::Align2::LEFT_CENTER,
        format!("ซื้อ {buy:.0}%"),
        font.clone(),
        theme::OK,
    );
    painter.text(
        egui::pos2(rect.center().x, line),
        egui::Align2::CENTER_CENTER,
        format!("ทั้งกระดาน {rows} ชั้น"),
        egui::FontId::proportional(9.0),
        theme::FAINT,
    );
    painter.text(
        egui::pos2(rect.max.x, line),
        egui::Align2::RIGHT_CENTER,
        format!("{sell:.0}% ขาย"),
        font,
        theme::BAD,
    );

    let bar = Rect::from_min_max(
        egui::pos2(rect.min.x, rect.max.y - 6.0),
        egui::pos2(rect.max.x, rect.max.y - 1.0),
    );
    let split = bar.min.x + bar.width() * (buy / 100.0) as f32;
    painter.rect_filled(
        Rect::from_min_max(bar.min, egui::pos2(split, bar.max.y)),
        2,
        theme::OK,
    );
    painter.rect_filled(
        Rect::from_min_max(egui::pos2(split, bar.min.y), bar.max),
        2,
        theme::BAD,
    );
}

/// How much one notch of a wheel is worth, in egui's scroll points.
const NOTCH: f32 = 50.0;

/// How far one frame's worth of scrolling zooms.
///
/// **Proportional to the scroll, not a fixed step per frame.** egui spreads
/// one notch of a wheel over several frames as it smooths it; a fixed step
/// each time those frames arrive turned a single notch into a dive all the
/// way to the floor of the zoom. Because this is a power of the scroll
/// amount, splitting a notch across frames multiplies back to exactly the
/// same zoom as taking it in one go.
fn zoom_factor(scroll: f32) -> f32 {
    let notches = (scroll / NOTCH).clamp(-4.0, 4.0);
    0.82_f32.powf(notches)
}

/// How tall one row of the book or the tape is.
const ROW: f32 = 15.0;

/// The three column headings over a book or a tape.
fn columns(ui: &mut egui::Ui, left: &str, middle: &str, right: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 13.0), Sense::hover());
    let painter = ui.painter();
    let font = egui::FontId::proportional(9.5);
    painter.text(
        egui::pos2(rect.min.x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        left,
        font.clone(),
        theme::FAINT,
    );
    painter.text(
        egui::pos2(rect.min.x + rect.width() * 0.66, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        middle,
        font.clone(),
        theme::FAINT,
    );
    painter.text(
        egui::pos2(rect.max.x, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        right,
        font,
        theme::FAINT,
    );
}

/// One row of the order book: price, size, and a bar as wide as everything
/// up to that price.
fn book_row(
    ui: &mut egui::Ui,
    level: &exchange::Level,
    total: f64,
    deepest: f64,
    colour: egui::Color32,
) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW), Sense::hover());
    let share = ((total / deepest) as f32).clamp(0.0, 1.0);
    let painter = ui.painter();
    painter.rect_filled(
        Rect::from_min_max(
            egui::pos2(rect.max.x - rect.width() * share, rect.min.y),
            rect.max,
        ),
        1,
        colour.gamma_multiply(0.13),
    );
    three_in(
        painter,
        rect,
        (&level.price.text, colour),
        (&level.size.text, theme::MUTED),
        (&fmt_total(total), theme::DIM),
    );
}

/// One row of the tape: the clock stays quiet and **the price carries the
/// colour**, because the colour is a fact about the trade, not about when
/// it happened.
fn trade_row(ui: &mut egui::Ui, at: &str, price: &str, size: &str, colour: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW), Sense::hover());
    three_in(
        ui.painter(),
        rect,
        (at, theme::FAINT),
        (price, colour),
        (size, theme::MUTED),
    );
}

fn three_in(
    painter: &egui::Painter,
    rect: Rect,
    left: (&str, egui::Color32),
    middle: (&str, egui::Color32),
    right: (&str, egui::Color32),
) {
    let font = egui::FontId::monospace(10.0);
    painter.text(
        egui::pos2(rect.min.x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        left.0,
        font.clone(),
        left.1,
    );
    painter.text(
        egui::pos2(rect.min.x + rect.width() * 0.66, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        middle.0,
        font.clone(),
        middle.1,
    );
    painter.text(
        egui::pos2(rect.max.x, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        right.0,
        font,
        right.1,
    );
}

/// A running total, short enough for a column that is three characters wide
/// on a narrow card.
fn fmt_total(total: f64) -> String {
    if total >= 1000.0 {
        format!("{total:.0}")
    } else if total >= 10.0 {
        format!("{total:.1}")
    } else {
        format!("{total:.3}")
    }
}

/// "2 ชม. 9 นาที" - how long until funding is charged again.
fn countdown(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    if hours > 0 {
        format!("{hours} ชม. {minutes} นาที")
    } else if minutes > 0 {
        format!("{minutes} นาที")
    } else {
        format!("{} วินาที", total % 60)
    }
}

/// A heading inside a card body.
fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(egui::RichText::new(title).size(10.5).color(theme::FAINT));
    ui.add_space(3.0);
}

/// The date half of a timestamp. A card has no room for microseconds.
fn day_of(at: &str) -> &str {
    match at.split_once('T') {
        Some((day, _)) => day,
        None => at,
    }
}

/// Unused for now but kept honest: parsing a price for display never silently
/// becomes zero.
#[allow(dead_code)]
fn price_or_dash(text: &str) -> String {
    match parse(text) {
        Some(_) => text.to_owned(),
        None => "-".to_owned(),
    }
}

#[allow(dead_code)]
fn sense_nothing() -> Sense {
    Sense::hover()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A context that has just seen the wheel turned.
    fn ctx_with_wheel(lines: f32) -> egui::Context {
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, lines),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers: egui::Modifiers::default(),
        });
        let mut output = ctx.run_ui(raw, |_| {});
        output.textures_delta.clear();
        ctx
    }

    /// Every piece of text the board painted in one frame.
    ///
    /// The only way to answer "what does it say on screen" without a screen.
    /// A card can hold the right number and still print the wrong one, and
    /// no amount of testing the number catches that.
    fn painted_text(board: &mut Board) -> Vec<String> {
        painted_at(board, 1.0)
    }

    /// The same, at a chosen moment - which is how a replay is tested: the
    /// head's position is a function of the clock and nothing else.
    fn painted_at(board: &mut Board, now: f64) -> Vec<String> {
        let ctx = egui::Context::default();
        crate::fonts::install(&ctx);
        // One pass to build the fonts, one to draw with them.
        let mut warm = ctx.run_ui(egui::RawInput::default(), |_| {});
        warm.textures_delta.clear();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            board.cards(ui, now);
        });
        output.textures_delta.clear();

        fn walk(shape: &egui::Shape, found: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => found.push(text.galley.text().to_owned()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        walk(shape, found);
                    }
                }
                _ => {}
            }
        }
        let mut found = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut found);
        }
        found
    }

    /// Ticks from a ping_pong run: flat, then a buy under the band, then
    /// holding it - the newest one `ago` seconds before now, because the
    /// card judges by the real clock whether the bot is still running.
    fn ping_pong_rows(ago: f64) -> Vec<bot::Row> {
        let base = crate::time::now_unix().unwrap() - ago - 25.0;
        let at = |offset: f64| crate::time::clock_iso(base + offset);
        let view = |avg: f64| {
            format!(
                r#""view":{{"average":"{avg}","buy_below":"{}","sell_above":"{}","window":"12","edge_pct":"0.03","stop_pct":"0.4","max_hold_s":"180"}}"#,
                avg * 0.9997,
                avg * 1.0003
            )
        };
        let mut rows: Vec<bot::Row> = (0..20)
            .map(|i| {
                serde_json::from_str(&format!(
                    r#"{{"at":"{}","symbol":"BTC_USDT_Perp","price":"{}",{}}}"#,
                    at(i as f64),
                    67000.0 + i as f64,
                    view(67000.0 + i as f64)
                ))
                .unwrap()
            })
            .collect();
        rows.push(
            serde_json::from_str(&format!(
                r#"{{"at":"{}","symbol":"BTC_USDT_Perp","price":"66980",{},
                    "signal":{{"side":"buy","stop_price":"66712.1"}},
                    "order":{{"side":"buy","limit_price":"66990","qty":"0.01"}},
                    "receipt":{{"broker_order_id":"0x00"}}}}"#,
                at(21.0),
                view(67010.0)
            ))
            .unwrap(),
        );
        rows.push(
            serde_json::from_str(&format!(
                r#"{{"at":"{}","symbol":"BTC_USDT_Perp","price":"66995",
                    "position":{{"side":"buy","qty":"0.01","entry_price":"66988"}},
                    "view":{{"average":"67008","buy_below":"66987.9","sell_above":"67028.1",
                            "stop":"66720.0","held_s":"4","max_hold_s":"180","window":"12","edge_pct":"0.03"}}}}"#,
                at(25.0)
            ))
            .unwrap(),
        );
        rows
    }

    /// The strategy picture explains the rule with the strategy's own
    /// numbers, and says what it is waiting for right now.
    #[test]
    fn the_strategy_picture_says_the_rule_and_what_it_is_waiting_for() {
        let mut board = Board::default();
        board.bot.rows = ping_pong_rows(2.0);
        let painted = painted_text(&mut board).join(" | ");
        assert!(painted.contains("หน้าตากลยุทธ์"), "{painted}");
        assert!(!painted.contains("หยุด"), "บอทยังรันอยู่: {painted}");
        assert!(painted.contains("เฉลี่ย 12 ราคาล่าสุด"), "{painted}");
        assert!(painted.contains("ห่างเส้นกลาง 0.03%"), "{painted}");
        assert!(
            painted.contains("ตอนนี้: ถือซื้ออยู่ · ปิดเมื่อราคากลับถึงเส้นกลาง 67008.0"),
            "{painted}"
        );
        assert!(painted.contains("ถือครบ 180 วิ (ผ่านไป 4)"), "{painted}");
        assert!(painted.contains("ซื้อ"), "จุดที่ซื้อต้องมีป้าย");
    }

    /// A bot that has stopped leaves a picture that stops moving while the
    /// candles beside it carry on. The card has to say so, or it looks
    /// frozen (asked about on 16 Nov) - and must not call the last tick
    /// "now".
    #[test]
    fn a_picture_from_a_bot_that_stopped_says_it_stopped() {
        let mut board = Board::default();
        board.bot.rows = ping_pong_rows(600.0);
        let painted = painted_text(&mut board).join(" | ");
        assert!(painted.contains("หยุดแล้ว"), "{painted}");
        assert!(painted.contains("บอทน่าจะหยุดไปแล้ว"), "{painted}");
        assert!(painted.contains("ตอนจบรอบ: ถือซื้ออยู่"), "{painted}");
        assert!(!painted.contains("ตอนนี้:"), "{painted}");
    }

    /// A strategy that does not report its lines gets a picture of the
    /// price and a sentence saying why there is nothing more - never a band
    /// the board made up.
    #[test]
    fn a_strategy_without_lines_is_told_so_and_no_band_is_invented() {
        let mut board = Board::default();
        board.bot.rows = (0..5)
            .map(|i| {
                serde_json::from_str(&format!(
                    r#"{{"at":"2026-11-16T04:00:0{i}+00:00","symbol":"BTC_USDT_Perp","price":"{}"}}"#,
                    100 + i
                ))
                .unwrap()
            })
            .collect();
        let painted = painted_text(&mut board).join(" | ");
        assert!(painted.contains("ไม่ได้บอกว่ามันดูเส้นไหน"), "{painted}");
        assert!(!painted.contains("ตอนนี้:"), "{painted}");
    }

    /// A layout saved before the strategy picture existed would put the
    /// code card straight on top of it. The cards that moved for it take
    /// this version's places; everything else stays where it was dragged.
    #[test]
    fn an_old_layout_does_not_put_the_code_card_on_top_of_the_strategy_picture() {
        let mut board = Board::default();
        board.restore(
            r#"{"market":{"x":5.0,"y":6.0,"w":1.0,"h":1.0},
                "code":{"x":390.0,"y":1390.0,"w":560.0,"h":240.0},
                "balance":{"x":40.0,"y":2320.0,"w":380.0,"h":260.0}}"#,
        );
        assert_eq!(board.slots["market"].x, 5.0, "ที่คนจัดไว้ต้องอยู่");
        assert!(
            !board.slots["code"]
                .rect()
                .intersects(board.slots["shape"].rect()),
            "การ์ดโค้ดทับภาพกลยุทธ์"
        );
        assert_eq!(
            board.slots["balance"].y,
            Board::default().slots["balance"].y
        );

        // Once saved with the picture in it, the person's places win again.
        let mut again = Board::default();
        again.restore(
            r#"{"shape":{"x":390.0,"y":1390.0,"w":1.0,"h":1.0},
                "code":{"x":11.0,"y":12.0,"w":1.0,"h":1.0}}"#,
        );
        assert_eq!(again.slots["code"].x, 11.0);
    }

    /// No two cards start on top of each other.
    #[test]
    fn the_fresh_layout_has_no_card_on_top_of_another() {
        let board = Board::default();
        let slots: Vec<(&&str, &Slot)> = board.slots.iter().collect();
        for (i, (a, sa)) in slots.iter().enumerate() {
            for (b, sb) in &slots[i + 1..] {
                assert!(!sa.rect().intersects(sb.rect()), "{a} ทับ {b}");
            }
        }
    }

    /// Every card sits inside the zone a jump button frames, or the button
    /// takes a student to half a card.
    #[test]
    fn the_strategy_picture_and_the_code_are_inside_the_bot_zone() {
        let board = Board::default();
        for id in ["shape", "code"] {
            assert!(
                view(1).contains_rect(board.slots[id].rect()),
                "{id} อยู่นอกโซนบอท"
            );
        }
    }

    /// A loss has to reach the screen wearing its minus sign.
    #[test]
    fn a_closed_trade_that_lost_money_prints_the_minus() {
        let mut board = Board::default();
        board.account_data.closed = account::read_closed(&serde_json::json!([{
            "instrument": "BTC_USDT_Perp",
            "is_long": false,
            "entry_price": "68714.0",
            "exit_price": "68836.7",
            "realized_pnl": "-17.7915",
            "cumulative_fee": "8.975182",
            "status": "CLOSED",
            "close_time_at": "2026-11-13T09:03:06Z"
        }]));
        let painted = painted_text(&mut board);
        assert!(
            painted.iter().any(|text| text.contains("-17.79")),
            "ไม่มีตัวเลขติดลบบนจอเลย · ที่วาดจริงคือ {painted:?}"
        );
    }

    /// The status card leads with the exchange figure, and says so.
    ///
    /// The bot keeps its own running total in `state.json`, and for a while
    /// that was the only number this card had. It is the number that cannot
    /// be checked against the website, and the one that goes stale the
    /// moment an order is rejected or a fee is charged. When the server has
    /// a key, the balance wins the headline and the bot figure moves to a
    /// labelled row underneath, with the gap between them spelled out.
    #[test]
    fn the_status_card_leads_with_the_exchange_figure_when_there_is_one() {
        let mut board = Board::default();
        board.bot.state = Some(crate::bot::BotState {
            day: "2026-11-13".into(),
            day_start_equity: "10000".into(),
            peak_equity: "10000".into(),
            ..Default::default()
        });
        board.bot.rows = vec![crate::bot::Row {
            at: "2026-11-13T09:03:06Z".into(),
            symbol: "BTC_USDT_Perp".into(),
            price: "68836.7".into(),
            equity: "10000".into(),
            ..Default::default()
        }];
        // The card reads the summary the poll builds, not the rows.
        board.bot.account = crate::bot::account_of(&board.bot.rows, board.bot.state.as_ref());
        board.account_data.balance = Some(account::read_balance(&serde_json::json!({
            "total_equity": "9966.404007",
            "available_balance": "9966.404007",
            "settle_currency": "USDT",
            "event_time_at": "2026-11-13T09:54:11Z"
        })));

        let painted = painted_text(&mut board).join(" | ");
        assert!(
            painted.contains("9966.404007"),
            "ต้องขึ้นยอดจริงจากตลาด: {painted}"
        );
        assert!(
            painted.contains("ทุนจริงที่ตลาด"),
            "ต้องบอกด้วยว่าเลขนี้มาจากไหน: {painted}"
        );
        assert!(
            painted.contains("ที่บอทบันทึกไว้"),
            "เลขของบอทต้องยังอยู่ ไม่ใช่หายไปเฉยๆ: {painted}"
        );
        // 9966.40 against the 10000 the bot still believes.
        assert!(painted.contains("-33.60"), "ต้องบอกว่าต่างกันเท่าไร: {painted}");
    }

    /// Without a key the same card falls back to the bot figure and labels
    /// it honestly, rather than showing a blank where the money goes.
    #[test]
    fn the_status_card_says_whose_number_it_is_when_there_is_no_key() {
        let mut board = Board::default();
        board.bot.state = Some(crate::bot::BotState {
            day: "2026-11-13".into(),
            day_start_equity: "10000".into(),
            ..Default::default()
        });
        board.bot.rows = vec![crate::bot::Row {
            at: "2026-11-13T09:03:06Z".into(),
            symbol: "BTC_USDT_Perp".into(),
            price: "68836.7".into(),
            equity: "10000".into(),
            ..Default::default()
        }];
        let painted = painted_text(&mut board).join(" | ");
        assert!(
            painted.contains("ทุนที่บอทบันทึกไว้"),
            "ไม่มีกุญแจก็ต้องบอกว่าเลขนี้เป็นของบอท: {painted}"
        );
    }

    /// Fees are printed next to what they were charged on, because that is
    /// the comparison the demo strategy exists to make visible.
    #[test]
    fn the_fee_card_prints_the_fee_and_the_rate_that_produced_it() {
        let mut board = Board::default();
        board.account_data.fills = account::read_fills(&serde_json::json!([{
            "instrument": "BTC_USDT_Perp",
            "is_buyer": true,
            "is_taker": true,
            "size": "0.145",
            "price": "68836.7",
            "fee": "4.491594",
            "fee_rate": "0.045",
            "fee_currency": "USDT",
            "event_time_at": "2026-11-13T09:03:06Z"
        }]));
        let painted = painted_text(&mut board).join(" | ");
        assert!(painted.contains("4.491594"), "{painted}");
        assert!(painted.contains("0.045"), "{painted}");
        assert!(
            painted.contains("16:03:06"),
            "เวลาไทยของ 09:03:06Z · {painted}"
        );
    }

    /// Run frames with pointer events in them, as a mouse would send.
    ///
    /// egui decides a drag over three frames - press, move, release - and
    /// `drag_started` only fires once the pointer has moved far enough, so a
    /// test that sends one event learns nothing. This sends what a hand
    /// sends.
    fn drag_pointer(board: &mut Board, from: egui::Pos2, to: egui::Pos2) {
        drag_with(board, from, to, Default::default());
    }

    /// The same gesture with Shift held throughout, which is how a
    /// selection band is drawn.
    fn drag_selecting(board: &mut Board, from: egui::Pos2, to: egui::Pos2) {
        drag_with(
            board,
            from,
            to,
            egui::Modifiers {
                shift: true,
                ..Default::default()
            },
        );
    }

    fn drag_with(board: &mut Board, from: egui::Pos2, to: egui::Pos2, modifiers: egui::Modifiers) {
        let ctx = egui::Context::default();
        // egui keeps its modifier state from `ModifiersChanged`, not from
        // the modifiers hanging off a pointer event, so the key is pressed
        // the way a keyboard presses it: held down before, released after.
        let frames: [Vec<egui::Event>; 5] = [
            vec![
                egui::Event::ModifiersChanged(modifiers),
                egui::Event::PointerMoved(from),
            ],
            vec![egui::Event::PointerButton {
                pos: from,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers,
            }],
            vec![egui::Event::PointerMoved(egui::pos2(
                (from.x + to.x) / 2.0,
                (from.y + to.y) / 2.0,
            ))],
            vec![egui::Event::PointerMoved(to)],
            vec![egui::Event::PointerButton {
                pos: to,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers,
            }],
        ];
        for events in frames {
            // egui learns the modifier state from the events themselves;
            // `RawInput` has no field for it. The press and release carry
            // Shift, and egui holds on to it in between.
            let raw = egui::RawInput {
                events,
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(3000.0, 3000.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(raw, |ui| board.cards(ui, 1.0));
            output.textures_delta.clear();
        }
    }

    /// The feature, end to end: drag across empty canvas, cards light up.
    #[test]
    fn dragging_across_the_canvas_picks_up_the_cards_it_crossed() {
        let mut board = Board::default();
        let market = board.slots["market"].rect();

        drag_selecting(
            &mut board,
            egui::pos2(market.min.x - 20.0, market.min.y - 20.0),
            egui::pos2(market.max.x - 10.0, market.max.y - 10.0),
        );

        assert!(
            board.selected.contains("market"),
            "ลากคลุมการ์ดตลาดแล้วต้องติดมาด้วย: {:?}",
            board.selected
        );
        assert!(
            !board.selected.contains("chart"),
            "ไม่ได้ลากไปถึงกราฟ: {:?}",
            board.selected
        );
        assert!(board.marquee.is_none(), "ปล่อยเมาส์แล้วกรอบต้องหายไป");
    }

    /// Without Shift the board pans, exactly as it always has.
    ///
    /// The guarantee that matters most: somebody who never hears about the
    /// selection band has lost nothing at all. There is no mode they could
    /// have left switched on, and nothing to switch back.
    #[test]
    fn a_drag_with_no_shift_held_selects_nothing() {
        let mut board = Board::default();
        let market = board.slots["market"].rect();

        drag_pointer(
            &mut board,
            egui::pos2(market.min.x - 20.0, market.min.y - 20.0),
            egui::pos2(market.max.x - 10.0, market.max.y - 10.0),
        );

        assert!(
            board.selected.is_empty(),
            "ไม่ได้กด Shift ต้องไม่เกิดกรอบ: {:?}",
            board.selected
        );
    }

    /// A drag that starts on a card is that card's business - a chart that
    /// is zoomed in scrolls itself, and nothing should be drawing a band
    /// across the board behind it.
    #[test]
    fn a_drag_that_starts_on_a_card_draws_no_band() {
        let mut board = Board::default();
        let market = board.slots["market"].rect();

        drag_selecting(
            &mut board,
            market.center(),
            market.center() + Vec2::new(300.0, 300.0),
        );

        assert!(
            board.selected.is_empty(),
            "เริ่มลากบนการ์ด ไม่ใช่การคลุมเลือก: {:?}",
            board.selected
        );
    }

    /// Clicking the bare canvas is how a person says never mind.
    #[test]
    fn a_click_on_nothing_clears_the_selection() {
        let mut board = Board::default();
        board.selected = ["market", "chart"].into_iter().collect();
        let empty = egui::pos2(
            board.slots["market"].x - 25.0,
            board.slots["market"].y - 25.0,
        );

        // A click is a press and release at the same place.
        drag_pointer(&mut board, empty, empty);

        assert!(board.selected.is_empty(), "{:?}", board.selected);
    }

    /// A layout saved earlier comes back.
    #[test]
    fn the_board_opens_where_the_cards_were_left() {
        let mut arranged = Board::default();
        let mut moved = arranged.slots["market"];
        moved.translate(Vec2::new(500.0, 300.0));
        arranged.slots.insert("market", moved);
        let saved = arranged.layout();

        let mut fresh = Board::default();
        let before = fresh.slots["market"];
        fresh.restore(&saved);

        assert_eq!(fresh.slots["market"].x, before.x + 500.0);
        assert_eq!(fresh.slots["market"].y, before.y + 300.0);
    }

    /// A card is drawn at the size this build draws it, wherever the person
    /// left it.
    ///
    /// Sizes change between versions - a card grows a row - and a saved
    /// height from last month would cut the new row off with no way for
    /// anyone to work out why.
    #[test]
    fn a_saved_layout_restores_positions_and_not_sizes() {
        let mut fresh = Board::default();
        let size = fresh.slots["market"];
        fresh.restore("{\"market\":{\"x\":10.0,\"y\":20.0,\"w\":9999.0,\"h\":9999.0}}");
        assert_eq!(
            (fresh.slots["market"].x, fresh.slots["market"].y),
            (10.0, 20.0)
        );
        assert_eq!(
            (fresh.slots["market"].w, fresh.slots["market"].h),
            (size.w, size.h)
        );
    }

    /// A layout from another version mentions cards that are gone and misses
    /// ones that are new. Neither is a reason to drop the rest of it.
    #[test]
    fn a_layout_from_another_version_is_used_as_far_as_it_goes() {
        let mut fresh = Board::default();
        let chart_before = fresh.slots["chart"];
        fresh.restore(
            "{\"market\":{\"x\":1.0,\"y\":2.0,\"w\":3.0,\"h\":4.0},\"a-card-we-deleted\":{\"x\":0.0,\"y\":0.0,\"w\":1.0,\"h\":1.0}}",
        );
        assert_eq!(fresh.slots["market"].x, 1.0, "ใบที่มีอยู่ต้องย้ายตาม");
        assert_eq!(
            fresh.slots["chart"].x, chart_before.x,
            "ใบที่ไฟล์ไม่ได้พูดถึงอยู่ที่เดิม"
        );
        assert!(!fresh.slots.contains_key("a-card-we-deleted"));
    }

    /// Nonsense in the saved file leaves the board alone.
    #[test]
    fn a_layout_file_that_is_not_a_layout_is_ignored() {
        let mut fresh = Board::default();
        let before = fresh.slots["market"];
        for rubbish in ["", "{", "null", "[1,2,3]", "ไม่ใช่ JSON"] {
            fresh.restore(rubbish);
            assert_eq!(fresh.slots["market"].x, before.x, "พังเพราะ {rubbish:?}");
        }
    }

    /// The button that undoes an afternoon of dragging.
    #[test]
    fn putting_the_cards_back_restores_every_one_of_them() {
        let mut board = Board::default();
        let before = board.slots.clone();
        let mut moved = board.slots["market"];
        moved.translate(Vec2::new(900.0, 900.0));
        board.slots.insert("market", moved);
        board.selected = ["market"].into_iter().collect();

        board.reset_layout();

        for (id, slot) in &before {
            assert_eq!(board.slots[id].x, slot.x, "{id} ไม่ได้กลับที่เดิม");
            assert_eq!(board.slots[id].y, slot.y, "{id} ไม่ได้กลับที่เดิม");
        }
        assert!(board.selected.is_empty(), "จัดใหม่แล้วไม่ควรเหลือใบที่เลือกไว้");
    }

    /// Drag inside the real scene, the way the running board is built.
    ///
    /// `drag_pointer` drives the cards directly, which is enough to test a
    /// selection band but says nothing about panning: panning belongs to
    /// the scene, and the scene is not there. This builds one.
    ///
    /// Returns where the board ended up looking.
    fn drag_in_scene(board: &mut Board, from: egui::Pos2, to: egui::Pos2, shift: bool) -> Rect {
        let ctx = egui::Context::default();
        let modifiers = if shift {
            egui::Modifiers {
                shift: true,
                ..Default::default()
            }
        } else {
            Default::default()
        };
        let steps = [
            vec![
                egui::Event::ModifiersChanged(modifiers),
                egui::Event::PointerMoved(from),
            ],
            vec![egui::Event::PointerButton {
                pos: from,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers,
            }],
            vec![egui::Event::PointerMoved(egui::pos2(
                (from.x + to.x) / 2.0,
                (from.y + to.y) / 2.0,
            ))],
            vec![egui::Event::PointerMoved(to)],
            vec![egui::Event::PointerButton {
                pos: to,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers,
            }],
        ];
        for events in steps {
            let raw = egui::RawInput {
                events,
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1200.0, 800.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(raw, |ui| {
                // The same call the running board makes, with the same
                // decision about who gets the left button.
                let hands_off = shift && !board.on_card;
                board.show_scene(ui, 1.0, hands_off);
            });
            output.textures_delta.clear();
        }
        board.scene_rect
    }

    /// The board moves when it is dragged. The plainest thing it does.
    ///
    /// Written after the board stopped moving at all: a widget laid over
    /// the canvas to catch selection drags caught every other drag too, and
    /// with it the pan - every button at once, because a drag sense does
    /// not care which button it was. Nothing in the suite noticed, because
    /// every test so far drove the cards and never the scene.
    #[test]
    fn dragging_the_canvas_still_moves_the_board() {
        let mut board = Board::default();
        let before = board.scene_rect;

        let after = drag_in_scene(
            &mut board,
            egui::pos2(600.0, 700.0),
            egui::pos2(400.0, 500.0),
            false,
        );

        assert_ne!(
            (after.min.x, after.min.y),
            (before.min.x, before.min.y),
            "ลากที่ว่างแล้วบอร์ดต้องเลื่อน"
        );
        assert!(board.selected.is_empty(), "ไม่ได้กด Shift ต้องไม่เลือกอะไร");
    }

    /// With Shift held the board stays put, because the drag is a band.
    #[test]
    fn holding_shift_keeps_the_board_still_while_a_band_is_drawn() {
        let mut board = Board::default();
        let before = board.scene_rect;

        let after = drag_in_scene(
            &mut board,
            egui::pos2(600.0, 700.0),
            egui::pos2(400.0, 500.0),
            true,
        );

        assert_eq!(
            (after.min.x, after.min.y),
            (before.min.x, before.min.y),
            "กด Shift ลาก = คลุมเลือก บอร์ดต้องอยู่นิ่ง"
        );
    }

    /// A band catches what it touches, not only what it swallows whole.
    #[test]
    fn a_rubber_band_catches_every_card_it_touches() {
        let board = Board::default();
        let market = board.slots["market"].rect();

        // A band over the market card alone.
        let caught = board.cards_in(market.shrink(10.0));
        assert!(caught.contains("market"), "{caught:?}");
        assert!(!caught.contains("chart"), "ไม่ได้ลากไปถึงกราฟ: {caught:?}");

        // A band that clips the corner of a card still counts it.
        let corner = Rect::from_min_size(market.max - Vec2::splat(4.0), Vec2::splat(40.0));
        assert!(board.cards_in(corner).contains("market"));

        // Far away from everything.
        let empty = Rect::from_min_size(egui::pos2(-9000.0, -9000.0), Vec2::splat(50.0));
        assert!(board.cards_in(empty).is_empty());
    }

    /// Dragging one chosen card drags the others the same distance.
    #[test]
    fn moving_one_selected_card_moves_the_whole_selection() {
        let mut board = Board::default();
        board.selected = ["market", "chart"].into_iter().collect();
        let market_before = board.slots["market"];
        let chart_before = board.slots["chart"];
        let book_before = board.slots["book"];

        // What `card::show` does to a dragged card, then hands back.
        let mut dragged = market_before;
        dragged.translate(Vec2::new(40.0, -25.0));
        board.put("market", dragged);

        let chart_now = board.slots["chart"];
        assert_eq!(chart_now.x, chart_before.x + 40.0);
        assert_eq!(chart_now.y, chart_before.y - 25.0);
        assert_eq!(board.slots["market"].x, market_before.x + 40.0);
        let book_now = board.slots["book"];
        assert_eq!(
            (book_now.x, book_now.y),
            (book_before.x, book_before.y),
            "การ์ดที่ไม่ได้เลือกต้องอยู่ที่เดิม"
        );
    }

    /// A card that is not in the selection takes nothing with it.
    #[test]
    fn dragging_an_unselected_card_leaves_the_selection_where_it_is() {
        let mut board = Board::default();
        board.selected = ["market", "chart"].into_iter().collect();
        let market_before = board.slots["market"];
        let book_before = board.slots["book"];

        let mut dragged = book_before;
        dragged.translate(Vec2::new(100.0, 100.0));
        board.put("book", dragged);

        assert_eq!(
            board.slots["market"].x, market_before.x,
            "ใบที่เลือกไว้ต้องไม่ขยับ"
        );
        assert_eq!(board.slots["book"].x, book_before.x + 100.0, "ใบที่ลากต้องขยับ");
    }

    /// Every card sits inside the bounds, and the bounds are what the
    /// canvas interaction is sized from.
    #[test]
    fn the_bounds_hold_every_card() {
        let board = Board::default();
        let bounds = board.bounds();
        for (id, slot) in &board.slots {
            assert!(bounds.contains_rect(slot.rect()), "{id} หลุดออกนอกขอบเขต");
        }
    }

    /// Is this point on a card? The question that decides whether the left
    /// button draws a band or pans the board.
    #[test]
    fn a_point_between_the_cards_is_not_on_a_card() {
        let board = Board::default();
        assert!(board.on_a_card(board.slots["market"].rect().center()));
        assert!(!board.on_a_card(egui::pos2(-500.0, -500.0)));
    }

    /// Draw one whole frame with account data in it.
    ///
    /// Six new cards, each reaching into a different shape of reply, is six
    /// chances to index past the end of a list or unwrap a field the
    /// exchange did not send. A card that panics takes the whole board down
    /// with it - in a browser that is a blank page with nothing in the
    /// console a student could act on.
    fn draw_one_frame(board: &mut Board) {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            board.cards(ui, 1.0);
        });
        output.textures_delta.clear();
    }

    #[test]
    fn the_account_cards_draw_with_real_shaped_data_in_them() {
        let mut board = Board::default();
        board.account_data.balance = Some(account::read_balance(&serde_json::json!({
            "total_equity": "9966.404007",
            "available_balance": "9966.404007",
            "unrealized_pnl": "-12.5",
            "initial_margin": "996.260248",
            "maintenance_margin": "0.0",
            "settle_currency": "USDT",
            "event_time_at": "2026-11-13T09:54:11Z"
        })));
        board.account_data.positions = account::read_positions(&serde_json::json!([{
            "instrument": "BTC_USDT_Perp",
            "size": "-0.145",
            "entry_price": "68714.0",
            "mark_price": "68800.0",
            "unrealized_pnl": "-12.5"
        }]));
        board.account_data.open_orders = account::read_orders(&serde_json::json!([{
            "legs": [{
                "instrument": "BTC_USDT_Perp",
                "size": "0.145",
                "limit_price": "68769.2",
                "is_buying_asset": true
            }],
            "state": { "status": "OPEN", "traded_size": ["0.0"] },
            "metadata": { "create_time_at": "2026-11-13T08:59:45Z" }
        }]));
        board.account_data.orders = board.account_data.open_orders.clone();
        board.account_data.fills = account::read_fills(&serde_json::json!([{
            "instrument": "BTC_USDT_Perp",
            "is_buyer": true,
            "is_taker": true,
            "size": "0.145",
            "price": "68836.7",
            "fee": "4.491594",
            "fee_rate": "0.045",
            "fee_currency": "USDT",
            "event_time_at": "2026-11-13T09:03:06Z"
        }]));
        board.account_data.closed = account::read_closed(&serde_json::json!([{
            "instrument": "BTC_USDT_Perp",
            "is_long": false,
            "closed_volume_base": "0.145",
            "entry_price": "68714.0",
            "exit_price": "68836.7",
            "realized_pnl": "-17.7915",
            "cumulative_fee": "8.975182",
            "status": "CLOSED",
            "open_time_at": "2026-11-13T09:00:50Z",
            "close_time_at": "2026-11-13T09:03:06Z"
        }]));
        draw_one_frame(&mut board);
    }

    #[test]
    fn the_account_cards_draw_when_there_is_no_key_at_all() {
        // The ordinary case for a student who has not connected anything:
        // every account card has to say so without falling over.
        let mut board = Board::default();
        for index in 0..account::Kind::ALL.len() {
            board.account_data.load[index] = Load::Missing("ยังไม่ได้ตั้งกุญแจ".into());
        }
        draw_one_frame(&mut board);
    }

    /// The wheel has to be taken before the scene is built, and this is the
    /// test that says so: the scene reads the same scroll a frame later, and
    /// if anything is left for it the whole board slides while the chart is
    /// being zoomed. Clicking a real mouse wheel is the only other way to
    /// find that out.
    #[test]
    fn the_wheel_zooms_the_chart_under_the_pointer_and_leaves_nothing_behind() {
        let ctx = ctx_with_wheel(3.0);
        let mut board = Board::default();
        board.hover = Some((Chart::Candles, 400));
        board.take_wheel(&ctx);

        assert!(board.candle_zoom.shown(400) < 400, "ล้อเมาส์บนกราฟต้องซูมกราฟ");
        let left = ctx.input(|i| i.smooth_scroll_delta.y);
        assert_eq!(left, 0.0, "ต้องไม่เหลือ scroll ให้ scene เอาไปเลื่อนบอร์ด");
    }

    /// A frame where nobody touched anything must leave the chart exactly
    /// as it was. Worth stating, because the zoom is driven from input the
    /// board reads behind the scene's back, and a leak there would creep
    /// the chart in on its own.
    #[test]
    fn a_frame_with_no_scrolling_leaves_the_chart_alone() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        output.textures_delta.clear();

        let mut board = Board::default();
        board.hover = Some((Chart::Candles, 400));
        for _ in 0..30 {
            board.take_wheel(&ctx);
        }
        assert!(
            board.candle_zoom.is_whole(),
            "ไม่มีใครหมุนล้อ กราฟต้องยังเป็น 400 แท่งเท่าเดิม"
        );
    }

    #[test]
    fn a_notch_split_across_frames_zooms_exactly_as_far_as_one_taken_whole() {
        // This is the property that matters: egui smooths one wheel notch
        // over however many frames it likes, and the board must not care.
        let whole = zoom_factor(50.0);
        let in_pieces = zoom_factor(20.0) * zoom_factor(18.0) * zoom_factor(12.0);
        assert!((whole - in_pieces).abs() < 1e-6, "{whole} vs {in_pieces}");
    }

    #[test]
    fn one_notch_is_one_step_of_zoom_not_a_dive_to_the_floor() {
        let mut zoom = crate::zoom::Zoom::default();
        zoom.zoom_by(zoom_factor(50.0), 400);
        let shown = zoom.shown(400);
        assert!(
            (300..400).contains(&shown),
            "คลิกเดียวควรซูมพอประมาณ ไม่ใช่เหลือ {shown} แท่ง"
        );
    }

    #[test]
    fn a_furious_scroll_is_still_bounded() {
        assert!(zoom_factor(100_000.0) > 0.0);
        assert!((zoom_factor(100_000.0) - 0.82_f32.powf(4.0)).abs() < 1e-6);
    }

    #[test]
    fn scrolling_down_zooms_back_out() {
        let ctx = ctx_with_wheel(-3.0);
        let mut board = Board::default();
        board.candle_zoom.zoom_by(0.3, 400);
        let before = board.candle_zoom.shown(400);
        board.hover = Some((Chart::Candles, 400));
        board.take_wheel(&ctx);
        assert!(board.candle_zoom.shown(400) > before);
    }

    #[test]
    fn a_card_still_holding_old_data_says_so_when_its_fetch_is_failing() {
        let (state, status) = stale_if_failed(
            &Load::Failed("เซิร์ฟเวอร์ตอบ 502".into()),
            theme::State::Ok,
            "12 รายการ".into(),
        );
        assert_eq!(state, theme::State::Bad);
        assert_eq!(status, "ข้อมูลค้าง");

        // A fetch that is fine, or a file that was never there, keep their
        // own words: "missing" already has a sentence inside the card.
        for load in [Load::Ready, Load::Loading, Load::Missing("ยังไม่มี".into())] {
            let (state, status) = stale_if_failed(&load, theme::State::Ok, "12 รายการ".into());
            assert_eq!(state, theme::State::Ok);
            assert_eq!(status, "12 รายการ");
        }
    }

    #[test]
    fn dragging_a_zoomed_chart_moves_the_chart_and_not_the_board() {
        let mut board = Board::default();
        assert!(!board.chart_takes_the_drag(), "ไม่ได้ชี้ที่กราฟ ลากคือเลื่อนบอร์ด");

        board.hover = Some((Chart::Candles, 400));
        assert!(
            !board.chart_takes_the_drag(),
            "กราฟที่เห็นครบทุกแท่งเลื่อนไปไหนไม่ได้ ปล่อยให้ลากบอร์ดดีกว่าลากแล้วไม่มีอะไรเกิด"
        );

        board.candle_zoom.zoom_by(0.5, 400);
        assert!(board.chart_takes_the_drag(), "ซูมแล้วลากต้องเลื่อนกราฟ");

        board.hover = Some((Chart::Equity, 300));
        assert!(!board.chart_takes_the_drag(), "เส้นทุนยังไม่ได้ซูม");
        board.equity_zoom.zoom_by(0.5, 300);
        assert!(board.chart_takes_the_drag());
    }

    #[test]
    fn the_wheel_elsewhere_on_the_board_is_left_for_the_board() {
        let ctx = ctx_with_wheel(3.0);
        let mut board = Board::default();
        board.hover = None;
        board.take_wheel(&ctx);
        assert!(board.candle_zoom.is_whole(), "ไม่ได้ชี้ที่กราฟ ก็ต้องไม่ซูมกราฟ");
        assert_ne!(
            ctx.input(|i| i.smooth_scroll_delta.y),
            0.0,
            "scroll ต้องตกถึง scene เพื่อเลื่อน/ซูมบอร์ดตามปกติ"
        );
    }

    #[test]
    fn the_equity_curve_has_its_own_window_separate_from_the_candles() {
        let ctx = ctx_with_wheel(3.0);
        let mut board = Board::default();
        board.hover = Some((Chart::Equity, 300));
        board.take_wheel(&ctx);
        assert!(board.equity_zoom.shown(300) < 300);
        assert!(board.candle_zoom.is_whole(), "ซูมเส้นทุนต้องไม่ไปขยับกราฟราคา");
    }

    #[test]
    fn every_wired_card_id_is_a_card_that_exists() {
        let board = Board::default();
        for (from, to) in WIRES {
            assert!(board.slots.contains_key(from), "ไม่มีการ์ดชื่อ {from}");
            assert!(board.slots.contains_key(to), "ไม่มีการ์ดชื่อ {to}");
        }
        for id in STAGES {
            assert!(board.slots.contains_key(id), "ไม่มีการ์ดของขั้น {id}");
        }
    }

    #[test]
    fn no_two_cards_are_stacked_on_top_of_each_other() {
        // A card hidden under another is a card nobody will ever read, and
        // the wires between them would run through solid panels.
        let board = Board::default();
        let slots: Vec<(&&str, Rect)> = board
            .slots
            .iter()
            .map(|(id, slot)| (id, slot.rect()))
            .collect();
        for (i, (a_id, a)) in slots.iter().enumerate() {
            for (b_id, b) in slots.iter().skip(i + 1) {
                let overlap = a.intersect(*b);
                assert!(
                    overlap.width() <= 0.0 || overlap.height() <= 0.0,
                    "การ์ด {a_id} กับ {b_id} ทับกัน"
                );
            }
        }
    }

    #[test]
    fn each_zone_button_really_frames_the_cards_it_promises() {
        let board = Board::default();
        let zones: [(usize, &[&str]); 5] = [
            (
                0,
                &[
                    "market", "funding", "info", "chart", "book", "trades", "depth", "feed",
                ],
            ),
            (1, &["s1", "s2", "s3", "s4", "s5", "account", "log", "code"]),
            (2, &["backtest", "quality", "equity", "notes"]),
            (
                3,
                &[
                    "balance",
                    "positions",
                    "open_orders",
                    "orders",
                    "closed",
                    "fills",
                    "paid_funding",
                ],
            ),
            (4, &["runs", "calendar"]),
        ];
        for (index, ids) in zones {
            let frame = view(index);
            for id in ids {
                let card = board.slots[id].rect();
                assert!(
                    frame.contains_rect(card),
                    "ปุ่ม {} ไม่ได้พาไปเห็นการ์ด {id}",
                    VIEWS[index].0
                );
            }
        }
    }

    // ----------------------------------------------- the code on the cards

    /// One tick, as the bot would have written it: the strategy ran, line 78
    /// went one way and line 80 went round three times.
    fn traced_row() -> bot::Row {
        bot::Row {
            at: "2026-11-13T18:00:00+00:00".to_string(),
            symbol: "BTC_USDT_Perp".to_string(),
            price: "100000".to_string(),
            equity: "9966.40".to_string(),
            trace: Some(bot::Trace {
                steps: 4,
                files: vec![PING_PONG.to_string()],
                // Line 78 once, then line 80 three times round a loop.
                path: vec![(0, 78), (0, 80), (0, 80), (0, 80)],
                calls: vec![bot::Call {
                    file: PING_PONG.to_string(),
                    func: "get_signal".to_string(),
                    line: 77,
                }],
                lines: [(
                    PING_PONG.to_string(),
                    [("78".to_string(), 1u64), ("80".to_string(), 3u64)]
                        .into_iter()
                        .collect(),
                )]
                .into_iter()
                .collect(),
                full: false,
            }),
            ..Default::default()
        }
    }

    const PING_PONG: &str = "tradebot/strategies/ping_pong.py";

    fn ping_pong() -> code::Source {
        code::Source {
            file: PING_PONG.to_string(),
            func: "get_signal".to_string(),
            first: 77,
            lines: vec![
                "def get_signal(self, market):".to_string(),
                "    if market.in_position:".to_string(),
                "        return None".to_string(),
                "    return self.buy(market)".to_string(),
            ],
        }
    }

    // --- the trip (ออกแบบโดย Claude คืน 14 พ.ย. 2026) ----------------------

    const SENT_AT: &str = "2026-11-13T19:20:02.750000+00:00";

    fn num(text: &str) -> Option<exchange::Num> {
        exchange::Num::from(Some(&serde_json::Value::String(text.into())))
    }

    fn sending_board(sent: &str) -> Board {
        let mut board = Board::default();
        board.bot.rows = vec![bot::Row {
            at: "2026-11-13T19:20:02.700000+00:00".into(),
            order: Some(bot::Order {
                side: "sell".into(),
                qty: "0.143".into(),
                limit_price: "68896.6".into(),
                sent_at: sent.into(),
                answered_ms: Some(380.0),
                ..Default::default()
            }),
            receipt: Some(bot::Receipt {
                broker_order_id: "0x00".into(),
                venue_id: "3163306331".into(),
                ..Default::default()
            }),
            ..Default::default()
        }];
        board.bot.stages = bot::stages_of(&board.bot.rows, None);
        board
    }

    fn on_the_exchange(status: &str, filled: &str, created: f64) -> account::Order {
        account::Order {
            size: num("0.143"),
            price: num("68896.6"),
            filled: num(filled),
            status: status.into(),
            client_order_id: "3163306331".into(),
            order_id: "0xabc".into(),
            created: Some(created),
            updated: Some(created + 0.3),
            ..Default::default()
        }
    }

    #[test]
    fn the_trip_card_follows_a_filled_order_to_the_end() {
        let mut board = sending_board(SENT_AT);
        let sent = crate::time::unix_of(SENT_AT).unwrap();
        board.account_data.orders = vec![on_the_exchange("FILLED", "0.143", sent + 0.1)];
        board.account_data.fills = vec![account::Fill {
            size: num("0.143"),
            price: num("68890"),
            is_taker: true,
            client_order_id: "3163306331".into(),
            when: Some(sent + 0.25),
            ..Default::default()
        }];
        let painted = painted_text(&mut board).join("\n");
        for wanted in [
            "เส้นทางออเดอร์",
            "จับคู่ครบทั้งก้อน",
            "380 ms",
            "taker",
            "ครบใน 250 ms",
        ] {
            assert!(painted.contains(wanted), "ไม่เห็น {wanted} · {painted}");
        }
    }

    #[test]
    fn the_send_stage_says_where_the_newest_order_has_got_to() {
        let mut board = sending_board(SENT_AT);
        let sent = crate::time::unix_of(SENT_AT).unwrap();
        board.account_data.orders = vec![on_the_exchange("CANCELLED", "0", sent + 0.1)];
        let painted = painted_text(&mut board).join("\n");
        assert!(painted.contains("ยกเลิก · ไม่ได้จับคู่เลย"), "{painted}");
    }

    #[test]
    fn an_order_waiting_in_the_book_counts_up_and_hurries_the_account() {
        let now = unix_now();
        let sent = crate::time::clock_iso(now - 5.0);
        let mut board = sending_board(&sent);
        board.account_data.open_orders = vec![on_the_exchange("OPEN", "0", now - 4.0)];
        assert!(board.newest_trip_is_live());
        let painted = painted_text(&mut board).join("\n");
        assert!(painted.contains("รอคิวในสมุด"), "{painted}");
        assert!(painted.contains("รอมาแล้ว"), "{painted}");
    }

    #[test]
    fn an_order_glows_while_it_travels_and_briefly_after_the_board_sees_it_land() {
        let board = sending_board(SENT_AT);
        let sent = crate::time::unix_of(SENT_AT).unwrap();
        let trip = journey::trips(&board.bot.rows).remove(0);
        let mut watch = None;

        // On its way: amber.
        watch_landing(&mut watch, &trip, &Phase::Looking, sent + 2.0);
        assert_eq!(
            trip_glow(&Phase::Looking, None, sent + 2.0).unwrap().0,
            theme::WARN
        );

        // The board hears of the fill five seconds after it happened - as on
        // testnet - and still shows it landing, from the moment it heard.
        watch_landing(&mut watch, &trip, &Phase::Filled, sent + 5.0);
        let landed = watch.as_ref().unwrap().2;
        assert_eq!(landed, Some(sent + 5.0));
        let (colour, strength) = trip_glow(&Phase::Filled, landed, sent + 5.5).unwrap();
        assert_eq!(colour, theme::OK);
        assert!(strength > 0.8);
        assert!(trip_glow(&Phase::Filled, landed, sent + 5.0 + LANDED_GLOW + 0.1).is_none());

        // Staying filled does not restart the glow.
        watch_landing(&mut watch, &trip, &Phase::Filled, sent + 30.0);
        assert_eq!(watch.as_ref().unwrap().2, Some(sent + 5.0));
    }

    #[test]
    fn opening_the_board_on_an_old_fill_does_not_light_it_up() {
        let board = sending_board(SENT_AT);
        let sent = crate::time::unix_of(SENT_AT).unwrap();
        let trip = journey::trips(&board.bot.rows).remove(0);
        let mut watch = None;
        watch_landing(&mut watch, &trip, &Phase::Filled, sent + 3600.0);
        assert_eq!(watch.as_ref().unwrap().2, None);

        // But a fill that beat the board by a second or two still counts.
        let mut watch = None;
        watch_landing(&mut watch, &trip, &Phase::Filled, sent + 3.0);
        assert_eq!(watch.as_ref().unwrap().2, Some(sent + 3.0));
    }

    #[test]
    fn with_no_order_yet_the_trip_card_says_what_it_will_show() {
        let mut board = Board::default();
        let painted = painted_text(&mut board).join("\n");
        assert!(painted.contains("ยังไม่ได้ส่ง"), "{painted}");
        assert!(!board.newest_trip_is_live());
    }

    #[test]
    fn stepping_to_an_older_order_shows_that_one() {
        let mut board = sending_board(SENT_AT);
        let mut older = board.bot.rows[0].clone();
        older.at = "2026-11-13T19:10:00+00:00".into();
        older.order.as_mut().unwrap().side = "buy".into();
        board.bot.rows.insert(0, older.clone());
        board.trip_pick = Some(older.at.clone());
        let (trip, _, _, index, count) = board.trip_now().unwrap();
        assert!(trip.is_buy);
        assert_eq!((index, count), (0, 2));
        // A replay of the newer tick wins over the pick.
        board.replay = Some(board.bot.rows[1].at.clone());
        assert!(!board.trip_now().unwrap().0.is_buy);
    }

    /// A board watching a bot that just ran the strategy.
    fn watching() -> Board {
        let mut board = Board::default();
        board.bot.rows = vec![traced_row()];
        board.bot.stages = bot::stages_of(&board.bot.rows, None);
        board.code.hold((PING_PONG.to_string(), 77), ping_pong());
        board
    }

    /// The whole point: a card that shows the code, not a sentence about it.
    #[test]
    fn a_stage_card_prints_the_line_of_python_that_really_ran() {
        let mut board = watching();
        let painted = painted_text(&mut board).join("\n");
        assert!(
            painted.contains("if market.in_position:"),
            "การ์ดกลยุทธ์ต้องพิมพ์บรรทัดจริงที่เดิน · {painted}"
        );
    }

    /// The branch that was *not* taken is half of what a reader wants.
    #[test]
    fn the_code_card_shows_the_lines_that_did_not_run_as_well() {
        let mut board = watching();
        let painted = painted_text(&mut board).join("\n");
        assert!(
            painted.contains("return None"),
            "บรรทัดที่ไม่ได้เดินต้องยังอยู่ · {painted}"
        );
    }

    /// A loop going round three times is the one thing a listing cannot show.
    #[test]
    fn a_line_that_ran_more_than_once_says_how_many_times() {
        let mut board = watching();
        let painted = painted_text(&mut board).join("\n");
        assert!(painted.contains("×3"), "{painted}");
    }

    /// Counted against the function, so "2/4" and not "2 lines".
    #[test]
    fn the_card_says_how_much_of_the_function_ran() {
        let mut board = watching();
        let painted = painted_text(&mut board).join("\n");
        assert!(painted.contains("เดิน 2/4 บรรทัด"), "{painted}");
    }

    /// The code card opens on the strategy, and says whose code it is.
    #[test]
    fn the_code_card_names_the_file_it_is_showing() {
        let mut board = watching();
        let painted = painted_text(&mut board).join("\n");
        assert!(painted.contains(PING_PONG), "{painted}");
        assert!(painted.contains("get_signal()"), "{painted}");
    }

    /// A board opened before the bot has ever run - which is every board,
    /// the first time somebody opens one. The code is still the code, so it
    /// is still shown; only the lit lines are missing.
    #[test]
    fn with_no_trace_the_card_shows_the_code_the_config_says_it_uses() {
        let mut board = Board::default();
        board.code.hold((PING_PONG.to_string(), 77), ping_pong());
        board.code.hold_plan(vec![
            bot::Call::default(),
            bot::Call {
                file: PING_PONG.to_string(),
                func: "get_signal".to_string(),
                line: 77,
            },
        ]);
        let painted = painted_text(&mut board).join("\n");
        assert!(
            painted.contains("if market.in_position:"),
            "โค้ดต้องขึ้นตั้งแต่ยังไม่ได้รัน · {painted}"
        );
        assert!(
            painted.contains("ยังไม่ได้รัน"),
            "แต่ต้องบอกด้วยว่ายังไม่ได้รัน ไม่ใช่ปล่อยให้เข้าใจว่าเดินแล้ว · {painted}"
        );
    }

    /// Nothing to show is a broken board, so it says which setting is wrong
    /// rather than going quiet.
    #[test]
    fn a_stage_with_no_file_at_all_says_which_setting_to_check() {
        let mut board = Board::default();
        board.bot.rows = vec![bot::Row {
            at: "2026-11-13T18:00:00+00:00".to_string(),
            ..Default::default()
        }];
        board.bot.stages = bot::stages_of(&board.bot.rows, None);
        let painted = painted_text(&mut board).join("\n");
        assert!(
            painted.contains("ยังไม่รู้ว่าขั้นนี้ใช้ไฟล์ไหน"),
            "ต้องบอกว่าทำไมไม่มีโค้ด ไม่ใช่เงียบ · {painted}"
        );
    }

    /// Pressing a stage number sends that stage to the code card. Driven
    /// through the state the button writes, because a click in a headless
    /// frame lands on a card that has not been laid out yet.
    #[test]
    fn the_code_card_follows_the_stage_that_was_picked() {
        let mut board = watching();
        board.code_stage = 2;
        let painted = painted_text(&mut board).join("\n");
        assert!(
            painted.contains("ด่านความเสี่ยง"),
            "หัวการ์ดโค้ดต้องเปลี่ยนตามขั้นที่เลือก · {painted}"
        );
        // The strategy card above still shows its own strip; what must
        // change is the wide card, which prints the full path of the file
        // it has open.
        assert!(
            !painted.contains(PING_PONG),
            "การ์ดโค้ดต้องไม่ค้างอยู่ที่ไฟล์ของขั้นเดิม · {painted}"
        );
        assert!(
            painted.contains("ยังไม่รู้ว่าขั้นนี้ใช้ไฟล์ไหน"),
            "และต้องบอกว่าทำไมถึงไม่มีอะไรให้ดู · {painted}"
        );
    }

    /// A tick that lands brightens the lines it ran, and the brightness
    /// fades. Both halves matter: without the fade every line stays lit and
    /// a bot that stopped looks like one that is working.
    #[test]
    fn a_tick_that_just_landed_is_brighter_than_one_from_a_minute_ago() {
        let mut board = watching();
        board.mark_tick(100.0);
        assert!(board.flash(100.0) > 0.9, "เพิ่งเดินต้องสว่าง");
        assert_eq!(board.flash(100.0 + FLASH), 0.0, "ผ่านไปแล้วต้องดับ");
        assert_eq!(board.flash(99.0), 0.0, "นาฬิกาเดินถอยหลังไม่ควรสว่างค้าง");
    }

    /// The clock the board watches is its own, and the row it watches is the
    /// newest one. A row that has not changed must not re-light the board.
    #[test]
    fn the_same_row_seen_again_does_not_count_as_a_new_tick() {
        let mut board = watching();
        board.mark_tick(100.0);
        board.mark_tick(140.0);
        assert_eq!(board.seen_at, 100.0, "แถวเดิมไม่ใช่รอบใหม่");

        board.bot.rows = vec![bot::Row {
            at: "2026-11-13T18:00:05+00:00".to_string(),
            ..Default::default()
        }];
        board.mark_tick(150.0);
        assert_eq!(board.seen_at, 150.0, "แถวใหม่คือรอบใหม่");
    }

    // ------------------------------------------- why nothing is lit up yet

    /// A plan for the strategy card, and the source behind it.
    fn plan_for_the_strategy() -> Vec<bot::Call> {
        vec![
            bot::Call::default(),
            bot::Call {
                file: PING_PONG.to_string(),
                func: "get_signal".to_string(),
                line: 77,
            },
        ]
    }

    fn untraced_rows() -> Vec<bot::Row> {
        vec![bot::Row {
            at: "2026-11-13T18:00:00+00:00".to_string(),
            symbol: "BTC_USDT_Perp".to_string(),
            price: "100000".to_string(),
            equity: "9966.40".to_string(),
            ..Default::default()
        }]
    }

    #[test]
    fn a_board_watching_nothing_says_the_bot_has_not_run() {
        let mut board = Board::default();
        board.code.hold((PING_PONG.to_string(), 77), ping_pong());
        board.code.hold_plan(plan_for_the_strategy());
        let painted = painted_text(&mut board).join("\n");
        assert!(painted.contains("ยังไม่ได้รัน"), "{painted}");
    }

    /// The bug this exists to prevent: a card saying "has not run" next to a
    /// card counting eighteen signals. The bot ran; the board cannot see
    /// which lines it took, which is a different sentence.
    #[test]
    fn a_bot_that_is_running_without_recording_is_not_called_a_bot_that_never_ran() {
        let mut board = Board::default();
        board.bot.rows = untraced_rows();
        board.bot.stages = bot::stages_of(&board.bot.rows, None);
        board.code.hold((PING_PONG.to_string(), 77), ping_pong());
        board.code.hold_plan_from_bot(plan_for_the_strategy());
        let painted = painted_text(&mut board).join("\n");
        assert!(
            !painted.contains("ยังไม่ได้รัน"),
            "บอทรันอยู่ ห้ามบอกว่ายังไม่ได้รัน · {painted}"
        );
        assert!(painted.contains("บอทยังไม่ได้จด"), "{painted}");
        assert!(
            painted.contains("ปิดแล้วรันใหม่"),
            "ต้องบอกด้วยว่าทำอะไรถึงจะหาย · {painted}"
        );
    }

    /// And when the file on screen is the config file's guess rather than
    /// the running bot's own answer, the card says that too - because the
    /// guess can be the wrong strategy entirely.
    #[test]
    fn a_file_that_is_only_a_guess_from_the_config_is_labelled_as_one() {
        let mut board = Board::default();
        board.bot.rows = untraced_rows();
        board.bot.stages = bot::stages_of(&board.bot.rows, None);
        board.code.hold((PING_PONG.to_string(), 77), ping_pong());
        board.code.hold_plan(plan_for_the_strategy());
        let painted = painted_text(&mut board).join("\n");
        assert!(
            painted.contains("เดามาจากคอนฟิก"),
            "ต้องเตือนว่าอาจเป็นคนละกลยุทธ์ · {painted}"
        );
    }

    /// Once a trace arrives none of that is said at all.
    #[test]
    fn a_traced_tick_says_what_ran_rather_than_why_nothing_did() {
        let mut board = watching();
        let painted = painted_text(&mut board).join("\n");
        assert!(!painted.contains("บอทยังไม่ได้จด"), "{painted}");
        assert!(!painted.contains("ยังไม่ได้รัน"), "{painted}");
        assert!(painted.contains("เดิน 2/4 บรรทัด"), "{painted}");
    }

    // ------------------------------------------------------- the replay

    /// The head is a function of the clock, so it never needs resetting and
    /// every card is at the same moment of the same tick.
    #[test]
    fn the_head_walks_a_line_at_a_time_and_comes_back_round() {
        let mut board = watching();
        board.mark_tick(1.0);
        assert_eq!(board.head(1.0, 4), 0);
        assert_eq!(board.head(1.0 + 1.0 / WALK_RATE, 4), 1);
        assert_eq!(board.head(1.0 + 3.0 / WALK_RATE, 4), 3);
        assert_eq!(
            board.head(1.0 + 4.0 / WALK_RATE, 4),
            0,
            "จบ path แล้วต้องวนกลับไปเริ่มใหม่ ไม่ใช่ค้างที่บรรทัดสุดท้าย"
        );
    }

    /// Four steps: line 78 once, then line 80 three times round a loop.
    #[test]
    fn the_card_says_which_line_the_replay_is_on_and_which_step_that_is() {
        let mut board = watching();
        board.mark_tick(1.0);
        let first = painted_at(&mut board, 1.0).join("\n");
        assert!(first.contains("บรรทัด 78 · 1/4 ก้าว"), "{first}");
        let second = painted_at(&mut board, 1.0 + 1.0 / WALK_RATE).join("\n");
        assert!(second.contains("บรรทัด 80 · 2/4 ก้าว"), "{second}");
    }

    /// A loop that comes back to the same line is three steps, not one.
    #[test]
    fn going_round_a_loop_is_more_than_one_step() {
        let board = watching();
        let source = ping_pong();
        let call = bot::Call {
            file: PING_PONG.to_string(),
            func: "get_signal".to_string(),
            line: 77,
        };
        let walk = board.walk_of(&call, &source);
        assert_eq!(walk.steps, vec![78, 80, 80, 80]);
        assert!(walk.ordered, "ลำดับมาจาก path ที่บอทจดจริง");
    }

    #[test]
    fn pausing_stops_the_replay_where_it_stands() {
        let mut board = watching();
        board.mark_tick(1.0);
        board.paused_at = Some(1.0 + 2.0 / WALK_RATE);
        assert_eq!(board.head(99.0, 4), 2, "เวลาเดินต่อ แต่หัวอ่านต้องไม่ไป");
        assert!(!board.walking(), "หยุดแล้วไม่ต้องรีเพนต์ถี่");
    }

    /// An audit trail written before the bot recorded order still walks -
    /// top to bottom through the lines that ran - and says plainly that the
    /// order is not the real one. Before this it sat still, which a person
    /// reported as "the highlight does not move".
    #[test]
    fn a_trail_with_no_recorded_order_walks_top_to_bottom_and_says_so() {
        let mut board = watching();
        if let Some(trace) = board.bot.rows.last_mut().and_then(|row| row.trace.as_mut()) {
            trace.path.clear();
            trace.files.clear();
        }
        assert!(board.walking(), "ยังต้องเดิน ไม่ใช่นิ่ง");
        board.mark_tick(1.0);
        let first = painted_at(&mut board, 1.0).join("\n");
        assert!(first.contains("บรรทัด 78 · บนลงล่าง 1/2"), "{first}");
        let second = painted_at(&mut board, 1.0 + 1.0 / WALK_RATE).join("\n");
        assert!(second.contains("บรรทัด 80 · บนลงล่าง 2/2"), "{second}");
        assert!(
            first.contains("ยังไม่ได้จดลำดับ"),
            "ต้องบอกว่าไม่ใช่ลำดับจริง · {first}"
        );
        assert!(!first.contains("ก้าว"), "ไม่ใช่ก้าวจริง ห้ามเรียกว่าก้าว · {first}");
    }

    #[test]
    fn the_line_under_the_head_is_lit_fully_and_the_lines_behind_it_fade() {
        let glow = glow_of(&[10, 11, 12, 13, 14], 4);
        assert_eq!(glow[&14], 1.0);
        assert!(glow[&13] < 1.0 && glow[&13] > glow[&12], "จางลงตามระยะ");
        assert!(glow[&12] > glow[&11]);
        assert!(!glow.contains_key(&10), "หางยาวแค่สี่ก้าว");
    }

    #[test]
    fn the_trail_does_not_wrap_round_to_light_code_that_has_not_run_yet() {
        let glow = glow_of(&[10, 11, 12, 13], 0);
        assert_eq!(glow.len(), 1, "ก้าวแรกไม่มีอะไรอยู่ข้างหลัง");
        assert_eq!(glow[&10], 1.0);
    }

    #[test]
    fn a_tight_loop_keeps_the_brighter_of_the_two_lights() {
        // 20, 21, 20: line 20 is both under the head and two steps back.
        let glow = glow_of(&[20, 21, 20], 2);
        assert_eq!(glow[&20], 1.0);
    }

    // --------------------------------------------------------- the history

    fn a_run() -> history::Run {
        history::Run {
            id: "aaa".into(),
            started_at: "2026-11-13T16:47:00+00:00".into(),
            ended_at: "2026-11-13T16:48:01+00:00".into(),
            strategy: "ping_pong".into(),
            exchange: "grvt".into(),
            mode: "grvt testnet".into(),
            ticks: 10,
            orders: 1,
            exits: 1,
            change: Some(-8.961703),
            finished: true,
            ..Default::default()
        }
    }

    #[test]
    fn the_history_card_says_when_a_run_was_what_it_ran_and_what_it_made() {
        let mut board = Board::default();
        board.past.runs = vec![a_run()];
        let painted = painted_text(&mut board).join("\n");
        assert!(painted.contains("23:47"), "เวลาไทยของ 16:47Z · {painted}");
        assert!(painted.contains("61 วิ"), "{painted}");
        assert!(painted.contains("ping_pong · grvt testnet"), "{painted}");
        assert!(painted.contains("10 รอบ · 1 ออเดอร์ · -8.96"), "{painted}");
    }

    /// A killed run has no counts. Printing "0 รอบ" would be a measurement
    /// of something nobody measured.
    #[test]
    fn a_run_that_never_finished_says_so_rather_than_showing_zeroes() {
        let mut board = Board::default();
        board.past.runs = vec![history::Run {
            finished: false,
            ticks: 0,
            ..a_run()
        }];
        let painted = painted_text(&mut board).join("\n");
        assert!(painted.contains("ยังไม่จบ"), "{painted}");
        assert!(!painted.contains("0 รอบ"), "{painted}");
    }

    #[test]
    fn a_profit_keeps_its_plus_sign_so_it_cannot_be_read_as_a_loss() {
        assert_eq!(money(12.4), "+12.40");
        assert_eq!(money(-12.4), "-12.40");
        assert_eq!(money(0.0), "+0.00");
    }

    #[test]
    fn a_run_is_timed_in_units_a_person_uses() {
        assert_eq!(length(61), "61 วิ");
        assert_eq!(length(600), "10 นาที");
        assert_eq!(length(8000), "2 ชม. 13 นาที");
    }

    fn a_day(date: &str, change: Option<f64>) -> history::Day {
        history::Day {
            date: date.into(),
            ticks: 166,
            orders: 2,
            change,
            first_equity: "9966.40".into(),
            last_equity: "9957.44".into(),
            ..Default::default()
        }
    }

    #[test]
    fn the_calendar_opens_on_the_month_the_bot_last_ran_in() {
        let mut board = Board::default();
        board.past.days = vec![a_day("2026-11-13", Some(-8.96))];
        let painted = painted_text(&mut board).join("\n");
        assert!(painted.contains("พฤศจิกายน 2026"), "{painted}");
        assert!(painted.contains("วันล่าสุด 2026-11-13"), "{painted}");
        assert!(painted.contains("2 ออเดอร์ · -8.96"), "{painted}");
    }

    /// Thirty squares, numbered, whatever the data says.
    #[test]
    fn the_calendar_draws_the_whole_month_not_only_the_days_with_data() {
        let mut board = Board::default();
        board.past.days = vec![a_day("2026-11-13", Some(-8.96))];
        let painted = painted_text(&mut board).join("\n");
        for day in ["1", "15", "30"] {
            assert!(painted.contains(day), "ขาดช่องวันที่ {day} · {painted}");
        }
    }

    /// A day nobody measured is not a flat day.
    #[test]
    fn a_day_with_no_measurement_prints_no_number_at_all() {
        let mut board = Board::default();
        board.past.days = vec![a_day("2026-11-13", None)];
        let painted = painted_text(&mut board).join("\n");
        assert!(painted.contains("วัดไม่ได้"), "{painted}");
        assert!(!painted.contains("+0"), "{painted}");
    }

    #[test]
    fn a_broken_escape_is_kept_rather_than_eaten() {
        assert_eq!(percent_decode("bot"), "bot");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%E0%B8%9A"), "บ");
    }

    #[test]
    fn a_link_can_open_the_board_on_any_zone_by_either_name() {
        let mut board = Board::default();
        assert!(board.open_at("bot"));
        assert_eq!(board.looking_at(), Some(1));
        assert!(board.open_at("#history"), "a URL fragment works as it is");
        assert_eq!(board.looking_at(), Some(4));
        assert!(board.open_at("บัญชีจริง"), "and so does the Thai label");
        assert_eq!(board.looking_at(), Some(3));
        assert!(
            board.open_at("#%E0%B8%9A%E0%B8%AD%E0%B8%97"),
            "the Thai label as a browser actually sends it"
        );
        assert_eq!(board.looking_at(), Some(1));
        let before = board.scene_rect;
        assert!(!board.open_at("nowhere"));
        assert_eq!(board.scene_rect, before, "an unknown name changes nothing");
    }

    #[test]
    fn the_nav_bar_marks_the_zone_the_board_is_looking_at() {
        let mut board = Board::default();
        assert_eq!(board.looking_at(), Some(0), "เปิดมาที่โซนตลาด");
        board.scene_rect = view(4);
        assert_eq!(board.looking_at(), Some(4));
        // Out in the empty space between zones, no button is lit rather
        // than the nearest one being lit wrongly.
        board.scene_rect = Rect::from_min_size(egui::pos2(9000.0, 9000.0), egui::vec2(10.0, 10.0));
        assert_eq!(board.looking_at(), None);
    }

    // ----------------------------------------- what the screenshot caught

    #[test]
    fn the_scroll_puts_the_head_in_the_middle_of_the_card() {
        // Row 20 of 13-pixel rows in a 130-pixel viewport: the middle of the
        // row sits at 266.5, so the view starts 65 above that.
        assert_eq!(follow_offset(20, 13.0, 130.0), 266.5 - 65.0);
    }

    #[test]
    fn the_first_rows_sit_at_the_top_rather_than_in_empty_space() {
        assert_eq!(follow_offset(0, 13.0, 130.0), 0.0);
        assert_eq!(follow_offset(3, 13.0, 130.0), 0.0);
    }

    /// A tick with no signal never reaches the gate. The gate card must say
    /// that - "this tick stopped before here" - and not that the bot failed
    /// to record anything, which is what it said when the screenshot was
    /// taken.
    #[test]
    fn a_stage_the_tick_did_not_reach_says_so_rather_than_blaming_the_bot() {
        let mut board = watching();
        board.code.hold(
            ("tradebot/risk.py".to_string(), 91),
            code::Source {
                file: "tradebot/risk.py".into(),
                func: "risk_gate".into(),
                first: 91,
                lines: vec!["def risk_gate(signal):".into(), "    return None".into()],
            },
        );
        board.code.hold_plan_from_bot(vec![
            bot::Call::default(),
            bot::Call::default(),
            bot::Call {
                file: "tradebot/risk.py".into(),
                func: "risk_gate".into(),
                line: 91,
            },
        ]);
        let painted = painted_text(&mut board).join("\n");
        assert!(painted.contains("รอบนี้ไม่ได้มาถึง"), "{painted}");
        assert!(!painted.contains("บอทยังไม่ได้จด"), "บอทจดแล้ว · {painted}");
    }

    #[test]
    fn the_send_order_card_never_shows_the_market_read_in_its_place() {
        // A quiet tick touched the broker only to read the price. Showing
        // that function under "send order" says the bot sent something.
        let calls = vec![
            bot::Call {
                file: "tradebot/brokers/grvt.py".into(),
                func: "fetch_mark_price".into(),
                line: 375,
            },
            bot::Call {
                file: "tradebot/core/engine.py".into(),
                func: "tick".into(),
                line: 227,
            },
        ];
        let picked = code::candidates(3, &calls);
        assert!(
            picked.iter().all(|call| call.func != "fetch_mark_price"),
            "{picked:?}"
        );
    }

    #[test]
    fn a_narrow_card_drops_the_indentation_every_line_shares() {
        let source = code::Source {
            file: "x.py".into(),
            func: "f".into(),
            first: 1,
            lines: vec![
                "    def f(self):".into(),
                "        if x:".into(),
                "".into(),
                "            return None".into(),
            ],
        };
        assert_eq!(
            source.indent(),
            4,
            "the def's own indent, blank lines ignored"
        );
    }

    // ------------------------------------------------------------ replay

    /// A trail of three ticks: quiet, an order, quiet again. The newest is
    /// quiet, which is exactly why the order could never be seen.
    fn three_ticks() -> Board {
        let mut board = Board::default();
        let quiet = |at: &str| bot::Row {
            at: at.to_string(),
            symbol: "BTC_USDT_Perp".into(),
            price: "100000".into(),
            equity: "9966.40".into(),
            trace: traced_row().trace,
            ..Default::default()
        };
        let mut order = quiet("2026-11-13T16:47:11+00:00");
        order.signal = Some(bot::Signal {
            side: "buy".into(),
            ref_price: "68621.4".into(),
            stop_price: "68278.6".into(),
            reason: "below the average".into(),
        });
        order.order = Some(bot::Order {
            side: "buy".into(),
            qty: "0.145".into(),
            limit_price: "68621.4".into(),
            risk_amount: "49.68".into(),
            ..Default::default()
        });
        board.bot.rows = vec![
            quiet("2026-11-13T16:47:06+00:00"),
            order,
            quiet("2026-11-13T16:47:16+00:00"),
        ];
        board.bot.stages = bot::stages_of(&board.bot.rows, None);
        board
    }

    #[test]
    fn the_cards_follow_the_newest_tick_until_told_otherwise() {
        let board = three_ticks();
        assert!(!board.replaying());
        assert_eq!(board.shown().unwrap().0, 2);
    }

    /// The thing that was missing: the order tick, on the cards.
    #[test]
    fn the_newest_order_can_be_put_back_on_the_cards() {
        let mut board = three_ticks();
        let at = board
            .last_order_tick()
            .expect("there is an order in the trail");
        board.replay_at(Some(at));
        assert!(board.replaying());
        assert_eq!(board.shown().unwrap().0, 1);
        let stages = board.stages_now();
        assert_eq!(
            stages[1].metric, "buy",
            "the strategy card shows the signal"
        );
        assert_eq!(
            stages[2].metric, "0.145",
            "the gate card shows the approval"
        );
    }

    #[test]
    fn a_replay_says_so_where_the_time_is_shown() {
        let mut board = three_ticks();
        board.replay_at(board.last_order_tick());
        let painted = painted_text(&mut board).join("\n");
        assert!(
            painted.contains("tick 23:47:11 · 2. กลยุทธ์"),
            "the code card names the tick, in Thai time · {painted}"
        );
    }

    #[test]
    fn stepping_walks_one_tick_at_a_time_and_stops_at_the_ends() {
        let mut board = three_ticks();
        board.step_tick(-1);
        assert_eq!(board.shown().unwrap().0, 1);
        board.step_tick(-1);
        assert_eq!(board.shown().unwrap().0, 0);
        board.step_tick(-1);
        assert_eq!(board.shown().unwrap().0, 0, "nothing before the first");
        board.step_tick(1);
        board.step_tick(1);
        assert!(
            !board.replaying(),
            "stepping onto the newest is following again"
        );
    }

    /// The trail is re-read every two seconds and grows. A replay is a
    /// tick, so a new tick arriving must not move it - nor restart its walk.
    #[test]
    fn a_new_tick_arriving_does_not_move_a_replay() {
        let mut board = three_ticks();
        board.replay_at(board.last_order_tick());
        board.mark_tick(10.0);
        let mut fresh = board.bot.rows[2].clone();
        fresh.at = "2026-11-13T16:47:21+00:00".into();
        board.bot.rows.push(fresh);
        board.mark_tick(11.0);
        assert_eq!(board.shown().unwrap().0, 1, "still the order tick");
        assert_eq!(board.seen_at, 10.0, "and the walk was not restarted");
    }

    /// A replayed tick that has fallen off the front of the trail is gone;
    /// the cards go back to the newest rather than showing nothing.
    #[test]
    fn a_replay_of_a_tick_no_longer_in_the_trail_falls_back_to_the_newest() {
        let mut board = three_ticks();
        board.replay_at(Some("2026-11-01T00:00:00+00:00".into()));
        assert!(!board.replaying());
        assert_eq!(board.shown().unwrap().0, 2);
    }

    #[test]
    fn the_trip_link_frames_the_send_stage_and_the_trip_together() {
        let mut board = Board::default();
        assert!(board.open_at("#trip"));
        let framed = board.scene_rect;
        assert!(framed.contains_rect(board.slot("s4").rect()));
        assert!(framed.contains_rect(board.slot("trip").rect()));
        assert!(
            framed.width() < view(1).width(),
            "closer than the whole pipeline, or it is not a focus"
        );
        assert!(board.open_at(
            "%E0%B8%AA%E0%B9%88%E0%B8%87%E0%B8%84%E0%B8%B3%E0%B8%AA%E0%B8%B1%E0%B9%88%E0%B8%87"
        ));
    }

    #[test]
    fn the_order_link_opens_the_pipeline_on_the_newest_order() {
        let mut board = three_ticks();
        assert!(board.open_at("#order"));
        assert_eq!(board.looking_at(), Some(1), "the pipeline zone");
        assert!(board.replay_order_on_load, "waiting for the trail");
        // What `ui` does once the trail has arrived.
        board.replay_order_on_load = false;
        board.replay_at(board.last_order_tick());
        assert_eq!(board.shown().unwrap().0, 1, "the order tick");
    }

    /// The real server sends rows without their traces. The cards must
    /// walk exactly the same once the trace has been fetched separately.
    #[test]
    fn a_slim_row_walks_once_its_trace_has_been_fetched() {
        let mut board = watching();
        let row = board.bot.rows.last_mut().unwrap();
        let trace = row.trace.take().unwrap();
        row.traced = true;
        let at = row.at.clone();
        assert!(board.trace().is_none(), "not here yet");
        assert_eq!(board.why_quiet(), Quiet::Loading);
        board.traces.hold(&at, trace);
        assert!(board.trace().is_some());
        let painted = painted_text(&mut board).join(
            "
",
        );
        assert!(painted.contains("บรรทัด 78 · 1/4 ก้าว"), "{painted}");
    }

    #[test]
    fn a_trace_the_server_no_longer_has_is_not_waited_for_for_ever() {
        let mut board = watching();
        let row = board.bot.rows.last_mut().unwrap();
        row.trace = None;
        row.traced = true;
        let at = row.at.clone();
        board.traces.forget_for_test(&at);
        assert_eq!(board.why_quiet(), Quiet::TraceGone);
    }

    #[test]
    fn choosing_the_newest_tick_is_the_same_as_following() {
        let mut board = three_ticks();
        let newest = board.bot.rows[2].at.clone();
        board.replay_at(Some(newest));
        assert_eq!(board.replay, None);
    }

    // ---------------------------------------------------- a run, opened

    fn the_order_run() -> history::Run {
        history::Run {
            started_at: "2026-11-13T16:47:00+00:00".into(),
            ended_at: "2026-11-13T16:48:00+00:00".into(),
            strategy: "ping_pong".into(),
            finished: true,
            ticks: 3,
            orders: 1,
            ..Default::default()
        }
    }

    #[test]
    fn pressing_a_run_that_traded_replays_its_trade() {
        let mut board = three_ticks();
        board.open_run(&the_order_run());
        assert_eq!(board.looking_at(), Some(1), "the pipeline zone");
        assert_eq!(board.shown().unwrap().0, 1, "the order tick");
        assert!(board.run_note.is_none());
    }

    #[test]
    fn a_run_older_than_the_trail_says_so_instead_of_replaying_another() {
        let mut board = three_ticks();
        let before = board.scene_rect;
        board.open_run(&history::Run {
            started_at: "2026-11-04T10:00:00+00:00".into(),
            ended_at: "2026-11-04T10:05:00+00:00".into(),
            ..the_order_run()
        });
        assert!(!board.replaying());
        assert_eq!(board.scene_rect, before, "stayed where it was");
        assert!(board.run_note.as_deref().unwrap_or("").contains("เก่ากว่า"));
    }

    #[test]
    fn a_run_worked_out_from_the_trail_is_labelled_as_one() {
        let mut board = Board::default();
        board.past.runs = vec![history::Run {
            inferred: true,
            strategy: String::new(),
            mode: "live".into(),
            ..a_run()
        }];
        let painted = painted_text(&mut board).join("\n");
        assert!(painted.contains("? · live · เดาจาก audit"), "{painted}");
    }

    #[test]
    fn a_run_with_no_receipt_says_so_rather_than_leaving_a_gap() {
        let mut board = Board::default();
        board.past.runs = vec![history::Run {
            inferred: true,
            mode: String::new(),
            ..a_run()
        }];
        let painted = painted_text(&mut board).join(
            "
",
        );
        assert!(painted.contains("ไม่มีใบยืนยัน · เดาจาก audit"), "{painted}");
        assert!(!painted.contains("·  ·"), "{painted}");
    }

    #[test]
    fn a_run_covers_its_own_ticks_and_nobody_else_s() {
        let run = the_order_run();
        assert!(run.covers("2026-11-13T16:47:11+00:00"));
        assert!(
            run.covers("2026-11-13T23:47:30+07:00"),
            "any spelling of the moment"
        );
        assert!(!run.covers("2026-11-13T16:49:00+00:00"));
        let open = history::Run {
            ended_at: String::new(),
            ..run
        };
        assert!(
            open.covers("2026-11-14T00:00:00+00:00"),
            "still going covers what follows"
        );
    }

    // -------------------------------------------------------- paid funding

    #[test]
    fn the_funding_card_prints_a_payment_as_money_out() {
        let mut board = Board::default();
        board.account_data.funding = account::read_funding(&serde_json::json!([{
            "event_time_at": "2026-11-13T16:00:00Z",
            "instrument": "BTC_USDT_Perp",
            "currency": "USDT",
            "amount": "0.4213"
        }]));
        let painted = painted_text(&mut board).join("\n");
        assert!(painted.contains("จ่าย -0.42 USDT"), "{painted}");
        assert!(painted.contains("23:00:00"), "เวลาไทย · {painted}");
    }

    /// No cards overlap in a zone: the paid-funding card was fitted in by
    /// shortening the order history, and the two must not sit on each other.
    #[test]
    fn the_account_zone_cards_do_not_overlap() {
        let board = Board::default();
        let ids = [
            "balance",
            "positions",
            "open_orders",
            "orders",
            "closed",
            "fills",
            "paid_funding",
        ];
        for (index, a) in ids.iter().enumerate() {
            for b in &ids[index + 1..] {
                let (ra, rb) = (board.slots[a].rect(), board.slots[b].rect());
                assert!(!ra.intersects(rb), "{a} ทับ {b}");
            }
        }
    }

    // -------------------------------------------------------------- speed

    #[test]
    fn the_walk_starts_at_a_pace_a_person_can_read() {
        let board = Board::default();
        assert_eq!(board.walk_rate, 4.0);
    }

    /// Changing speed must not throw the head to another line: a teacher
    /// slows down to talk about the line on screen, not a different one.
    #[test]
    fn changing_speed_keeps_the_head_on_the_line_it_was_on() {
        let mut board = watching();
        board.mark_tick(0.0);
        let now = 0.6; // 2.4 steps in at 4 a second: step 2
        assert_eq!(board.head(now, 100), 2);
        board.next_speed(now);
        assert_eq!(board.walk_rate, 8.0);
        assert_eq!(board.head(now, 100), 2, "same line, faster from here");
        board.next_speed(now);
        assert_eq!(board.walk_rate, 2.0, "and round to slow");
        assert_eq!(board.head(now, 100), 2);
    }

    #[test]
    fn the_speed_button_says_how_fast_it_is_going() {
        let mut board = watching();
        let painted = painted_text(&mut board).join("\n");
        assert!(painted.contains("ปกติ"), "{painted}");
    }
}
