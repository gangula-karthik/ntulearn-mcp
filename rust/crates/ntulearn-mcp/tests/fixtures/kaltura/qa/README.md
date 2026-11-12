# Kaltura QA fixture pack (adversarial)

Fabricated edge cases for the `src/kaltura.rs` parsers, plus one real
reference file. The real captured protocol fixtures live one level up in
`tests/fixtures/kaltura/` (core pack). Protocol background: repo-root
`KALTURA_SPEC.md`.

| Fixture | Must trigger |
|---|---|
| `body_html_weird.json` | 12 hostile content-item bodies for `extract_embeds`, each with its one-line expectation inline (`cases[].expect`): malformed bbfile JSON, non-Kaltura blti-links, missing `entryid/`, non-object bbfile, unicode/emoji titles, missing title, duplicate embeds, 3-good-plus-1-hostile, bbcswebdav-only, uppercase entry ids, empty body. |
| `manifest_crlf.m3u8` | Master manifest with CRLF line endings — `caption_track_from_manifest` must still return the `Edited` track (label `Edited`, language `en`). |
| `manifest_bom.m3u8` | UTF-8 BOM glued to `#EXTM3U` — must not hide the SUBTITLES line. |
| `manifest_no_subs.m3u8` | Variants reference `SUBTITLES="subs"` but no `#EXT-X-MEDIA` line exists — `caption_track_from_manifest` must return `None` (`TranscriptError::NoCaptions`), not a parse error. |
| `playlist_10segs.m3u8` | 10 `segmentIndex/N.vtt` entries with varied `#EXTINF` durations and a legal non-EXT comment line — `segments_from_playlist` must return exactly 10, in order, ignoring comments. |
| `vtt_malformed_timestamps.vtt` | Garbage timestamps, stray text, double timestamp line, consecutive duplicates. Must not panic. Expected `vtt_to_text` output: `good cue one malformed arrow line must not leak into text no timestamp at all stray text line double timestamp line duplicate line` |
| `vtt_empty.vtt` | Header only — `vtt_to_text` must return `""`. |
| `vtt_inline_tags.vtt` | `<v>`, `<c>`, `<b>/<i>` and inline `<00:00:02.500>` tags stripped; entities are NOT decoded (contract strips tags only). Expected: `Hello world &amp; welcome inline timestamp tag must vanish bold cue and italic cue` |
| `vtt_multiline_cues.vtt` | Multi-line cues joined with single spaces; only CONSECUTIVE duplicates collapsed (the non-adjacent repeat of "first line of cue two" survives). Expected: `first line of cue one second line of cue one first line of cue two second line of cue two first line of cue two` |
| `caption_assets_zero.xml` | Empty `<objects/>`, totalCount 0 — `parse_caption_assets_xml` returns an empty vec. |
| `caption_assets_error.xml` | Real `KalturaAPIException` envelope (`INVALID_KS`) where a list response is expected — must parse to an empty vec, never panic; callers treat it as a re-launch signal. |
| `flavor_assets_zero.xml` | Empty flavor list — `parse_flavor_assets_xml` returns an empty vec. |
| `flavor_assets_1p7gb.xml` | REAL capture (entry `0_td7lgutt`): the 1.7 GB original (`sizeInBytes` 1696369677 — needs u64) plus a 123 MB 540p. Exercises `size_bytes` parsing and "smallest" selection arithmetic. |
| `reference_transcript_counts.json` | REAL reference (live-verified 2026-11-12): entry/title/words for all 25 CC0006 videos. `words` counts the cleaned text (`vtt_to_text` + `split_whitespace`), NOT raw VTT tokens (raw `0_88lztt5a` VTT is 3553 whitespace tokens; cleaned words = 2224). Used by `scripts/verify-kaltura-e2e.sh` with a ±5% tolerance. |
