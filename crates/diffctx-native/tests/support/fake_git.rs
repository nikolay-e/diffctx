// A `git` earlier on PATH that fails one numbered call of a run, or misbehaves
// on one subcommand, and execs the real git for every other one.

// With FAKE_GIT_SLOTS set, every call also claims the next number under that
// directory, in the order the calls begin, and logs its arguments there; the
// call numbered FAKE_GIT_FAIL_INDEX fails like a refused read. The numbers
// are the same for the same run, so a sweep can fail each call in turn.
//
// A stalling mode records it in the file FAKE_GIT_STALLED names, when its output
// stops: the event a run's stall knob (DIFFCTX_TEST_GIT_STALL_FILE) waits for.
pub const FAKE_GIT: &str = r#"#!/bin/sh
echo "$*" >> "$FAKE_GIT_CALLS"
if [ -n "$FAKE_GIT_SLOTS" ]; then
  until mkdir "$FAKE_GIT_SLOTS/lock" 2>/dev/null; do :; done
  read -r n < "$FAKE_GIT_SLOTS/next"
  echo $((n + 1)) > "$FAKE_GIT_SLOTS/next"
  echo "$*" >> "$FAKE_GIT_SLOTS/calls"
  rmdir "$FAKE_GIT_SLOTS/lock"
  if [ "$n" = "$FAKE_GIT_FAIL_INDEX" ]; then
    echo $$ >> "$FAKE_GIT_PIDS"
    echo 'fatal: simulated failure of the call' >&2
    exit 128
  fi
fi
for a in "$@"; do
  for t in $FAKE_GIT_TARGET; do
    if [ "$a" = "$t" ]; then hit=1; fi
  done
done
if [ -z "$hit" ]; then exec "$REAL_GIT" "$@"; fi
echo $$ >> "$FAKE_GIT_PIDS"
stall() { [ -z "$FAKE_GIT_STALLED" ] || echo stalled >> "$FAKE_GIT_STALLED"; while :; do sleep 60; done; }
case "$FAKE_GIT_MODE" in
  stall_before_read) stall ;;
  partial_header) read -r _; printf 'abc'; stall ;;
  stall_in_body) read -r _; printf '%s blob 100\n0123456789' 0123456789012345678901234567890123456789; stall ;;
  stall_in_oversized) read -r _; printf '%s blob 99000000\n0123456789' 0123456789012345678901234567890123456789; stall ;;
  malformed) read -r _; echo 'this is not a header'; stall ;;
  early_eof) exit 0 ;;
  fail) echo 'fatal: simulated failure' >&2; exit 128 ;;
  fail_after_first) [ $(wc -l < "$FAKE_GIT_PIDS") -gt 1 ] || exec "$REAL_GIT" "$@"; echo 'fatal: detected dubious ownership in repository' >&2; exit 128 ;;
  descendant_holds_stdout) stall & echo $! >> "$FAKE_GIT_PIDS"; exec "$REAL_GIT" "$@" ;;
  stderr_flood) head -c 4000000 /dev/zero | tr '\0' 'e' >&2; exec "$REAL_GIT" "$@" ;;
esac
exec "$REAL_GIT" "$@"
"#;

// A `git` that runs the real one and says every refusal in another language:
// the exit status and stdout are git's own, the stderr words are not English.
// What the engine decides from git's words, this shim turns the wrong way.
pub const TRANSLATED_GIT: &str = r#"#!/bin/sh
err=$(mktemp)
"$REAL_GIT" "$@" 2>"$err"
status=$?
if [ -s "$err" ]; then echo 'Schwerwiegend: übersetzte Meldung' >&2; fi
rm -f "$err"
exit $status
"#;
