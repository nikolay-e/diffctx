//! One fitter for every answer.
//!
//! A body (an impact answer, a locate list, a pack document) sheds one step at a
//! time in an order of its own ([`Shed`]); this finds the first step at which what
//! the door emits fits the budget. The cap is on the delivered bytes, so the caller
//! says how to measure them. Impact, locate and pack each cut in a loop of their
//! own before, on three texts.

/// A body that gives up its least needed content one step at a time.
pub trait Shed: Clone {
    /// One step of shedding, least needed first; `false` once nothing more can go.
    fn shed_one(&mut self) -> bool;
}

/// Up to `steps` steps of shedding; how many took place.
fn shed<T: Shed>(body: &mut T, steps: usize) -> usize {
    (0..steps).take_while(|_| body.shed_one()).count()
}

/// How many steps below the one bisection found are looked at.
const LOOK_BELOW: usize = 16;

/// Sheds `body` until `measure` is within `cap`, or until nothing more can go.
///
/// The first step that fits is found in doubling strides, then halved: a measure
/// per step re-rendered the whole answer for each dropped caller, and 6000 callers
/// of a removed API took over a minute to fit (#510). Bisection finds the least
/// fitting step only when the measure falls with every step. A pack document does
/// not: its first cut adds a coverage block and marks the file it emptied
/// "omitted". So once a fitting step is found, the steps below it are tried, up to
/// [`LOOK_BELOW`], while they still fit: the answer never sheds one more than it
/// had to within that reach, and a fall that is steep enough costs one extra
/// measure.
pub fn shed_to_fit<T: Shed>(body: &mut T, cap: u32, measure: &dyn Fn(&T) -> u32) {
    let fits = |b: &T| measure(b) <= cap;
    let mut stride = 1;
    while !fits(body) {
        let mut ahead = body.clone();
        let taken = shed(&mut ahead, stride);
        let exhausted = taken < stride;
        if !exhausted && !fits(&ahead) {
            *body = ahead;
            stride *= 2;
            continue;
        }
        if exhausted && !fits(&ahead) {
            *body = ahead;
            break;
        }
        // `taken` steps fit and none did before this stride: the first that
        // fits lies in (lo, hi].
        let (mut lo, mut hi) = (0, taken);
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            let mut probe = body.clone();
            shed(&mut probe, mid);
            if fits(&probe) {
                (hi, ahead) = (mid, probe);
            } else {
                lo = mid;
            }
        }
        // The measure may have risen on the way: a step below may fit as well.
        for below in (1..hi).rev().take(LOOK_BELOW) {
            let mut probe = body.clone();
            shed(&mut probe, below);
            if !fits(&probe) {
                break;
            }
            ahead = probe;
        }
        *body = ahead;
        break;
    }
}
