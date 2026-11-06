# Peerceptiv (PCV) Integration Spec — for ntulearn-mcp

Reverse-engineered 2026-11-06 against the live CC0006 peer evaluation
(course `_2707079_1`, content `_6241854_1`). Every write below was
live-verified with an idempotent same-value save (HTTP 200). Fixtures in
`tests/fixtures/pcv/` are real captured responses (trimmed where noted).

## 1. Launch chain (LTI 1.3, Peerceptiv variant)

Different from FeedbackFruits on TWO points: the entry executor is
`launchPlacement` (not `launchLink`), and the landing app runs on
`asia.app.peerceptiv.com` while the API lives on `sg.api.peerceptiv.com`.

1. Discover the placement URL: the content item's REST record
   (`/learn/api/public/v1/courses/{course_id}/contents`) has
   `contentHandler.id` starting with `resource/x-bb-bltiplacement-`
   (FeedbackFruits items are `resource/x-bb-blti-link` — clean filter).
   The REST record does NOT carry the placement id; fetch the classic page
   `GET /webapps/blackboard/content/listContent.jsp?course_id={course_id}&content_id={parentId}`
   (BbRouter cookie) and regex the anchor
   `href="(/webapps/blackboard/execute/blti/launchPlacement?blti_placement_id=_N_1&content_id=_X_1&course_id=_Y_1)"`.
2. `GET {ntulearn}{placement_url}` (BbRouter) -> 302 to
   `https://sg.api.peerceptiv.com/oidc/authorize?...` (login_hint tokens are
   SINGLE-USE: every launch must start fresh from step 2, never replay the
   authorize URL).
3. `GET sg.api.peerceptiv.com/oidc/authorize` -> 302
   `developer.blackboard.com/api/v1/gateway/oidcauth?...`
4. `GET developer.blackboard.com/...` -> 307 back to
   `{ntulearn}/webapps/blackboard/execute/blti/launchPlacement?cmd=authenticate&...`
5. `GET` that (BbRouter) -> 200 auto-submit form: `action=https://sg.api.peerceptiv.com/lti/launch`,
   hidden inputs `id_token` + `state`.
6. `POST` the form (Origin/Referer ntulearn) -> 302 to
   `https://asia.app.peerceptiv.com/course/{pcvCourseId}/assignment/{assignmentId}/dashboard`
   and Set-Cookie `oidc_token` + `access_token` (JWTs) on `.sg.api.peerceptiv.com`.
   PARSE pcvCourseId + assignmentId out of that Location — they are the API ids.

BbRouter isolation: the NTULearn cookie is only ever sent to
ntulearn.ntu.edu.sg (same policy as fbf.rs). Provider cookies (peerceptiv
domains) persist in the session jar.

## 2. Read API (base `https://sg.api.peerceptiv.com`, cookie auth)

The full API is Swagger-documented at `/v3/api-docs` (336 paths) but the
spec's bodies are stale for writes; trust the SPA-observed shapes below.

