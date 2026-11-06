# FeedbackFruits (FBF) integration spec — ntulearn-mcp v0.7.6

Reverse-engineered 6 Nov 2026, live-verified end-to-end against the CC0007
"Week 13 Final Peer Evaluation" (NTULearn course `_2706844_1`, content `_6208502_1`).
Fixtures under `tests/fixtures/fbf/` are real captured responses.

## 1. LTI launch chain (all requests carry the BbRouter cookie)

```
GET  https://ntulearn.ntu.edu.sg/webapps/blackboard/execute/blti/launchLink?course_id={cid}&content_id={content_id}
```
Redirects (follow automatically): 302 → `au-api.feedbackfruits.com/v1/lti/login?...`
→ 307 → `api.au.feedbackfruits.com/v1/lti/login?...`
→ 302 → `developer.blackboard.com/api/v1/gateway/oidcauth?...`
→ 307 → `ntulearn.ntu.edu.sg/webapps/blackboard/execute/blti/launchPlacement?cmd=authenticate&...`
→ 200 HTML containing ONE auto-submit form:
`<form action="https://au-api.feedbackfruits.com/v1/lti/launch" method="POST">`
with hidden inputs `id_token` (JWT) and `state`.

`POST` the form fields to the action URL (browser-like headers; keep cookies).
Response 200 lands on `https://au.feedbackfruits.com/courses/lti%2Fcourse/{course_uuid}/activities/{activity_uuid}?...`
and sets cookies on `.feedbackfruits.com`:
- `access_token` — JWT, `exp` ≈ now+30..60min. THIS IS THE API BEARER TOKEN.
- `refresh_token` — long-lived
- `_feedbackfruits-api_session`
- `AWSALB` / `AWSALBCORS` / `JSESSIONID` (lb cruft, ignore)

The final URL gives you the FBF **activity_id** (path segment `activities/{uuid}`).
Region is `au`; parse API base from the landing page `<meta name="api-url" content="https://api.au.feedbackfruits.com">`.

**Session persistence**: store `{api_base, access_token, refresh_token, api_session, activity_ids_seen}` at
`{config_dir}/ntulearn-mcp/fbf.json` (config_dir = same dir as the existing cookie file — see `cookie.rs::cookie_file_path`).
JWT-decode `exp` (base64url payload, no verification) to know validity. On expiry: if `refresh_token` present try
re-launch anyway — the cheapest correct refresh is to redo the whole launch chain with the (auto-refreshed) BbRouter cookie.
A launch costs 4 redirects + 1 POST.

NOTE: an LTI launch is **per content item**: the activity uuid comes from the landing URL. Cache content_id → activity_id.

## 2. Read API (JSON:API, Authorization: Bearer {access_token})

All verified 200 with a student token:

| purpose | GET |
|---|---|
| activity (title, tasks rel, progress rel) | `/v1/activities/{activity_id}` |
| activity + tasks inlined | `/v1/activities/{activity_id}?include=tasks` |
| task detail (deadline-at, reviewed-count, allocation-set, progress rel) | `/v1/tasks/{task_id}` |
| progress object (progress %, per-criterion counts/averages) | `/v1/progresses/{progress_id}` |
| MY GIVEN allocations (one per ratee) | `/v1/assignments/allocations?filter[task_id]={tid}&filter[person_id]={my_user_id}` |
| my ratings on one allocation | `/v1/assignments/allocations/{aid}/ratings` |
| criteria + option ids (rubric) | `/v1/assignments/allocations/{aid}/criteria` |
| one option (title/description/weight) | `/v1/assignments/criterion_options/{oid}` |
| user name | `/v1/users/{uid}` |
| reviews RECEIVED by me (empty until published) | `/v1/assignments/tasks/{tid}/allocations?direction=receiver` |

- `my_user_id` = JWT `sub` of the access_token.
- Task types seen: `instruct-tasks`, `user-review-tasks` (title "Give ratings" = rubric ratings;
  title "Overall comments" = written reviews), `user-review-view-tasks` (view received),
  `summative-grade-tasks` (`grades-published-at` null until release).
- Rating object: relationships `criterion` (assignments/rubric-criterion), `option`
  (assignments/criterion-option), `allocation`. The criterion's options live in
  `criteria.json` fixture → `relationships.options.data[]` (ids only; fetch each via
  `/v1/assignments/criterion_options/{id}`). Options are ordered levels, weight 0..4
  ("Never" .. "Always" for this rubric).
- Release state: allocation attrs `ratings-published-at` / `reviews-published-at`;
  summative-grade-task attr `grades-published-at`. Until then `direction=receiver`
  returns `data: []` — report "not yet released", never an error.
- 403 on `/v1/assignments/allocations` with wrong filters is normal; the
  `filter[task_id]+filter[person_id]` combo above is the working one.

## 3. Write API (LIVE-VERIFIED 6 Nov: PATCH round-trip 200)

Routes live under `/v1/assignments/…` — the bare `/v1/ratings` paths 404:
- create: `POST /v1/assignments/ratings`
- modify: `PATCH /v1/assignments/ratings/{rating_id}` (verified 200)
- clear: `DELETE /v1/assignments/ratings/{rating_id}`
- reviews: `POST /v1/assignments/reviews` / `PATCH /v1/assignments/reviews/{id}`

