#!/usr/bin/env bash
# Kaltura transcripts — post-merge live verification (course CC0006 = _2707070_1).
#
# Run AFTER merging feat/kaltura-core + feat/kaltura-wiring + feat/kaltura-qa:
#   scripts/verify-kaltura-e2e.sh             # full live run (needs a live session)
#   scripts/verify-kaltura-e2e.sh --dry-run   # no network: fixture/doc presence only
#
# Every check prints one compact PASS/FAIL line; exits nonzero on any FAIL.
# Ground truth for word counts: tests/fixtures/kaltura/qa/reference_transcript_counts.json
# (live-verified 2026-11-12; `words` = vtt_to_text + split_whitespace — see
# KALTURA_SPEC.md §3 for why the raw-VTT token count 3553 is NOT the reference).
#
# Env overrides: BIN=<path to ntulearn-mcp> (skips the cargo build).
set -u

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${BIN:-}"
DRY_RUN=0
[ "${1:-}" = "--dry-run" ] && DRY_RUN=1

COURSE="_2707070_1"          # CC0006 (live-verified reference course)
CONTENT="_5976141_1"         # Topic 6 Part 1 — holds entry 0_88lztt5a
ENTRY="0_88lztt5a"
EXPECTED_VIDEOS=25
FLAVOR_540P="0_z24etxpq"     # 960x540 flavor of 0_88lztt5a, 150718384 bytes
FLAVOR_540P_BYTES=150718384
OPENING="A very warm welcome to Part 2 of this lecture on energy and climate change."

REF_JSON="$REPO_ROOT/rust/crates/ntulearn-mcp/tests/fixtures/kaltura/qa/reference_transcript_counts.json"
export REF_JSON
FIXTURES=(
  body_html_weird.json
  manifest_crlf.m3u8
  manifest_bom.m3u8
  manifest_no_subs.m3u8
  playlist_10segs.m3u8
  vtt_malformed_timestamps.vtt
  vtt_empty.vtt
  vtt_inline_tags.vtt
  vtt_multiline_cues.vtt
  caption_assets_zero.xml
  caption_assets_error.xml
  flavor_assets_zero.xml
  flavor_assets_1p7gb.xml
  reference_transcript_counts.json
)
QA_DIR="rust/crates/ntulearn-mcp/tests/fixtures/kaltura/qa"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf 'PASS  %s\n' "$1"; }
bad() { fail=$((fail+1)); printf 'FAIL  %s — %s\n' "$1" "$2"; }
info(){ printf -- '-- %s\n' "$1"; }


# ---------------------------------------------------------------------------
# dry-run: no network, no binary — fixture + doc presence only
# ---------------------------------------------------------------------------
if [ "$DRY_RUN" = 1 ]; then
  info "dry-run: file/fixture presence only (no network, no binary needed)"
  [ -s "$REPO_ROOT/KALTURA_SPEC.md" ] \
    && ok "KALTURA_SPEC.md present" || bad "KALTURA_SPEC.md present" "missing/empty at repo root"
  for rel in "${FIXTURES[@]}"; do
    f="$REPO_ROOT/$QA_DIR/$rel"
    if [ -s "$f" ]; then ok "fixture ${rel}"; else bad "fixture ${rel}" "missing/empty: $QA_DIR/$rel"; fi
  done
  if command -v python3 >/dev/null 2>&1; then
    if REF_JSON="$REF_JSON" python3 - <<'PY'
import json, os, sys
try:
    data = json.load(open(os.environ["REF_JSON"]))
except Exception as e:
    print(f"unparseable reference json: {e}"); sys.exit(1)
if not isinstance(data, list) or len(data) != 25:
    print("expected 25 entries"); sys.exit(1)
seen = set()
for e in data:
    if not (isinstance(e.get("entry"), str) and e["entry"] not in seen
            and isinstance(e.get("words"), int) and e["words"] > 0):
        print(f"bad entry: {e}"); sys.exit(1)
    seen.add(e["entry"])
PY
    then ok "reference counts: 25 valid entries"; else bad "reference counts: 25 valid entries" "see python errors above"; fi
  else
    bad "reference counts: 25 valid entries" "python3 not found"
  fi
  printf '\nRESULT: %d passed, %d failed\n' "$pass" "$fail"
  exit $((fail > 0))
fi

# ---------------------------------------------------------------------------
# build + binary
# ---------------------------------------------------------------------------
if [ -z "$BIN" ]; then
  BIN="$REPO_ROOT/rust/target/release/ntulearn-mcp"
  info "building release binary (cargo build --release)..."
  if ! (cd "$REPO_ROOT/rust" && cargo build --release); then
    bad "cargo build --release" "build failed"
    printf '\nRESULT: %d passed, %d failed\n' "$pass" "$fail"
    exit 1
  fi