- `GET /me` -> caller identity: `{userId, firstName, lastName, email, role}`.
- `GET /assignment/{aid}` -> assignment: `assignmentName`,
  `peerEvaluationDeadline` / `effectivePeerEvaluationDeadline`, status flags
  (`peerEvaluationOnly`, `memberEvalEnabled`, `complete`), `user` (the
  OWNER'S user object — NOT the caller; use /me for self).
- `GET /assignment/{aid}/user/{uid}/progress` -> progress: `evaluationCount`,
  `requiredEvaluationCount`, `countEvaluationsComplete/Incomplete`,
  `status.completedAllEvaluations`, `status.missedEvaluating`,
  `group.groupName`, `groupMembers`, `incompleteEvaluations`,
  `completedEvaluations` (full eval records).
- `GET /assignment/{aid}/evals/user/{uid}` -> my evals GIVEN: array of
  PeerEvaluation `{peerEvalId, peer: User, target: "MEMBER",
  reviewRatings: [{ratingId, score, scaleScore}], reviewComments:
  [{commentId, commentNumber, comment}]}`. Empty array when none yet.
- `GET /assignment/{aid}/evals/user/{uid}/received` -> evals received
  (same shape, peer = evaluator). Availability depends on release settings
  (`hideRatingResults` etc. are true pre-release on this assignment).
- `GET /assignment/{aid}/groups` -> all groups; find the caller's by
  `groupMembers[].user.userId == myUid`. Members minus self = evaluation
  targets. NOTE: group member users have `firstName` = school ("CCDS",
  "EEE") and `lastName` = name — display as `lastName` (+ firstName if
  disambiguating).
- `GET /assignment/{aid}/eval/MEMBER/rubric` -> rubric `items`: rating items
  `{ratingId, name (e.g. "Fulfilling one's responsibilities duly (30%)"),
  ratingLevels: [{ratingLevelId, score (1..10), levelDescription}]}` and
  comment items `{commentId, commentName}`.
- `GET /eval/{peerEvalId}/map/rating` -> `{ratingId: score}` (my saved
  scores for one eval).
- `GET /eval/{peerEvalId}/comments/numbered` -> `{commentId: [texts...]}`.

Score semantics: `score` is the raw level (1..10); `scaleScore` = 100 is
max. Level descriptions for this rubric: 1 Never, 3 Rarely, ... 9 Always
(user's saves are all 9). Resolve a level description OR a raw number to
`score` in the rate tool.

## 3. Write API (LIVE-VERIFIED 6 Nov: both PUTs returned 200)

- create eval (when none exists for a peer):
  `POST /assignment/{aid}/eval/user/{myUid}/peer/{peerUid}/member` body `{}`
  -> the new PeerEvaluation (peerEvalId). (Not live-tested: creating new
  evals would add data; the CC0006 evals already existed. The tool only
  calls it when the peer has no eval yet.)
- save ratings: `PUT /eval/{peerEvalId}/ratings` body `{ratingId: score}`
  map — FULL map replace, so send every rating item.
- save comments: `PUT /eval/{peerEvalId}/comments/numbered` body
  `{commentId: [text, ...]}` (array index + 1 = commentNumber).
  Full-record style like FBF: the PUT body is the complete map.

Both PUTs return the updated PeerEvaluation. On 401/403, re-launch once
(token refresh) and retry.

## 4. Tool surface (mirror the FBF tools; all dry_run=true default)

- `ntulearn_pcv_status(course_id, content_id?)` — discovery + report.
  Without content_id: content-tree walk filtered to
  `contentHandler.id.starts_with("resource/x-bb-bltiplacement")`; for each,
  resolve the placement URL via the parent page, launch, and report. With
  content_id: straight to that item. Report: assignmentName, deadlines,
  evaluation progress (x/y complete, completedAllEvaluations), group name,
  teammates with per-teammate done/not-done.
- `ntulearn_pcv_evals(course_id, content_id, direction=given|received)` —
  ratings + comments per teammate/criterion.
- `ntulearn_pcv_rate(course_id, content_id, teammate, criterion, level, dry_run)`
  — level = level description substring ("Always") or numeric score.
- `ntulearn_pcv_comment(course_id, content_id, teammate, text, dry_run)` —
  writes the comment prompt(s); if a prompt has an existing multi-text
  list, replace index 0 and keep the rest.

Session cache `{config_dir}/ntulearn-mcp/pcv.json` mirroring fbf.json:
per (course_id, content_id) the pcv course/assignment ids + cookie jar.
Writes must never touch evals for peers the caller did not name.

## 5. Rust module contract: `src/pcv.rs`

Mirror `src/fbf.rs` structure closely:
- `pub struct PcvSession` with `api_base`, `app landing url`, `pcv_course_id`,
  `assignment_id`, `user_id`, cookie state; `PcvSession::launch(state, course_id, content_id)`.
- Launch internals: `placement_url(client, course_id, content_id) -> String`
  (REST contents lookup for parentId + handler check, then listContent.jsp
  regex); the redirect walk with the BbRouter-only-on-ntulearn policy;
  form extraction; POST; Location parsing (course/{uuid}/assignment/{uuid}).
- Reads: `me()`, `assignment(aid)`, `progress(aid, uid)`,
  `evals_given(aid, uid)`, `evals_received(aid, uid)`, `groups(aid)`,
  `my_group(aid, uid)`, `rubric(aid, target="MEMBER")`,
  `ratings_map(peer_eval_id)`, `comments_numbered(peer_eval_id)`.
- Writes: `upsert_rating(peer_eval_id, rating_id, score, dry_run)` (GET map,
  set key, PUT full map), `upsert_comment(peer_eval_id, comment_id, text,
  dry_run)` (GET map, set index 0, PUT), `create_eval(aid, my_uid, peer_uid,
  dry_run)` (POST member eval).
- Tests: fixture-driven like fbf.rs (launch form parse, Location parse,
  placement-URL regex on content_page.html, rubric level resolution,
  ratings/comments map merge, progress/evals parsing). Target ~20 tests.

## 6. Test matrix (live)

1. discovery: `pcv _2707079_1` finds `_6241854_1` (Peerceptiv) and does NOT
   list FBF items.
2. status with content_id: assignment name S1AY26_Peereval_TG53toTG56,
   deadline 2026-11-13T15:59:59Z, 5/5 evals complete,
   completedAllEvaluations true, group C27, teammates Isabelle / Hui /
   Jun Rong / Hamsini / Jonathan.
3. evals given: 5 teammates x 3 ratings (score 9) + 1 comment "very good".
4. evals received: gated or empty pre-release; must not error.
5. rate dry-run: resolves teammate+criterion+level, shows PUT payload,
   sends nothing.
6. comment dry-run: same.
7. idempotent live write: re-PUT identical ratings map + comments map for
   one teammate -> 200, data unchanged.
8. regression: existing 26 tools + `cargo test` all green.
