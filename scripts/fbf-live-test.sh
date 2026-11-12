#!/bin/zsh
# FBF integration live test matrix — run AFTER cargo build --release + binary swap
set -u
BIN="${BIN:-$HOME/.local/bin/ntulearn-mcp}"
CID="_2706844_1"
XID="_6208502_1"
pass=0; fail=0
t() { # t <name> <expected-substring> <cmd...>
  local name="$1"; local expect="$2"; shift 2
  local out=$("$@" 2>&1)
  if printf %s "$out" | grep -qi "$expect"; then
    pass=$((pass+1)); echo "PASS  $name"
  else
    fail=$((fail+1)); echo "FAIL  $name — expected [$expect], got:"; printf %s "$out" | head -8 | sed 's/^/      /'
  fi
}

echo "== 1. status, auto-discover =="
t "fbf_status discovers eval" "Week 13 Final Peer Evaluation" $BIN fbf_status --course_id $CID
echo "== 2. status, explicit content_id =="
t "fbf_status explicit" "Completed" $BIN fbf_status --course_id $CID --content_id $XID
t "fbf_status deadline" "17 Nov" $BIN fbf_status --course_id $CID --content_id $XID
echo "== 3. reviews given =="
t "reviews given teammate" "CHERYL" $BIN fbf_reviews --course_id $CID --content_id $XID --direction given
t "reviews given criterion" "responsibilities" $BIN fbf_reviews --course_id $CID --content_id $XID --direction given
echo "== 4. reviews received =="
t "reviews received gated" -e "not yet released" $BIN fbf_reviews --course_id $CID --content_id $XID --direction received
echo "== 5. rate dry-run =="
t "rate dry-run no-send" "dry" $BIN fbf_rate --course_id $CID --content_id $XID --teammate CHERYL --criterion "duly" --option "Always" --dry_run true
echo "== 6. comment dry-run =="
t "comment dry-run" "dry" $BIN fbf_comment --course_id $CID --content_id $XID --teammate CHERYL --text "great teammate" --dry_run true
echo "== 7. idempotent live write (same value) =="
t "rate live idempotent" "PATCH\|updated\|no change\|unchanged" $BIN fbf_rate --course_id $CID --content_id $XID --teammate CHERYL --criterion "duly" --option "Always" --dry_run false
echo "== 8. regression: existing tools untouched =="
t "list_courses works" "CC0007\|course" $BIN list_courses
t "gradebook works" "Peer Evaluation" $BIN get_gradebook --course_ids $CID

echo
echo "RESULT: $pass passed, $fail failed"
exit $((fail > 0))