else
  info "using BIN=$BIN (skipping build)"
fi
if [ -x "$BIN" ]; then ok "binary present ($BIN)"; else bad "binary present" "$BIN not executable"; exit 1; fi

# command tokens: accept kebab (kaltura-list) or snake (kaltura_list) spellings
HELP_ALL="$("$BIN" help 2>/dev/null || true)"
cmd_token() { # cmd_token <primary> <alt>
  if printf '%s\n' "$HELP_ALL" | grep -qw -- "$1"; then printf '%s' "$1"
  elif printf '%s\n' "$HELP_ALL" | grep -qw -- "$2"; then printf '%s' "$2"
  else printf '%s' "$1"; fi
}
KLIST="$(cmd_token kaltura-list kaltura_list)"
KTR="$(cmd_token kaltura-transcripts kaltura_transcripts)"
KDL="$(cmd_token kaltura-download kaltura_download)"
for c in "$KLIST" "$KTR" "$KDL"; do
  if printf '%s\n' "$HELP_ALL" | grep -qw -- "$c"; then ok "command registered: $c"
  else bad "command registered: $c" "not in \`$BIN help\`"; fi
done

# flag spellings differ by wiring; discover from per-command help, fall back to repo kebab convention
probe_flag() { # probe_flag <cmd> <candidate...> -> prints first flag present in `help <cmd>`
  local cmd="$1"; shift
  local h; h="$("$BIN" help "$cmd" 2>/dev/null || true)"
  [ -n "$h" ] || return 1
  local cand
  for cand in "$@"; do
    if printf '%s\n' "$h" | grep -qE "(^|[^-])--${cand}([ =:]|$)"; then
      printf '%s' "$cand"; return 0
    fi
  done
  return 1
}
COURSE_FLAG="$(probe_flag "$KLIST" course-id course || printf course-id)"
CONTENT_FLAG="$(probe_flag "$KTR" content-id content || printf content-id)"
OUT_FLAG="$(probe_flag "$KTR" save-dir out destination-dir output-dir || true)"
QUALITY_FLAG="$(probe_flag "$KDL" quality || printf quality)"
info "flags: course=--$COURSE_FLAG content=--$CONTENT_FLAG out=${OUT_FLAG:+--$OUT_FLAG} quality=--$QUALITY_FLAG"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/kaltura-e2e.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

run_tool() { # run_tool <json-out-file> <cmd-token> <args...>
  local out="$1"; shift
  "$BIN" "$@" --json >"$out" 2>"$out.err"
}


# shared python helpers for the JSON checks below (written to $TMP at runtime)
cat >"$TMP/helpers.py" <<'PY'
import json, os

def walk(v):
    """Yield every dict nested in v (depth-first)."""
    if isinstance(v, dict):
        yield v
        for x in v.values():
            yield from walk(x)
    elif isinstance(v, list):
        for x in v:
            yield from walk(x)

def load_ref():
    return {e["entry"]: e["words"] for e in json.load(open(os.environ["REF_JSON"]))}

def embeds(payload):
    """(content_id, entry_id) pairs from any kaltura-list payload shape."""
    out = set()
    for d in walk(payload):
        e = d.get("entry_id") or d.get("entryId")
        c = d.get("content_id") or d.get("contentId")
        if e and c:
            out.add((c, e))
    return out

def transcript_records(payload):
    """Per-video summary dicts: have a word count plus an entry reference."""
    recs = []
    for d in walk(payload):
        e = d.get("entry") or d.get("entry_id") or d.get("entryId")
        if e and "words" in d:
            recs.append((e, d))
    return recs

def strings(d):
    """Every string value nested in d."""
    for x in walk(d):
        for v in x.values():
            if isinstance(v, str):
                yield v

def txt_path(rec):
    p = [s for s in strings(rec) if s.endswith(".txt")]
    return p[0] if p else None
PY
export HELPERS="$TMP/helpers.py"

# ---------------------------------------------------------------------------
# 1. kaltura-list — exactly 25 videos, 0_88lztt5a under _5976141_1
# ---------------------------------------------------------------------------
info "kaltura-list on $COURSE (walks the content tree)..."
run_tool "$TMP/list.json" "$KLIST" "--$COURSE_FLAG" "$COURSE"
if [ -s "$TMP/list.json" ]; then ok "kaltura-list runs"
else bad "kaltura-list runs" "$(head -c 300 "$TMP/list.err")"; fi

