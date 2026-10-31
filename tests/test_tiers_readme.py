"""The README's language tiers are the registry's (T5): one table, two readers.

The table lives in `crates/diffctx-native/src/lang/mod.rs`; a language that
moves tier there moves in the README in the same change.
"""

import re
from pathlib import Path

PROJECT_ROOT = Path(__file__).parent.parent
TIER_NAMES = {"One": 1, "Two": 2, "Three": 3}


def _code_rows() -> dict[int, list[str]]:
    source = (PROJECT_ROOT / "crates" / "diffctx-native" / "src" / "lang" / "mod.rs").read_text(encoding="utf-8")
    table = re.search(r"pub const TIERS: &\[\(Tier, &str, &\[&str\]\)\] = &\[(.*?)\n\];", source, re.DOTALL)
    assert table, "lang/mod.rs has no TIERS table"
    rows: dict[int, list[str]] = {}
    for tier, label in re.findall(r'\(\s*Tier::(One|Two|Three),\s*"([^"]+)",', table.group(1)):
        rows.setdefault(TIER_NAMES[tier], []).append(label)
    return rows


def _readme_rows() -> dict[int, tuple[list[str], str]]:
    readme = (PROJECT_ROOT / "README.md").read_text(encoding="utf-8")
    section = re.search(r"^## Language tiers\n(.*?)(?=^## )", readme, re.DOTALL | re.MULTILINE)
    assert section, "README.md has no 'Language tiers' section"
    rows = re.findall(r"^\| (\d) \| ([^|]+?) \| ([^|]+?) \|$", section.group(1), re.MULTILINE)
    return {int(tier): ([name.strip() for name in names.split(",")], promise) for tier, names, promise in rows}


def test_the_readme_lists_the_languages_the_registry_tiers():
    code = _code_rows()
    readme = _readme_rows()
    assert sorted(readme) == [1, 2, 3]
    assert {tier: names for tier, (names, _) in readme.items() if tier in (1, 2)} == code


def test_tier_three_is_every_other_language_and_never_claims_an_absence():
    code = _code_rows()
    assert 3 not in code, "Tier 3 is every language without a row"
    names, promise = _readme_rows()[3]
    assert names == ["Every other language"]
    assert "not modelled" in promise.lower()
    assert "never claims" in promise