The API validates the FULL record on every write: a bare option-only PATCH is
422 (criterion/parent/parent-type "can't be empty"). Both create and update
must mirror the SPA's `createRecord("rating", {parent, course, allocation,
criterion, option})` — all five relationships:
`allocation` (assignments/allocation), `criterion` (assignments/rubric-criterion),
`option` (assignments/criterion-option), `parent` (activities),
`course` (lti/course; resolvable from the activity's course relationship).
Written reviews: same five relationships minus option, plus
`attributes: {body}` — NO `kind` attribute (the SPA never sets one); the review
POST path is implemented to match but not yet live-verified.
Writes go to the SAME api base with the SAME bearer token. Any write tool MUST support
`dry_run` (default true) that resolves ids + shows the payload but does not send.

## 4. New MCP tools (naming per existing `ntulearn_*` convention)

1. `ntulearn_fbf_status(course_id, content_id?, response_format)` — read-only.
   content_id optional: when omitted, walk the course content tree, find
   `resource/x-bb-blti-link` items, launch each (cached discovery) and keep FBF ones.
   Output per activity: title, per-task {title, type, deadline, my progress %, done},
   overall completion, release state (grades-published-at).
2. `ntulearn_fbf_reviews(course_id, content_id, direction=given|received, response_format)` — read-only.
   given: per teammate name → criterion title → option title (+weight). received: same from
   others, or explicit "not yet released" + release-state fields.
3. `ntulearn_fbf_rate(course_id, content_id, teammate, criterion, option, dry_run=true, response_format)` — write.
   teammate/criterion/option matched case-insensitively by substring against names/titles;
   option also accepts the weight number (0-4). Upsert semantics: PATCH existing rating if
   the (teammate, criterion) pair already has one, else POST. Returns before→after.
4. `ntulearn_fbf_comment(course_id, content_id, teammate, text, dry_run=true, response_format)` — write.
   Upsert written review on the "Overall comments" task for that teammate.

All four take `response_format` toon|json|markdown like every other tool.
Schemas in `schemas.rs` (follow the existing `Tool::new(...).with_annotations(...)` pattern;
write tools: read_only_hint=false, destructive_hint=false, idempotent_hint=true for rate).
Handlers in `handlers.rs` (`h_fbf_*` + dispatch arms), CLI specs in `cli.rs`, TOON renderers in `toon.rs`.

## 5. Rust module contract — `src/fbf.rs`

```rust
pub struct FbfSession { /* api_base, token, user_id, client, ... */ }
pub struct FbfActivity { pub id: String, pub title: String }
pub struct FbfTask { pub id: String, pub title: Option<String>, pub kind: String, pub deadline: Option<String>, pub progress_id: Option<String>, pub reviewed_count: Option<u32> }
pub struct FbfRating { pub id: String, pub criterion_id: String, pub option_id: String }
pub struct FbfCriterion { pub id: String, pub title: String, pub facet_weight: f64, pub option_ids: Vec<String> }
pub struct FbfOption { pub id: String, pub title: String, pub weight: i64, pub description: Option<String> }

impl FbfSession {
    /// Full launch chain from the existing BbRouter cookie. Loads a cached
    /// unexpired session for this content_id when available.
    pub async fn launch(state: &AppState, course_id: &str, content_id: &str) -> Result<Self, String>;
    pub fn user_id(&self) -> &str;
    pub async fn activity(&self, activity_id: &str) -> Result<FbfActivity, String>;
    pub async fn tasks(&self, activity_id: &str) -> Result<Vec<FbfTask>, String>;
    pub async fn progress(&self, progress_id: &str) -> Result<u8, String>; // progress %
    pub async fn allocations_given(&self, task_id: &str) -> Result<Vec<(String /*alloc_id*/, String /*ratee_user_id*/)>, String>;
    pub async fn ratings(&self, allocation_id: &str) -> Result<Vec<FbfRating>, String>;
    pub async fn criteria(&self, allocation_id: &str) -> Result<Vec<FbfCriterion>, String>;
    pub async fn option(&self, option_id: &str) -> Result<FbfOption, String>;
    pub async fn user_name(&self, user_id: &str) -> Result<String, String>;
    pub async fn upsert_rating(&self, allocation_id: &str, criterion_id: &str, option_id: &str, dry_run: bool) -> Result<Value, String>;
    pub async fn upsert_review(&self, allocation_id: &str, body: &str, dry_run: bool) -> Result<Value, String>;
}
```
Reuse `AppState`'s reqwest client / cookie resolution exactly like `client.rs` does
(see `Client::current_cookie_value`, `install_cookie`). Launch needs the BbRouter as a
plain header on ntulearn.ntu.edu.sg requests and a cookie jar for the FBF domains —
a separate reqwest client with `cookie_store(true)` is fine.

## 6. Test matrix (live, after build)

- `fbf_status` no content_id: discovers the item in `_2706844_1`, reports 100% completed, deadline 17 Nov 09:00
- `fbf_status` with content_id: same
- `fbf_reviews direction=given`: 5 teammates × 4 criteria, values match the UI screenshot (100%)
- `fbf_reviews direction=received`: "not yet released" (grades-published-at null)
- `fbf_rate` dry_run=true: resolves ids, shows payload, sends nothing
- `fbf_rate` dry_run=false re-setting the SAME option on one rating: PATCH round-trip, value unchanged (safe idempotent live write)
- `fbf_comment` dry_run=true
- CLI: `ntulearn-mcp fbf_status --course_id _2706844_1` etc.
- `cargo test` green; existing 22 tools untouched (schemas count 26 after)
