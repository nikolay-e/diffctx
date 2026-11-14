// What every door delivers is decided once, in `diffctx::delivery`. These call it
// in-process on real git repositories: the empty-diff exit and its hint, the
// ceiling a reply may not pass, the envelope that is charged to it. The doors'
// own printing is the door-parity suite's.

use _diffctx::delivery::{Delivery, EXIT_EMPTY_DIFF, Mode, Note, Profile, Request, deliver};
use _diffctx::tokenizer::count_tokens;

mod support;
use support::repo;

const BASE: &[(&str, &str)] = &[
    (
        "shop/pricing.py",
        "def total(items):\n    return sum(items)\n",
    ),
    (
        "shop/checkout.py",
        "from shop.pricing import total\n\n\ndef charge(cart):\n    return total(cart)\n",
    ),
    (
        "shop/report.py",
        "from shop.pricing import total\n\n\ndef summarize(orders):\n    return total(orders)\n",
    ),
];

const CHANGE: &[(&str, &str)] = &[(
    "shop/pricing.py",
    "def total(items):\n    return round(sum(items), 2)\n",
)];

/// The envelope and the answer are counted as one text, and a token can join
/// across their seam.
const SLACK: u32 = 4;

fn request(root: &std::path::Path, mode: Mode, range: &str) -> Request {
    let mut request = Request::new(root.to_path_buf(), mode);
    request.range = Some(range.to_string());
    request
}

fn warned(delivery: &Delivery) -> Vec<&str> {
    delivery
        .notes
        .iter()
        .filter_map(|note| match note {
            Note::Warning(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn an_empty_pack_and_an_empty_locate_exit_four_and_name_what_to_try() {
    let tmp = repo(BASE, CHANGE);
    for mode in [Mode::Pack, Mode::Locate] {
        // The change is committed: the working tree matches HEAD.
        let delivery = deliver(&request(tmp.path(), mode, "HEAD")).unwrap();
        assert_eq!(delivery.exit, EXIT_EMPTY_DIFF, "{mode:?}");
        let said = warned(&delivery);
        assert!(
            said.iter()
                .any(|line| line.contains("the working tree matches HEAD")),
            "{mode:?}: {said:?}"
        );
    }
}

#[test]
fn an_answer_with_something_in_it_exits_zero_and_summarizes() {
    let tmp = repo(BASE, CHANGE);
    for mode in [Mode::Pack, Mode::Locate] {
        let delivery = deliver(&request(tmp.path(), mode, "HEAD~1..HEAD")).unwrap();
        assert_eq!(delivery.exit, 0, "{mode:?}");
        assert!(warned(&delivery).is_empty(), "{mode:?}");
        assert!(
            delivery.notes.iter().any(
                |note| matches!(note, Note::Figure(line) if line.contains("tokens (o200k_base)"))
            ),
            "{mode:?}: {:?}",
            delivery.notes
        );
        assert!(delivery.listed > 0, "{mode:?}");
    }
}

#[test]
fn an_empty_impact_is_an_answer_not_a_failure() {
    let tmp = repo(BASE, CHANGE);
    let mut hook = request(tmp.path(), Mode::Impact, "HEAD");
    hook.profile = Profile::Hook;
    let delivery = deliver(&hook).unwrap();
    assert_eq!(delivery.exit, 0);
    assert!(warned(&delivery).is_empty());
}

/// The tokens the least an impact answer can say takes, as the engine declares
/// them when a budget cannot hold it.
fn least_impact(tmp: &std::path::Path) -> u32 {
    let mut ask = request(tmp, Mode::Impact, "HEAD~1..HEAD");
    ask.symbol = Some("total".to_string());
    ask.budget = Some(1);
    let text = deliver(&ask).unwrap().text;
    let after = text
        .split("takes ")
        .nth(1)
        .unwrap_or_else(|| panic!("a budget of 1 says how little fits: {text}"));
    after
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("a number after `takes`: {text}"))
}

#[test]
fn a_ceiling_fits_the_reply_envelope_included_and_never_refuses_it() {
    let tmp = repo(BASE, CHANGE);
    let envelope = "Note: budget_tokens=9,999 is above max_tokens; fitted.\n\n";
    let least = least_impact(tmp.path());
    let mut ask = request(tmp.path(), Mode::Impact, "HEAD~1..HEAD");
    ask.symbol = Some("total".to_string());
    ask.envelope = envelope.to_string();
    ask.ceiling = Some(least + count_tokens(envelope) + SLACK);
    let delivery = deliver(&ask).unwrap();
    assert!(delivery.text.starts_with(envelope), "{}", delivery.text);
    assert!(
        count_tokens(&delivery.text) <= least + count_tokens(envelope) + SLACK,
        "{} tokens over {}: {}",
        count_tokens(&delivery.text),
        least + count_tokens(envelope) + SLACK,
        delivery.text
    );
}

#[test]
fn a_ceiling_below_the_least_says_the_budget_is_too_small_and_cuts_no_further() {
    let tmp = repo(BASE, CHANGE);
    let mut ask = request(tmp.path(), Mode::Impact, "HEAD~1..HEAD");
    ask.symbol = Some("total".to_string());
    ask.ceiling = Some(1);
    let delivery = deliver(&ask).unwrap();
    assert!(
        delivery.text.contains("(budget too small:"),
        "{}",
        delivery.text
    );
}

#[test]
fn a_hook_profile_renders_the_automatic_answer_within_its_cap() {
    let tmp = repo(BASE, CHANGE);
    let mut hook = request(tmp.path(), Mode::Impact, "HEAD~1..HEAD");
    hook.profile = Profile::Hook;
    let delivery = deliver(&hook).unwrap();
    assert!(
        count_tokens(&delivery.text) <= _diffctx::impact::AUTOMATIC_TOKEN_CAP,
        "{}",
        delivery.text
    );
}
