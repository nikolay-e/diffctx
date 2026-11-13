//! One fitter for every answer.
//!
//! A body (an impact answer, a locate list, a pack document) sheds one step at a
//! time in an order of its own ([`Shed`]); this finds the first step at which what
//! the door emits fits the budget. The cap is on the delivered bytes, so the caller
//! says how to measure them. Impact, locate and pack each cut in a loop of their
//! own before, on three texts.

#![deny(clippy::disallowed_methods)]

/// A body that gives up its least needed content one step at a time.
pub trait Shed: Clone {
    /// One step of shedding, least needed first; `false` once nothing more can go.
    fn shed_one(&mut self) -> bool;

    /// Refreshes what a measure reads and a step leaves stale, once per batch of
    /// steps.
    fn settle(&mut self) {}
}

/// Up to `steps` steps of shedding, settled; how many took place.
fn shed<T: Shed>(body: &mut T, steps: usize) -> usize {
    let taken = (0..steps).take_while(|_| body.shed_one()).count();
    body.settle();
    taken
}

/// Sheds `body` until `measure` is within `cap`, or until nothing more can go.
///
/// The first step that fits is found in doubling strides, then halved, each probe
/// continuing from the last step known not to fit: a measure per step re-rendered
/// the whole answer for each dropped caller, and 6000 callers of a removed API took
/// over a minute to fit (#510). The step found has none that fits just below it,
/// but the least fitting one only when the measure falls with every step; a pack
/// document does not: its first cut adds a coverage block and marks the file it
/// emptied "omitted".
pub fn shed_to_fit<T: Shed>(body: &mut T, cap: u32, measure: &dyn Fn(&T) -> u32) {
    let fits = |b: &T| measure(b) <= cap;
    if fits(body) {
        return;
    }
    let mut stride = 1;
    loop {
        let mut ahead = body.clone();
        let taken = shed(&mut ahead, stride);
        if !fits(&ahead) {
            *body = ahead;
            if taken < stride {
                break;
            }
            stride *= 2;
            continue;
        }
        // `taken` steps fit and none did before this stride: the first that
        // fits lies in (lo, hi], and `body` is the state at `lo`.
        let (mut lo, mut hi) = (0, taken);
        while hi - lo > 1 {
            let half = (hi - lo) / 2;
            let mut probe = body.clone();
            shed(&mut probe, half);
            if fits(&probe) {
                (hi, ahead) = (lo + half, probe);
            } else {
                (lo, *body) = (lo + half, probe);
            }
        }
        *body = ahead;
        break;
    }
}
