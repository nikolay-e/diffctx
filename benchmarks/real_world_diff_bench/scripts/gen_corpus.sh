#!/bin/bash
# Generate diffctx-benchmark corpus. MD first; YAML only if MD succeeded (avoid double hang penalty).
set -u
D=/Users/nikolay/.local/bin/diffctx
REPOS=/Users/nikolay/diffctx/test-repos
BASE=/tmp/bench_corpus
CAP=30
mkdir -p "$BASE"
tok() { grep -oE '^[0-9,]+ tokens' "$1" 2>/dev/null | head -1 | grep -oE '[0-9,]+' | tr -d ','; }

while IFS=$'\t' read -r repo idx sha _ desc; do
  d="$BASE/$repo/$(printf '%03d' "$idx")-${sha:0:7}"
  mkdir -p "$d"
  cd "$REPOS/$repo" 2>/dev/null || {
    echo "MISSING $repo"
    continue
  }
  rng="${sha}~1..${sha}"
  git diff "$rng" --stat >"$d/diff.stat" 2>/dev/null
  git diff "$rng" --numstat >"$d/diff.numstat" 2>/dev/null
  git diff "$rng" >"$d/truth.diff" 2>/dev/null
  gf=$(wc -l <"$d/diff.numstat" | tr -d ' ')
  perl -e "alarm $CAP; exec @ARGV" "$D" . --diff "$rng" -f md >"$d/ctx.md" 2>"$d/md.err"
  rcmd=$?
  mdtok=$(tok "$d/md.err")
  ymltok=0
  rcyaml=NA
  changed=0
  frags=0
  ctxfiles=0
  if [ "$rcmd" = "0" ]; then
    perl -e "alarm $CAP; exec @ARGV" "$D" . --diff "$rng" -f yaml >"$d/ctx.yaml" 2>"$d/yaml.err"
    rcyaml=$?
    ymltok=$(tok "$d/yaml.err")
    changed=$(grep -c 'role: "changed"' "$d/ctx.yaml" 2>/dev/null)
    frags=$(grep -cE '^  - path:' "$d/ctx.yaml" 2>/dev/null)
    ctxfiles=$(grep -oE 'path: "[^"]+"' "$d/ctx.yaml" 2>/dev/null | sort -u | wc -l | tr -d ' ')
  fi
  status=ok
  { [ "$rcmd" = "142" ] || [ "$rcmd" = "137" ]; } && status=hang
  [ "$rcmd" = "4" ] && status=empty
  { [ "$status" = ok ] && [ "${mdtok:-0}" -gt 20000 ]; } && status=over_dump
  printf 'repo\t%s\nidx\t%s\nsha\t%s\ndesc\t%s\ngit_files\t%s\nrc_md\t%s\nrc_yaml\t%s\nmd_tokens\t%s\nyaml_tokens\t%s\nchanged_frags\t%s\ntotal_frags\t%s\nctx_files\t%s\nstatus\t%s\n' \
    "$repo" "$idx" "$sha" "$desc" "$gf" "$rcmd" "$rcyaml" "${mdtok:-0}" "${ymltok:-0}" "${changed:-0}" "${frags:-0}" "$ctxfiles" "$status" >"$d/meta.tsv"
  echo "$repo/$idx ${sha:0:7} status=$status gitfiles=$gf md_tok=${mdtok:-0} yaml_tok=${ymltok:-0} changed=$changed ctxfiles=$ctxfiles"
done <"${1:-$BASE/manifest.tsv}"
echo "GEN_DONE ${1:-all}"
