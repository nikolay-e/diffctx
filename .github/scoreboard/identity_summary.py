#!/usr/bin/env python3
"""identity_summary.py <ignored-run.log> <default-run.log>: the totals of the two identity_corpus runs.

The ignored run is the per-language measurement (`--ignored --nocapture`): one `identity/<language>` line
each. The default run holds the planted witness and the remainder check."""

import re
import sys
from pathlib import Path

measured = Path(sys.argv[1]).read_text(errors="replace")
default = Path(sys.argv[2]).read_text(errors="replace")
passed = failed = 0
for m in re.finditer(r"^test result: \w+\. (\d+) passed; (\d+) failed", measured, re.M):
    passed, failed = passed + int(m.group(1)), failed + int(m.group(2))
langs = re.findall(r"^identity/(\w+): (\d+) case\(s\) checked, (\d+) without an account, (\d+) violation\(s\)", measured, re.M)
checked = sum(int(x[1]) for x in langs)
unchecked = sum(int(x[2]) for x in langs)
violations = sum(int(x[3]) for x in langs)
dpass = re.search(r"^test result: (\w+)\. (\d+) passed; (\d+) failed", default, re.M)
print("# identity_corpus\n")
print(
    f"totals: {passed} passed / {failed} failed of the per-language tests ({len(langs)} languages reported), "
    f"{checked} cases checked, {violations} violations, {unchecked} without an account\n"
)
print("| language | cases checked | without an account | violations |\n|---|---:|---:|---:|")
for lang, c, u, v in langs:
    print(f"| {lang} | {c} | {u} | {v} |")
print()
print(
    "default run (planted witness, remainder): "
    + (f"{dpass.group(1)}, {dpass.group(2)} passed / {dpass.group(3)} failed" if dpass else "no result line")
)
for m in re.finditer(r"^test (\S+) \.\.\. (FAILED|ok)$", default, re.M):
    print(f"- {m.group(1)}: {m.group(2)}")
