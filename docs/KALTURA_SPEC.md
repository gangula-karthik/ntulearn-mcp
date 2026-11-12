# Kaltura Transcripts Integration Spec — for ntulearn-mcp

Reverse-engineered 2026-11-12 against the live CC0006 course
(course `_2707070_1`, content `_5976141_1` = "Topic 6: Energy and Climate
Change (II) Policy Instruments and Economics of Climate Change Part 1",
Kaltura entry `0_88lztt5a`). Every route below was live-verified. Fixtures
live in `rust/crates/ntulearn-mcp/tests/fixtures/kaltura/`: the core pack is
real captured responses; `qa/` is the adversarial pack (fabricated — each
file's purpose is documented in `qa/README.md`). The live word-count ground
truth for all 25 CC0006 videos is `qa/reference_transcript_counts.json`.

NTULearn's Kaltura is a KAF (Kaltura Application Framework) deployment:

| Piece | Host |
|---|---|
| LTI tool (KAF) | `ntulearnv1.ntu.edu.sg` |
| Playback metadata (XML API, playManifest) | `api.sgp2.ovp.kaltura.com` / `api.sg.kaltura.com` |
| Caption sub-playlists | `vodcdn.sg.kaltura.com` |
| Media CDN (MP4, no auth) | `cfvod.sgp2.ovp.kaltura.com` |

Partner id is 117 (`flashvars.wid == "_117"`).

## 1. Discovery (no LTI launch needed)

`GET /learn/api/public/v1/courses/{course_id}/contents/{content_id}` with
`fields=id,title,contentHandler,body`. The body embeds anchors like
(`content_body_topic6_part1.json`):

```html
<a href="undefined?Kq3cZcYS15=..." data-bbtype="embedded-app"
   data-bbfile="{&quot;linkType&quot;:&quot;resource/x-bb-blti-link&quot;,
     &quot;linkRefId&quot;:&quot;_1326_1&quot;,
     &quot;url&quot;:&quot;https://ntulearnv1.ntu.edu.sg/browseandembed/index/media/entryid/0_88lztt5a/...&quot;,
     &quot;title&quot;:&quot;Topic 6: ... Part 1&quot;}">undefined</a>
```

- The `data-bbfile` value is HTML-escaped JSON — entity-decode (`&quot;`,
  `&amp;`, ...) BEFORE `serde_json` parsing.
- Kaltura filter: `linkType == "resource/x-bb-blti-link"` AND the url
  contains `/browseandembed/` and `entryid/`. FeedbackFruits items share the
  linkType but never match the url; non-LTI bbfiles are skipped, not errors.
- entry id: first `entryid/(0_[a-z0-9]+)` match in the url (case-insensitive).
- One body can carry several embeds — return all, duplicates included.
- `placement_id` is the `linkRefId` (`_1326_1`) — the launch handle for §2.

Discovery is pure REST + parsing: the LTI chain in §2 is only needed to
mint a player KS for transcripts/downloads.

## 2. LTI 1.3 launch chain (Kaltura variant — differs from PCV)

Same skeleton as the Peerceptiv chain but the executor lands on KAF, and
step 1's URL MUST carry `course_id` AND `content_id` (omitting either 500s
with `Course.getId() because this._course is null`).

1. `GET https://ntulearn.ntu.edu.sg/webapps/blackboard/execute/blti/launchPlacement?blti_placement_id={placement_id}&wrapped=true&course_id={course_id}&content_id={content_id}`
   (BbRouter cookie, NTULearn origin only)
   -> 302 `https://ntulearnv1.ntu.edu.sg/hosted/index/oidc-init?...`
2. `GET` oidc-init (no BbRouter; Referer ntulearn.ntu.edu.sg)
   -> 302 `https://developer.blackboard.com/api/v1/gateway/oidcauth?...`
3. `GET` gateway (Referer ntulearnv1.ntu.edu.sg)
   -> 307 `https://ntulearn.ntu.edu.sg/webapps/blackboard/execute/blti/launchPlacement?cmd=authenticate&...`
4. `GET` authenticate (BbRouter; Referer developer.blackboard.com) -> 200
   auto-submit form (`authenticate_form_compact.html`):
   `action=https://ntulearnv1.ntu.edu.sg/hosted/index/oauth2-launch`,
   hidden inputs `id_token` (JWT) + `state`.
5. `POST` oauth2-launch (form-encoded, Origin/Referer ntulearnv1) -> 200;
   Kaltura session cookies land on `ntulearnv1.ntu.edu.sg` (keep the jar).
6. `GET` the `data-bbfile` url (or its media-redirect variant) with those
   cookies -> 200 entry page carrying `var flashvars = {...};`
   (`entry_page_flashvars.html`). `flashvars.ks` is the player KS (v2,
   base64-ish — URL-encode it when it rides in a query string).
   `flashvars.wid == "_117"` -> partner id 117.

