//! Traces fetched one tick at a time.
//!
//! The trail arrives without them - a trace is fifteen times the rest of
//! its row, and the trail is re-read every two seconds - so the cards ask
//! for the one tick they are showing. A trace never changes once the bot has
//! written it, so each is asked for once and kept. Only the most recent few
//! dozen are kept, which is far more than anyone steps back through.

use std::collections::{BTreeMap, VecDeque};
use std::sync::mpsc::{channel, Receiver, Sender};

use crate::bot::Trace;

/// How many traces are kept. Stepping back through a long session is the
/// most anyone does; sixty-four ticks is several minutes of a fast bot.
const KEEP: usize = 64;

/// How long to wait before asking again for a trace the server said it
/// could not find - a tick that has scrolled out of the file, usually.
const RETRY_AFTER: f64 = 30.0;

enum State {
    /// Asked for, not answered yet.
    Waiting(f64),
    Have(Box<Trace>),
    /// The server had none for this tick; ask again after this time.
    Missing(f64),
}

pub struct Traces {
    held: BTreeMap<String, State>,
    /// Arrival order, so the oldest is the one let go.
    order: VecDeque<String>,
    tx: Sender<(String, Option<Trace>)>,
    rx: Receiver<(String, Option<Trace>)>,
}

impl Default for Traces {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            held: BTreeMap::new(),
            order: VecDeque::new(),
            tx,
            rx,
        }
    }
}

impl Traces {
    /// Take in whatever arrived.
    pub fn poll(&mut self, now: f64) {
        while let Ok((at, trace)) = self.rx.try_recv() {
            let state = match trace {
                Some(trace) => State::Have(Box::new(trace)),
                None => State::Missing(now + RETRY_AFTER),
            };
            self.keep(at, state);
        }
    }

    /// Make sure the trace of the tick written at `at` is here or coming.
    pub fn want(&mut self, at: &str, now: f64) {
        match self.held.get(at) {
            Some(State::Have(_)) => return,
            Some(State::Waiting(sent)) if crate::api::still_waiting(Some(*sent), now) => return,
            Some(State::Missing(until)) if now < *until => return,
            _ => {}
        }
        self.keep(at.to_owned(), State::Waiting(now));
        let tx = self.tx.clone();
        let wanted = at.to_owned();
        let path = format!("/api/trace?at={}", crate::code::encode(at));
        ehttp::fetch(ehttp::Request::get(crate::api::url(&path)), move |result| {
            let _ = tx.send((wanted, read(result)));
        });
    }

    /// The trace of one tick, if it has arrived.
    pub fn get(&self, at: &str) -> Option<&Trace> {
        match self.held.get(at) {
            Some(State::Have(trace)) => Some(trace),
            _ => None,
        }
    }

    /// Is the trace of this tick on its way?
    pub fn waiting(&self, at: &str) -> bool {
        matches!(self.held.get(at), Some(State::Waiting(_)))
    }

    /// Hand one over without a network. Tests only.
    #[cfg(test)]
    pub fn hold(&mut self, at: &str, trace: Trace) {
        self.keep(at.to_owned(), State::Have(Box::new(trace)));
    }

    fn keep(&mut self, at: String, state: State) {
        if !self.held.contains_key(&at) {
            self.order.push_back(at.clone());
        }
        self.held.insert(at, state);
        while self.order.len() > KEEP {
            if let Some(oldest) = self.order.pop_front() {
                self.held.remove(&oldest);
            }
        }
    }
}

fn read(result: Result<ehttp::Response, String>) -> Option<Trace> {
    let response = result.ok()?;
    if !response.ok {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(response.text()?).ok()?;
    if value.get("present").and_then(|p| p.as_bool()) != Some(true) {
        return None;
    }
    serde_json::from_value(value.get("trace")?.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trace_that_arrived_is_kept_and_not_asked_for_again() {
        let mut traces = Traces::default();
        traces.hold("a", Trace::default());
        assert!(traces.get("a").is_some());
        // Asking again changes nothing: it is already here.
        traces.want("a", 1.0);
        assert!(!traces.waiting("a"));
    }

    #[test]
    fn only_the_newest_few_dozen_are_kept() {
        let mut traces = Traces::default();
        for index in 0..(KEEP + 10) {
            traces.hold(&format!("t{index:03}"), Trace::default());
        }
        assert!(traces.get("t000").is_none(), "the oldest went first");
        assert!(traces.get(&format!("t{:03}", KEEP + 9)).is_some());
        assert_eq!(traces.held.len(), KEEP);
    }

    #[test]
    fn a_tick_the_server_could_not_find_is_not_asked_for_every_frame() {
        let mut traces = Traces::default();
        traces.keep("gone".into(), State::Missing(100.0));
        traces.want("gone", 50.0);
        assert!(!traces.waiting("gone"), "still inside the back-off");
    }
}