if [ -s "$TMP/list.json" ]; then
  detail="$(LIST_JSON="$TMP/list.json" python3 - <<'PY'
import json, os
exec(open(os.environ["HELPERS"]).read())
payload = json.load(open(os.environ["LIST_JSON"]))
found = embeds(payload)
if len(found) != 25:
    print(f"expected 25 embeds, found {len(found)}")
elif ("_5976141_1", "0_88lztt5a") not in found:
    print("entry 0_88lztt5a not found under content _5976141_1")
else:
    print("OK")
PY
)"
  if [ "$detail" = "OK" ]; then ok "kaltura-list: exactly 25 videos incl. 0_88lztt5a"
  else bad "kaltura-list: exactly 25 videos incl. 0_88lztt5a" "$detail"; fi
fi


# ---------------------------------------------------------------------------
# 2. kaltura-transcripts — one content item (Topic 6 Part 1)
# ---------------------------------------------------------------------------
info "kaltura-transcripts for content $CONTENT..."
TR_ARGS=("$KTR" "--$COURSE_FLAG" "$COURSE" "--$CONTENT_FLAG" "$CONTENT")
[ -n "$OUT_FLAG" ] && TR_ARGS+=("--$OUT_FLAG" "$TMP")
run_tool "$TMP/tr_one.json" "${TR_ARGS[@]}"
if [ -s "$TMP/tr_one.json" ]; then ok "kaltura-transcripts (one video) runs"
else bad "kaltura-transcripts (one video) runs" "$(head -c 300 "$TMP/tr_one.err")"; fi

if [ -s "$TMP/tr_one.json" ]; then
  py_one() { # py_one <check-name> <check-id>
    local name="$1" which="$2"
    local detail
    detail="$(TR_JSON="$TMP/tr_one.json" python3 - "$which" <<'PY'
import json, os, sys
exec(open(os.environ["HELPERS"]).read())
which = sys.argv[1]
payload = json.load(open(os.environ["TR_JSON"]))
ref = load_ref()
mine = [(e, d) for e, d in transcript_records(payload) if e == "0_88lztt5a"]
if not mine:
    print("no transcript record for entry 0_88lztt5a"); sys.exit(1)
r = mine[0][1]
if which == "words":
    want, got = ref["0_88lztt5a"], r["words"]
    if abs(got - want) > 0.05 * want:
        print(f"words {got} outside ±5% of reference {want}"); sys.exit(1)
elif which == "label":
    if r.get("label") != "Edited":
        print(f"label {r.get('label')!r} != 'Edited'"); sys.exit(1)
elif which == "segments":
    if r.get("segments") != 4:
        print(f"segments {r.get('segments')} != 4"); sys.exit(1)
elif which == "files":
    vtt = [s for s in strings(r) if s.endswith(".vtt")]
    txt = [s for s in strings(r) if s.endswith(".txt")]
    if not vtt or not txt:
        print("missing .vtt/.txt path in record"); sys.exit(1)
    if not all(os.path.getsize(p) > 0 for p in vtt + txt):
        print("empty transcript file on disk"); sys.exit(1)
elif which == "txtwords":
    p = txt_path(r)
    got = len(open(p, encoding="utf-8").read().split())
    if got != r["words"]:
        print(f"txt has {got} words, report says {r['words']}"); sys.exit(1)
elif which == "opening":
    body = " ".join(open(txt_path(r), encoding="utf-8").read().split())
    opening = "A very warm welcome to Part 2 of this lecture on energy and climate change."
    if opening not in body:
        print("opening sentence not found in txt"); sys.exit(1)
print("OK")
PY
)"
    if [ "$detail" = "OK" ]; then ok "$name"; else bad "$name" "$detail"; fi
  }
  py_one "transcripts: words within ±5% of reference (2224)" words
  py_one "transcripts: label is 'Edited'" label
  py_one "transcripts: 4 segments" segments
  py_one "transcripts: .vtt + .txt written and non-empty" files
  py_one "transcripts: txt word count matches report" txtwords
  py_one "transcripts: opening sentence present" opening
fi


# ---------------------------------------------------------------------------
# 3. kaltura-transcripts — whole course vs reference counts (±5%)
# ---------------------------------------------------------------------------
info "kaltura-transcripts for ALL videos in $COURSE (25 videos + politeness sleeps; a few minutes)..."
TR_ALL_ARGS=("$KTR" "--$COURSE_FLAG" "$COURSE")
[ -n "$OUT_FLAG" ] && TR_ALL_ARGS+=("--$OUT_FLAG" "$TMP")
run_tool "$TMP/tr_all.json" "${TR_ALL_ARGS[@]}"
if [ -s "$TMP/tr_all.json" ]; then ok "kaltura-transcripts (all videos) runs"
else bad "kaltura-transcripts (all videos) runs" "$(head -c 300 "$TMP/tr_all.err")"; fi