`login_hint` tokens are SINGLE-USE: never replay a step-2 URL; always
restart from step 1 (same policy as `pcv.rs`).

Session caching: the minted KS is valid for ANY entry of the same course
(live-verified across the 25 CC0006 videos) — cache per `course_id`, and
re-launch from step 1 when the API answers `INVALID_KS`/401 mid-run.
BbRouter isolation: the NTULearn cookie only ever goes to
`ntulearn.ntu.edu.sg`; KAF/Kaltura cookies persist in the session jar.

## 3. Transcript fetch (playManifest route — the verified one)

`caption_captionasset/action/serve` 200s an EMPTY body with a player KS —
dead end. The verified route is HLS:

1. `GET https://api.sgp2.ovp.kaltura.com/p/{partner}/sp/{partner}00/playManifest/entryId/{entry}/format/applehttp/protocol/https/a.m3u8?referrer=aHR0cHM6Ly9udHVsZWFybnYxLm50dS5lZHUuc2c%3D&ks={urlencoded ks}`
   -> master m3u8 (`manifest_with_captions.m3u8`). No `flavorIds` needed —
   the URL is generic for any entry (verified). It carries
   `#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID="subs",NAME="Edited",...,LANGUAGE="en",URI="https://vodcdn.sg.kaltura.com/api_v3/index.php/service/caption_captionasset/action/serveWebVTT/captionAssetId/{id}/segmentDuration/300/ks/{ks}/version/{v}/a.m3u8"`
   plus `#EXT-X-STREAM-INF` flavor variants. The caption label is the
   `NAME` attribute ("Edited", "English", ...).
   A video with NO captions simply lacks the SUBTITLES line ->
   `TranscriptError::NoCaptions` (per-video note, never a whole-call error).
2. `GET` the SUBTITLES URI -> caption sub-playlist listing
   `segmentIndex/N.vtt` with `#EXTINF` durations
   (`caption_playlist.m3u8`; 4 segments for a 19:33 video at
   `segmentDuration/300`).
3. `GET {sub_playlist_base}/segmentIndex/N.vtt` for each N (in order) and
   concatenate the bodies (`segment_1.vtt`..`segment_4.vtt` are real).

Text cleanup (`vtt_to_text`): keep cue text only — strip WEBVTT/NOTE/STYLE
headers, `-->` timestamp lines, inline `<...>` tags; drop CONSECUTIVE
duplicate cue lines; join with single spaces. `words` is
`split_whitespace().count()` of that cleaned text — NOT a raw token count
of the VTT (raw count includes timestamps: 3553 raw tokens vs 2224 words
for `0_88lztt5a`; see `qa/reference_transcript_counts.json`).

## 4. Caption/flavor metadata (Kaltura XML API — metadata only)

- `GET https://api.sg.kaltura.com/api_v3?service=caption_captionAsset&action=list&filter:entryIdEqual={entry}&ks={ks}`
  -> `caption_assets_one.xml`. NOTE the filter is `filter:entryIdEqual`
  (plain `entryId` fails silently).
- Same with `service=flavorAsset` -> `flavor_assets_two.xml` /
  `flavor_assets_three.xml`.

Fields used: caption `id`, `language`, `label`, `isDefault`; flavor `id`,
`width`, `height`, `sizeInBytes` (u64 — originals run to 1.7 GB;
`qa/flavor_assets_1p7gb.xml`), `isOriginal`, `version`, `fileExt`.

Metadata traps (all real):
- The ORIGINAL flavor of `0_88lztt5a` reports `sizeInBytes=0` while its
  legacy `<size>` (KB) holds the real ~266 MB. Quality selection must not
  let a zero/nonsense `sizeInBytes` win "smallest".
- `sizeInBytes` can also be STALE vs the CDN: flavor `0_z24etxpq` (540p)
  reports 150,718,384 bytes, but the CDN's Content-Length is 68,660,862
  (live-verified). Never trust it for progress/verification — use the
  actual streamed byte count or a HEAD.
- An `INVALID_KS` error XML has the same envelope as a list response with
  an `<error>` node instead of `<objects>` — parse to an empty vec, never
  panic (`qa/caption_assets_error.xml`).
- Only the signed `serveWebVTT` URL from playManifest downloads captions
  (§3); these lists are for display and flavor picking only. A flavor's
  `<version>` goes into the video URL (§5).

## 5. Video download (open CDN — no auth, Range honored)

`https://cfvod.sgp2.ovp.kaltura.com/p/{partner}/sp/{partner}00/serveFlavor/entryId/{entry}/v/{flavor.version}/flavorId/{flavor.id}/name/a.{file_ext}`

