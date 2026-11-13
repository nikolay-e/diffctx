//! One fitter for every answer.
//!
//! A body (an impact answer, a locate list, a pack document) sheds one step at a
//! time in an order of its own ([`Shed`]); this finds the first step at which what
//! the door emits fits the budget. The cap is on the delivered bytes, so the caller
//! says how to measure them.

#![deny(clippy::disallowed_methods)]

/// A body that gives up its least needed content one step at a time.
pub trait Shed: Clone {
    /// One step of shedding, least needed first; `false` once nothing more can go.
    fn shed_one(&mut self) -> bool;

    /// Refreshes what a measure reads, once per batch of steps; whether the
    /// run's limits are over.
    fn settle(&mut self) -> bool {
        false
    }
}

/// Up to `steps` steps, settled: how many took place, and whether limits are over.
fn shed<T: Shed>(body: &mut T, steps: usize) -> (usize, bool) {
    let taken = (0..steps).take_while(|_| body.shed_one()).count();
    (taken, body.settle())
}

/// Sheds `body` until `measure` is within `cap`, or until nothing more can go.
///
/// The first step that fits is found in doubling strides, then halved, each probe
/// continuing from the last step known not to fit: a measure per step re-rendered
/// the whole answer for each dropped caller, and 6000 callers took over a minute
/// to fit (#510). The step found has none that fits just below it, and is the
/// least fitting one only when the measure falls with every step; a pack document
/// does not (its first cut adds a coverage block). Limits that are over end the
/// measuring: nothing fits then, so the doubling goes on to the least, and a step
/// already found stays.
pub fn shed_to_fit<T: Shed>(body: &mut T, cap: u32, measure: &dyn Fn(&T) -> u32) {
    let fits = |b: &T| measure(b) <= cap;
    if fits(body) {
        return;
    }
    for stride in (0..).map(|k| 1usize << k) {
        let mut ahead = body.clone();
        let (taken, over) = shed(&mut ahead, stride);
        if over || !fits(&ahead) {
            *body = ahead;
            if taken < stride {
                break;
            }
            continue;
        }
        // `taken` steps fit and none did before this stride: the first that
        // fits lies in (lo, hi], and `body` is the state at `lo`.
        let (mut lo, mut hi) = (0, taken);
        while hi - lo > 1 {
            let half = (hi - lo) / 2;
            let mut probe = body.clone();
            let (_, over) = shed(&mut probe, half);
            if !over && fits(&probe) {
                (hi, ahead) = (lo + half, probe);
            } else {
                (lo, *body) = (lo + half, probe);
            }
        }
        *body = ahead;
        break;
    }
    body.settle();
}
