//! SCROLL mode's helper: asks, off the UI thread, which part of a window
//! scrolls (`framefang_platform::scroll_parts`), so the capture screen can
//! light up the page instead of the whole window. Only the newest question
//! is answered; older ones still waiting are dropped.

use std::sync::mpsc::{channel, Receiver, Sender};

pub type Rect = (i32, i32, i32, i32);

/// A question: window handle and virtual-screen point.
pub type Ask = (isize, (i32, i32));

pub struct Probe {
    asks: Sender<Ask>,
    answers: Receiver<(Ask, Option<Rect>)>,
}

impl Probe {
    pub fn start() -> Probe {
        let (asks, ask_rx) = channel::<Ask>();
        let (answer_tx, answers) = channel();
        // Ends when the probe is dropped (its sender with it).
        let _ = std::thread::Builder::new().name("scroll-probe".into()).spawn(move || {
            while let Ok(mut ask) = ask_rx.recv() {
                while let Ok(newer) = ask_rx.try_recv() { ask = newer; }
                let part = framefang_platform::scroll_parts::part_at(ask.0, ask.1);
                if answer_tx.send((ask, part)).is_err() { break; }
            }
        });
        Probe { asks, answers }
    }

    pub fn ask(&self, ask: Ask) {
        let _ = self.asks.send(ask);
    }

    /// Answers that came in since the last call.
    pub fn answers(&self) -> Vec<(Ask, Option<Rect>)> {
        self.answers.try_iter().collect()
    }
}

/// Close enough to an earlier question that its answer stands.
pub fn near(a: (i32, i32), b: (i32, i32)) -> bool {
    (a.0 - b.0).abs() <= 24 && (a.1 - b.1).abs() <= 24
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_is_answered_off_the_thread_and_a_window_without_one_says_none() {
        let probe = Probe::start();
        probe.ask((0, (10, 10)));
        let t = std::time::Instant::now();
        let mut got = Vec::new();
        while got.is_empty() && t.elapsed() < std::time::Duration::from_secs(5) {
            got = probe.answers();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(got, vec![((0, (10, 10)), None)], "no window, no part");
        assert!(near((100, 100), (120, 80)) && !near((100, 100), (130, 100)));
    }
}