if [ -s "$TMP/tr_all.json" ]; then
  detail="$(TR_JSON="$TMP/tr_all.json" python3 - <<'PY'
import json, os, sys
exec(open(os.environ["HELPERS"]).read())
payload = json.load(open(os.environ["TR_JSON"]))
ref = load_ref()
recs = dict(transcript_records(payload))
errs = []
for e, want in ref.items():
    r = recs.get(e)
    if r is None:
        errs.append(f"{e}: no record"); continue
    if abs(r["words"] - want) > 0.05 * want:
        errs.append(f"{e}: {r['words']} vs {want}"); continue
    p = txt_path(r)
    if p:
        got = len(open(p, encoding="utf-8").read().split())
        if got != r["words"]:
            errs.append(f"{e}: txt {got} != reported {r['words']}")
if errs:
    print("; ".join(errs[:5])); sys.exit(1)
print(f"OK {len(ref)}/{len(ref)}")
PY
)"
  if [ "$(printf '%s' "$detail" | cut -d' ' -f1)" = "OK" ]; then
    ok "transcripts(all): all 25 word counts within ±5%, txt files match"
  else
    bad "transcripts(all): all 25 word counts within ±5%, txt files match" "$detail"
  fi
fi

# ---------------------------------------------------------------------------
# 4. kaltura-download — smallest flavor for Topic 6 Part 1, then delete
# ---------------------------------------------------------------------------
info "kaltura-download (smallest) for content $CONTENT (~150 MB stream; be patient)..."
run_tool "$TMP/dl.json" "$KDL" "--$COURSE_FLAG" "$COURSE" "--$CONTENT_FLAG" "$CONTENT" "--$QUALITY_FLAG" smallest
if [ -s "$TMP/dl.json" ]; then ok "kaltura-download runs"
else bad "kaltura-download runs" "$(head -c 300 "$TMP/dl.err")"; fi

DL_FILE=""
if [ -s "$TMP/dl.json" ]; then
  detail="$(DL_JSON="$TMP/dl.json" python3 - <<'PY'
import json, os, sys
exec(open(os.environ["HELPERS"]).read())
payload = json.load(open(os.environ["DL_JSON"]))
cands = [d for d in walk(payload) if "bytes" in d]
if not cands:
    print("no bytes field in output"); sys.exit(1)
r = cands[0]
if r["bytes"] < 10_000_000:
    print(f"bytes {r['bytes']} suspiciously small"); sys.exit(1)
paths = [s for s in strings(r) if s.endswith(".mp4")]
if not paths:
    print("no .mp4 path in output"); sys.exit(1)
path = paths[0]
if not os.path.isfile(path):
    print(f"file missing: {path}"); sys.exit(1)
if os.path.getsize(path) != r["bytes"]:
    print(f"file size {os.path.getsize(path)} != reported {r['bytes']}"); sys.exit(1)
# smallest must not be the original (the 0_88lztt5a original reports sizeInBytes=0)
orig = next((v for k, v in r.items() if k.lower() in ("is_original", "isoriginal")), None)
if orig is None:
    for d in walk(r):
        if "isOriginal" in d or "is_original" in d:
            orig = d.get("isOriginal", d.get("is_original")); break
if orig:
    print("smallest picked the ORIGINAL flavor"); sys.exit(1)
fid = next((v for k, v in r.items() if k.lower() in ("flavor_id", "flavorid")), None)
if fid == "0_z24etxpq" and abs(r["bytes"] - 150718384) > 0.10 * 150718384:
    print(f"540p flavor bytes {r['bytes']} outside ±10% of 150718384"); sys.exit(1)
print("OK " + path)
PY
)"
  if [ "$(printf '%s' "$detail" | cut -d' ' -f1)" = "OK" ] && [ -f "${detail#OK }" ]; then
    ok "download: smallest non-original flavor, file size == reported bytes"
    DL_FILE="${detail#OK }"
    rm -f "$DL_FILE"
    if [ ! -e "$DL_FILE" ]; then ok "download: file deleted after check"
    else bad "download: file deleted after check" "still present: $DL_FILE"; fi
  else
    bad "download: smallest non-original flavor, file size == reported bytes" "$detail"
  fi
fi

printf '\nRESULT: %d passed, %d failed\n' "$pass" "$fail"
exit $((fail > 0))
