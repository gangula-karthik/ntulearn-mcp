//! Kaltura transcript / video-download integration — WIRING STUB.
//!
//! This file carries ONLY the frozen interface contract (PLAN.md §1) so the
//! tool + CLI wiring (schemas.rs, cli.rs, handlers.rs, render.rs) compiles
//! and its unit tests run before the kaltura CORE module (branch
//! feat/kaltura-core) lands. Every function body is a deliberate failure or
//! empty result; the integrator replaces this file WHOLESALE with the real
//! implementation at merge time. See KALTURA_SPEC.md for the live-verified
//! protocol this module will implement.

use std::path::Path;

use serde::Serialize;

use crate::AppState;

/// Every stub body reports the same reason: the caller wired against the
/// contract, the implementation just is not merged yet.
const STUB_ERR: &str = "kaltura core not merged yet";

/// One Kaltura video embedded in a Blackboard content item.
#[derive(Debug, Clone, Serialize)]
pub struct KalturaEmbed {
    /// BB content item id, e.g. "_5976141_1".
    pub content_id: String,
    /// data-bbfile "title" ("" if absent).
    pub title: String,
    /// Kaltura entry id, e.g. "0_88lztt5a".
    pub entry_id: String,
    /// linkRefId (LTI placement id), e.g. "_1326_1".
    pub placement_id: String,
    /// Full KAF embed url from data-bbfile.
    pub embed_url: String,
}

/// Parse data-bbfile Kaltura embeds from a content item body HTML.
/// Skips malformed JSON, non-LTI bbfiles, and non-Kaltura LTI links
/// (url must contain "/browseandembed/" and "entryid/"). Never errors.
pub fn extract_embeds(_body_html: &str) -> Vec<KalturaEmbed> {
    Vec::new()
}

/// One caption track attached to a Kaltura entry (XML API metadata).
#[derive(Debug, Clone, Serialize)]
pub struct CaptionAsset {
    pub id: String,
    pub language: String,
    pub label: String,
    pub is_default: bool,
}

/// One playable flavor (rendition) of a Kaltura entry.
#[derive(Debug, Clone, Serialize)]
pub struct FlavorAsset {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub size_bytes: u64,
    pub is_original: bool,
    pub version: u32,
    pub file_ext: String,
}

/// A concatenated caption track: the raw VTT, its plain text, and counts.
#[derive(Debug, Clone, Serialize)]
pub struct Transcript {
    /// Concatenated segment bodies.
    pub vtt: String,
    /// vtt_to_text(vtt).
    pub text: String,
    /// text.split_whitespace().count().
    pub words: usize,
    /// Number of VTT segment files fetched.
    pub segments: usize,
    /// EXT-X-MEDIA NAME ("Edited", "English", ...).
    pub label: String,
}

/// Why a transcript could not be fetched.
#[derive(Debug)]
pub enum TranscriptError {
    /// The manifest had no SUBTITLES line.
    NoCaptions,
    /// Transport / non-200.
    Network(String),
    /// Malformed m3u8/playlist.
    Parse(String),
}

impl TranscriptError {
    /// LLM-readable message; mentions ntulearn_login when it smells like
    /// auth. STUB: always the stub marker — the real module owns the wording.
    pub fn message(&self) -> String {
        STUB_ERR.to_string()
    }
}

/// A launched Kaltura session (LTI 1.3 chain done, player KS minted).
pub struct KalturaSession {
    /// Placeholder fields so the stub type-checks; never constructed.
    _ks: String,
    _partner_id: u32,
}

impl KalturaSession {
    /// Full LTI 1.3 OIDC chain for one content item, then the embed page
    /// fetch to mint a player KS. The session is cached per course_id; the
    /// minted KS is valid for ANY entry of the same course (live-verified).
    pub async fn launch(
        _state: &AppState,
        _course_id: &str,
        _content_id: &str,
    ) -> Result<Self, String> {
        Err(STUB_ERR.to_string())
    }

    /// The minted player KS (v2, base64-ish).
    pub fn ks(&self) -> &str {
        ""
    }

    /// Partner id parsed from flashvars "wid" (117 for NTU).
    pub fn partner_id(&self) -> u32 {
        0
    }

    pub async fn caption_assets(&self, _entry_id: &str) -> Result<Vec<CaptionAsset>, String> {
        Err(STUB_ERR.to_string())
    }

    pub async fn flavors(&self, _entry_id: &str) -> Result<Vec<FlavorAsset>, String> {
        Err(STUB_ERR.to_string())
    }

    pub async fn transcript(&self, _entry_id: &str) -> Result<Transcript, TranscriptError> {
        Err(TranscriptError::Network(STUB_ERR.to_string()))
    }

    /// Stream the MP4 for one flavor to `path`. Returns bytes written.
    pub async fn download_video(
        &self,
        _entry_id: &str,
        _flavor: &FlavorAsset,
        _path: &Path,
    ) -> Result<u64, String> {
        Err(STUB_ERR.to_string())
    }
}

/// `var flashvars = {...};` -> the whole JSON value; callers read "ks" and
/// "wid". STUB: always None.
pub fn parse_flashvars(_page_html: &str) -> Option<serde_json::Value> {
    None
}

/// A SUBTITLES track referenced by a master m3u8 manifest.
pub struct CaptionTrackRef {
    pub uri: String,
    pub label: String,
    pub language: String,
}

/// `#EXT-X-MEDIA:TYPE=SUBTITLES,...,NAME="Edited",...,URI="https://..."`.
/// STUB: always None.
pub fn caption_track_from_manifest(_m3u8: &str) -> Option<CaptionTrackRef> {
    None
}

/// segmentIndex/N.vtt entries of the caption sub-playlist. STUB: always [].
pub fn segments_from_playlist(_playlist: &str) -> Vec<String> {
    Vec::new()
}

/// Cue text only: strip WEBVTT headers/timestamps/inline <..> tags, drop
/// consecutive duplicate lines, join with single spaces. STUB: always "".
pub fn vtt_to_text(_vtt: &str) -> String {
    String::new()
}

/// captionAsset.list XML -> assets. Invalid KS or empty lists give [].
/// STUB: always [].
pub fn parse_caption_assets_xml(_xml: &str) -> Vec<CaptionAsset> {
    Vec::new()
}

/// flavorAsset.list XML -> flavors. Invalid KS or empty lists give [].
/// STUB: always [].
pub fn parse_flavor_assets_xml(_xml: &str) -> Vec<FlavorAsset> {
    Vec::new()
}

/// The BB authenticate auto-submit form -> (action, [(name, value)]).
/// Fields: id_token, state. Missing form => None. STUB: always None.
pub fn parse_auto_submit_form(_html: &str) -> Option<(String, Vec<(String, String)>)> {
    None
}