Raw MP4; the CDN's `Content-Length` is authoritative (see the stale
`sizeInBytes` trap in §4). Stream to disk (chunked), never slurp: 540p of a
20-min lecture is ~68 MB, the 1080p original ~266 MB-1.7 GB. The CDN
sometimes RESETS long-lived connections mid-body (a 266 MB original died at
198 MB, live-verified) — it honors Range, so resume from the bytes already
written. Quality rules: `smallest` = fewest bytes with `height > 0` and a
sane nonzero size (legacy `<size>` KB as fallback when `sizeInBytes` is 0);
`best` = largest non-original; `original` = the `isOriginal` flavor.

## 6. Performance (whole-course transcripts)

Measured on CC0006 (25 videos, 99 VTT segments), same machine, warm
discovery cache, 2026-11-12:

| run | before | after |
|---|---|---|
| cold (full LTI chain) | 28.2 s | 4.2 s |
| warm (kaltura.json session hit) | 28.2 s | ~1.1 s |

What changed (in order of impact):

1. **Cross-run session cache** (`kaltura.json`, next to the cookie file —
   the pcv.json pattern): the player KS lives for hours and is
   course-wide, but every CLI invocation used to re-run the ~7-hop LTI
   chain. Now the second run skips it. TTL 1 h (the KS is opaque); the
   first rejection drops the entry.
2. **Bounded entry concurrency (12)**: videos were walked one at a time
   with a fixed 300 ms pause each; the bound replaces the pause as the
   politeness policy.
3. **Parallel VTT segments (6 per video)**: the segments of one video are
   independent CDN files fetched concurrently, playlist order preserved.

Failure-shape notes for the relaunch path:

- A dead KS on playManifest is **HTTP 404 with an empty body** (not
  401/INVALID_KS) — fetch_text maps that shape to a rejected session so
  the run self-heals instead of reporting every video broken. Live
  verification: a poisoned kaltura.json recovers in one relaunch (~5 s
  for the whole course).
- Under concurrency, eviction is conditional on the cached KS still being
  the rejected one (`invalidate_if_ks`): a straggler's failure must never
  evict a freshly re-launched session.

## 7. Tool surface

- `ntulearn_kaltura_list(course_id)` — discovery (§1) over the full content
  tree (parents + children, like `pcv_status` auto-discovery). Returns
  content_id, title, entry_id, placement_id per video; non-Kaltura LTI
  items are skipped silently.
- `ntulearn_kaltura_transcripts(course_id, content_id?, save_dir?, format?)`
  — omit `content_id` for every video in the course. Writes
  `{save_dir}/kaltura/{course_id}/{safe_title}.vtt` and `.txt`
  (`format` = `vtt|txt|both`, default both; txt = timestamps stripped).
  `save_dir` defaults to the state download dir. Per-video summary: title,
  entry, words, segments, label, file paths; "no captions" is a per-item
  NOTE. 0.3s politeness sleep between entries.
- `ntulearn_kaltura_download(course_id, content_id, quality?)` — §5, default
  `smallest`. Returns flavor chosen, bytes, path.

Response formats toon (default) / json / markdown via `render.rs`.

## 8. Rust module contract: `src/kaltura.rs`

See PLAN.md §1 (frozen): `KalturaEmbed`, `CaptionAsset`, `FlavorAsset`,
`Transcript`, `TranscriptError`, `KalturaSession` (`launch`, `ks`,
`partner_id`, `caption_assets`, `flavors`, `transcript`,
`download_video`), and the pure helpers `extract_embeds`,
`parse_flashvars`, `caption_track_from_manifest`, `segments_from_playlist`,
`vtt_to_text`, `parse_caption_assets_xml`, `parse_flavor_assets_xml`,
`parse_auto_submit_form`. Style: `pcv.rs` (doc comments, LLM-readable error
strings that mention `ntulearn_login` for auth-shaped failures, no unwrap
in non-test code, fixture-driven `#[cfg(test)]` tests).

## 9. Test matrix (live, post-merge — `scripts/verify-kaltura-e2e.sh`)

1. `kaltura-list` on `_2707070_1` finds exactly 25 videos, including
   `0_88lztt5a` under content `_5976141_1`.
2. `kaltura-transcripts` for `_5976141_1`: words within ±5% of the
   reference 2224 (`qa/reference_transcript_counts.json`), label "Edited",
   4 segments, `.vtt` + `.txt` written, and the txt opens with
   "A very warm welcome to Part 2 of this lecture on energy and climate
   change."
3. `kaltura-transcripts` for the whole course: every reference entry's word
   count within ±5%, `.txt` word count equals the reported words.
4. `kaltura-download --quality smallest` for `_5976141_1`: non-original
   flavor, `height > 0`, file size equals reported bytes, then deleted.
