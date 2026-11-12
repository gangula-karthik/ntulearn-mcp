//! Kaltura video + transcript integration.
//!
//! Kaltura videos are embedded in NTULearn course documents as
//! `data-bbfile` LTI links to the university's KAF instance
//! (`ntulearnv1.ntu.edu.sg`). There is no direct login: captions and flavor
//! metadata live on Kaltura's API and are authorized by a player KS minted
//! by the LTI 1.3 launch chain (KALTURA_SPEC.md §1–§2). This module owns
//! that chain plus the pure parsers for every Kaltura wire format involved:
//! the content-body embed JSON, the embed page's `flashvars`, the HLS
//! master/sub playlists, WebVTT segments, and the XML asset lists.
//!
//! Differences from pcv.rs / fbf.rs that matter:
//! * Discovery needs NO launch: the entry id is in the content body's
//!   data-bbfile embed, so `extract_embeds` is pure REST + parsing. Only
//!   transcripts/flavors/metadata need the launched session.
//! * The entry executor is `launchPlacement` (like pcv.rs) but the placement
//!   id comes straight from the data-bbfile `linkRefId` — no classic-page
//!   scraping. `course_id` + `content_id` query params are REQUIRED on the
//!   first hop (omitting them 500s server-side).
//! * The chain lands on `ntulearnv1.ntu.edu.sg` (KAF), whose page mints a
//!   player KS valid for ANY entry of the same course — so the session is
//!   cached per course_id, not per content item.
//! * Caption download does NOT use captionAsset.serve (it 200s empty with a
//!   player KS): the only verified route is the signed serveWebVTT HLS
//!   sub-playlist inside the playManifest master (KALTURA_SPEC.md §2).
//! * Video files come off an open CDN (`cfvod.sgp2.ovp.kaltura.com`) — no
//!   auth at all, just the flavor id + version.
//!
//! Design notes shared with pcv.rs / fbf.rs:
//! * The workspace reqwest does NOT enable the `cookies` feature, so the LTI
//!   chain runs on a small hand-rolled cookie jar with manual redirect
//!   following (each module keeps its own; the chain here never leaves
//!   NTULearn/KAF/developer.blackboard.com origins).
//! * The BbRouter header is only attached to the NTULearn origin — the chain
//!   passes through developer.blackboard.com and KAF, and leaking a session
//!   cookie to third parties is exactly what a browser would not do.
//! * login_hint tokens in the oidc-init URL are SINGLE-USE: a fresh launch
//!   ALWAYS restarts from the placement URL; no intermediate chain URL is
//!   ever persisted or replayed.
//! * The minted player KS is an opaque "v2|..." token with no readable
//!   expiry, so the session cache is unexpired in-memory state that is
//!   dropped wholesale the moment Kaltura answers INVALID_KS/401 — the next
//!   tool call re-runs the chain (mirroring the "refresh = re-launch" policy
//!   of the other LTI modules).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::AppState;

/// A desktop Chrome UA for the LTI chain. The chain impersonates the browser
/// the user would launch the tool from; some hops behave differently for
/// non-browser user agents. API calls after landing also carry it (harmless).
const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
                          AppleWebKit/537.36 (KHTML, like Gecko) \
                          Chrome/130.0.0.0 Safari/537.36";
/// Cap on manually-followed redirects. The observed chain is 3 GET hops + 1
/// POST + the embed page; anything beyond this is a loop, not a launch.
const MAX_REDIRECTS: usize = 12;
/// The NTU KAF origin: host of the embed pages that mint the player KS, the
/// Origin/Referer the launch POST identifies as, and the referrer the
/// playManifest endpoint checks (base64-encoded into the URL).
const KAF_ORIGIN: &str = "https://ntulearnv1.ntu.edu.sg";
/// Host form of [`KAF_ORIGIN`] (cookie collection target after the POST).
const KAF_HOST: &str = "ntulearnv1.ntu.edu.sg";
/// Kaltura XML API origin for caption/flavor asset metadata.
const API_XML_BASE: &str = "https://api.sg.kaltura.com/api_v3";
/// Kaltura playManifest origin (HLS master playlists).
const MANIFEST_BASE: &str = "https://api.sgp2.ovp.kaltura.com";
/// Open CDN origin for raw MP4 flavors (no auth; Range honored).
const CDN_BASE: &str = "https://cfvod.sgp2.ovp.kaltura.com";

// ===========================================================================
// Cookie jar (reqwest here has no `cookies` feature; see module docs)
// ===========================================================================

/// Minimal cookie jar: domain -> (name -> (value, host_only)). Enough for
/// the LTI chain, where cookies are set on the NTULearn/KAF domains and
/// re-sent on the next hop. No expiry handling — the chain lives seconds,
/// and stale cookies are overwritten by fresh Set-Cookie headers.
#[derive(Default)]
struct CookieJar {
    cookies: HashMap<String, HashMap<String, (String, bool)>>,
}

impl CookieJar {
    /// Ingest Set-Cookie header values received for a request to `host`.
    /// Only the `Domain` attribute matters for the chain; `Secure`/`Path`
    /// conditions always hold for our https, same-path usage.
    fn ingest(&mut self, host: &str, values: &[String]) {
        for raw in values {
            let mut parts = raw.split(';');
            let Some(first) = parts.next() else { continue };
            let Some((name, value)) = first.split_once('=') else {
                continue;
            };
            let name = name.trim().to_string();
            let value = value.trim().to_string();
            if name.is_empty() {
                continue;
            }
            let mut domain = host.to_ascii_lowercase();
            let mut host_only = true;
            for attr in parts {
                let attr = attr.trim();
                if let Some(d) = attr
                    .strip_prefix("Domain=")
                    .or_else(|| attr.strip_prefix("domain="))
                {
                    domain = d.trim().trim_start_matches('.').to_ascii_lowercase();
                    host_only = false;
                }
            }
            self.cookies
                .entry(domain)
                .or_default()
                .insert(name, (value, host_only));
        }
    }

    /// `name=value` pairs applicable to `host`: exact-domain cookies plus
    /// Domain-scoped cookies for subdomains, per RFC 6265 matching.
    fn pairs_for(&self, host: &str) -> Vec<String> {
        let h = host.to_ascii_lowercase();
        let mut out = Vec::new();
        for (domain, cookies) in &self.cookies {
            let subdomain = h.ends_with(&format!(".{domain}"));
            for (name, (value, host_only)) in cookies {
                if h == *domain || (subdomain && !*host_only) {
                    out.push(format!("{name}={value}"));
                }
            }
        }
        out
    }
}

fn set_cookie_values(headers: &reqwest::header::HeaderMap) -> Vec<String> {
    headers
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok().map(|s| s.to_string()))
        .collect()
}

/// Cookie header value for a request to `host`: jar cookies plus the BbRouter
/// session cookie — but ONLY on the NTULearn origin (see module docs).
fn cookie_header(jar: &CookieJar, host: &str, bb_host: &str, bb_router: &str) -> Option<String> {
    let mut pairs = jar.pairs_for(host);
    if !bb_router.is_empty() && host.eq_ignore_ascii_case(bb_host) {
        pairs.push(format!("BbRouter={bb_router}"));
    }
    if pairs.is_empty() {
        None
    } else {
        Some(pairs.join("; "))
    }
}

/// The cookie map for `host` out of a launch jar — the KAF session state a
/// [`KalturaSession`] keeps so embed pages can be re-fetched without a
/// re-launch.
fn jar_cookie_map(jar: &CookieJar, host: &str) -> HashMap<String, String> {
    jar.pairs_for(host)
        .into_iter()
        .filter_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            Some((name.to_string(), value.to_string()))
        })
        .collect()
}

// ===========================================================================
// HTTP for the LTI chain (manual redirects, shared jar)
// ===========================================================================

/// One final (non-redirect) response of the chain.
struct Page {
    url: reqwest::Url,
    status: u16,
    body: String,
}

/// Client for the launch chain: no automatic redirects (we follow them by
/// hand to observe Set-Cookie at every hop), 30s timeout like client.rs.
fn chain_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(BROWSER_UA)
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("Kaltura launch: reqwest client build failed: {e}"))
}

/// GET with browser-ish headers, following redirects manually and feeding
/// every hop's Set-Cookie into the jar. The chain is short and linear; a
/// loop beyond MAX_REDIRECTS is reported instead of spun forever.
async fn get_following(
    http: &reqwest::Client,
    jar: &mut CookieJar,
    start: reqwest::Url,
    bb_host: &str,
    bb_router: &str,
) -> Result<Page, String> {
    let mut url = start.clone();
    for _ in 0..=MAX_REDIRECTS {
        let host = url.host_str().unwrap_or_default().to_string();
        let mut req = http
            .get(url.clone())
            .header(reqwest::header::USER_AGENT, BROWSER_UA)
            .header(
                reqwest::header::ACCEPT,
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9");
        if let Some(cookies) = cookie_header(jar, &host, bb_host, bb_router) {
            req = req.header(reqwest::header::COOKIE, cookies);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("Kaltura launch: GET {url} failed: {e}"))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        jar.ingest(&host, &set_cookie_values(&headers));
        if (300..400).contains(&status) {
            let location = headers
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    format!("Kaltura launch: HTTP {status} from {url} has no Location header")
                })?;
            url = url.join(location).map_err(|e| {
                format!(
                    "Kaltura launch: cannot resolve redirect target {location:?} \
                     against {url}: {e}"
                )
            })?;
            continue;
        }
        let body = resp
            .text()
            .await
            .map_err(|e| format!("Kaltura launch: reading body of {url} failed: {e}"))?;
        return Ok(Page { url, status, body });
    }
    Err(format!(
        "Kaltura launch: more than {MAX_REDIRECTS} redirects starting at {start}"
    ))
}

/// POST the LTI auto-submit form fields to the action URL, then follow any
/// redirects to the final 200. Origin/Referer describe the KAF origin the
/// form posts into (KALTURA_SPEC.md §1 step 5), matching the live-verified
/// request. Only the first request is the POST; later hops are GETs
/// (browser semantics for 301/302/303 after a POST).
async fn post_launch_form(
    http: &reqwest::Client,
    jar: &mut CookieJar,
    action: reqwest::Url,
    fields: &[(String, String)],
    bb_host: &str,
    bb_router: &str,
) -> Result<Page, String> {
    let body = form_urlencode(fields);
    let mut url = action.clone();
    let mut first_post = true;
    for _ in 0..=MAX_REDIRECTS {
        let host = url.host_str().unwrap_or_default().to_string();
        let mut req = if first_post {
            http.post(url.clone())
                .header(reqwest::header::USER_AGENT, BROWSER_UA)
                .header(
                    reqwest::header::ACCEPT,
                    "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
                )
                .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .header(reqwest::header::ORIGIN, KAF_ORIGIN)
                .header(reqwest::header::REFERER, KAF_ORIGIN)
                .body(body.clone())
        } else {
            http.get(url.clone())
                .header(reqwest::header::USER_AGENT, BROWSER_UA)
                .header(
                    reqwest::header::ACCEPT,
                    "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
                )
                .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
        };
        first_post = false;
        if let Some(cookies) = cookie_header(jar, &host, bb_host, bb_router) {
            req = req.header(reqwest::header::COOKIE, cookies);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("Kaltura launch: POST to {url} failed: {e}"))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        jar.ingest(&host, &set_cookie_values(&headers));
        if (300..400).contains(&status) {
            let location = headers
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    format!("Kaltura launch: HTTP {status} from {url} has no Location header")
                })?;
            url = url.join(location).map_err(|e| {
                format!(
                    "Kaltura launch: cannot resolve redirect target {location:?} \
                     against {url}: {e}"
                )
            })?;
            continue;
        }
        let text = resp
            .text()
            .await
            .map_err(|e| format!("Kaltura launch: reading body of {url} failed: {e}"))?;
        return Ok(Page {
            url,
            status,
            body: text,
        });
    }
    Err(format!(
        "Kaltura launch: more than {MAX_REDIRECTS} redirects after POST to {action}"
    ))
}

/// application/x-www-form-urlencoded body for the launch fields. Encoded by
/// hand (percent-encoding of everything non-alphanumeric) so the JWT/state
/// values need no escaping assumptions; a server decodes %XX identically.
fn form_urlencode(fields: &[(String, String)]) -> String {
    use percent_encoding::NON_ALPHANUMERIC;
    use percent_encoding::utf8_percent_encode;
    fields
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                utf8_percent_encode(k, NON_ALPHANUMERIC),
                utf8_percent_encode(v, NON_ALPHANUMERIC)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// First 300 chars of an error body, single line (same shape as client.rs's
/// API error snippets) so error strings stay readable in tool output.
fn snippet(text: &str) -> String {
    text.chars()
        .take(300)
        .collect::<String>()
        .replace('\n', " ")
}

// ===========================================================================
// Discovery: data-bbfile Kaltura embeds in content body HTML (no launch)
// ===========================================================================

/// A Kaltura video embedded in a course document: everything the tools need
/// before any launch happens. `content_id` is stamped by the caller that
/// knows which content item the body came from ([`extract_embeds_for`]).
#[derive(Debug, Clone, Serialize)]
pub struct KalturaEmbed {
    /// BB content item id, e.g. "_5976141_1".
    pub content_id: String,
    /// data-bbfile "title" ("" when the embed carries none).
    pub title: String,
    /// Kaltura entry id, e.g. "0_88lztt5a".
    pub entry_id: String,
    /// LTI placement id (data-bbfile "linkRefId"), e.g. "_1326_1" — the
    /// launch chain's first hop is keyed by it.
    pub placement_id: String,
    /// Full KAF embed URL from data-bbfile (the page that mints the KS).
    pub embed_url: String,
}

/// The Kaltura entry id out of a KAF embed URL: the path segment after
/// `entryid/` (case-insensitive), which must look like `0_<hash>` per the
/// live-observed id shape (`entryid/(0_[a-z0-9]+)`, case-insensitive).
fn entry_id_from_url(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    let pos = lower.find("/entryid/")?;
    let rest = &url[pos + "/entryid/".len()..];
    let seg: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let (_, hash) = seg.split_once('_')?;
    if seg.starts_with("0_") && !hash.is_empty() && hash.chars().all(|c| c.is_ascii_alphanumeric())
    {
        Some(seg)
    } else {
        None
    }
}

/// Parse data-bbfile Kaltura embeds from a content item body HTML, stamping
/// each with `content_id`. Skips malformed JSON, non-LTI bbfiles, and
/// non-Kaltura LTI links (the url must contain `/browseandembed/` and
/// `entryid/` — FeedbackFruits links share the linkType but not the URL
/// shape). An identical embed (same entry + placement) repeated in one body
/// is returned once; distinct placements of the same entry are all kept.
/// Never errors — a body without embeds is an empty vec.
pub fn extract_embeds_for(content_id: &str, body_html: &str) -> Vec<KalturaEmbed> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(body_html);
    let Ok(sel) = Selector::parse("[data-bbfile]") else {
        return Vec::new();
    };
    let mut out: Vec<KalturaEmbed> = Vec::new();
    for el in doc.select(&sel) {
        // scraper's HTML parser has already entity-decoded the attribute
        // value (&quot; -> "), so this is the raw JSON.
        let raw = el.value().attr("data-bbfile").unwrap_or_default();
        let Ok(bbfile) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        if bbfile.get("linkType").and_then(|v| v.as_str()) != Some("resource/x-bb-blti-link") {
            continue;
        }
        let Some(url) = bbfile.get("url").and_then(|v| v.as_str()) else {
            continue;
        };
        let url_lower = url.to_ascii_lowercase();
        if !url_lower.contains("/browseandembed/") || !url_lower.contains("entryid/") {
            continue;
        }
        let Some(entry_id) = entry_id_from_url(url) else {
            continue;
        };
        let placement_id = bbfile
            .get("linkRefId")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        // A body that embeds the same video twice lists it once; the same
        // video under two different placements is two distinct embeds.
        if out
            .iter()
            .any(|e| e.entry_id == entry_id && e.placement_id == placement_id)
        {
            continue;
        }
        out.push(KalturaEmbed {
            content_id: content_id.to_string(),
            title: bbfile
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            entry_id,
            placement_id,
            embed_url: url.to_string(),
        });
    }
    out
}

/// [`extract_embeds_for`] without a content id to stamp — the embeds come
/// back with an empty `content_id`; use [`extract_embeds_for`] when the
/// originating content item is known.
pub fn extract_embeds(body_html: &str) -> Vec<KalturaEmbed> {
    extract_embeds_for("", body_html)
}

// ===========================================================================
// Embed page parsing: flashvars + partner id
// ===========================================================================

/// Extract the embed page's `var flashvars = {...};` object as JSON. The
/// object literal /// A KAF "cookie check" interstitial: the embed URL serves a skeleton page
/// that JavaScript-redirects (`window.location.href = "..."`) to the real
/// `media-redirect` page carrying the player flashvars. Extract that target
/// (same-origin absolute URL; `\/` escapes unescaped) for the HTTP-only
/// walker to follow.
pub fn js_location_href(page_html: &str) -> Option<String> {
    let start = page_html.find("window.location.href")?;
    let rest = &page_html[start..];
    let q1 = rest.find('"')?;
    let rest = &rest[q1 + 1..];
    let q2 = rest.find('"')?;
    Some(rest[..q2].replace("\\/", "/"))
}

/// `var flashvars = {...}` on a KAF media page: the JSON object is brace-scanned with string awareness and parsed by
/// serde_json, so a page without a parseable flashvars yields None.
/// Callers read "ks" (the player KS) and, on pages that carry it, "wid"
/// from the value (the real NTU pages carry wid next to it instead —
/// see [`partner_id_from_page`]).
pub fn parse_flashvars(page_html: &str) -> Option<Value> {
    let start = page_html.find("var flashvars")?;
    let after = page_html[start + "var flashvars".len()..].trim_start();
    let after = after.strip_prefix('=')?;
    let after = after.trim_start();
    let open = after.strip_prefix('{')?;
    // Balanced-brace scan honoring JSON strings and escapes.
    let mut depth = 1usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in open.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let text = format!("{{{}}}", &open[..i]);
                    return serde_json::from_str(&text).ok();
                }
            }
            _ => {}
        }
    }
    None
}

/// The Kaltura partner id out of a `wid` value ("_117" -> 117).
fn parse_partner_wid(wid: &str) -> Option<u32> {
    wid.trim().trim_start_matches('_').parse::<u32>().ok()
}

/// The Kaltura partner id off an embed page: the `wid` token in the page's
/// player bootstrap (`'wid': '_117'` or `"wid":"_117"` — the real pages
/// carry it next to the flashvars, not inside them). Non-numeric or absent
/// wid values are skipped, not guessed.
pub fn partner_id_from_page(page_html: &str) -> Option<u32> {
    for (i, _) in page_html.match_indices("wid") {
        let after = page_html[i + "wid".len()..].trim_start();
        // the real bootstraps quote the key: 'wid': '_117' / "wid":"_117"
        let after = match after.strip_prefix('\'').or_else(|| after.strip_prefix('"')) {
            Some(rest) => rest.trim_start(),
            None => after,
        };
        let Some(after) = after.strip_prefix(':') else {
            continue;
        };
        let after = after.trim_start();
        let Some(quote) = after.chars().next() else {
            continue;
        };
        if quote != '"' && quote != '\'' {
            continue;
        }
        let value: String = after[1..].chars().take_while(|c| *c != quote).collect();
        if let Some(partner) = parse_partner_wid(&value) {
            return Some(partner);
        }
    }
    None
}

// ===========================================================================
// LTI auto-submit form (authenticate page)
// ===========================================================================

/// The BB authenticate auto-submit form -> (action, [(name, value)]).
/// Blackboard's launchPlacement?cmd=authenticate page contains exactly one
/// form posting hidden `id_token` (and usually `state`) to the KAF
/// oauth2-launch endpoint; every named input is returned so the POST is a
/// faithful browser submit. A form without a non-empty `id_token`, or a
/// page without a matching form, yields None (auth-shaped: the caller
/// reports it as a stale BbRouter session).
pub fn parse_auto_submit_form(html: &str) -> Option<(String, Vec<(String, String)>)> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let form_sel = Selector::parse("form").ok()?;
    let input_sel = Selector::parse("input").ok()?;
    for form in doc.select(&form_sel) {
        let mut fields: Vec<(String, String)> = Vec::new();
        let mut has_id_token = false;
        for input in form.select(&input_sel) {
            let name = input.value().attr("name").unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let value = input.value().attr("value").unwrap_or("");
            if name == "id_token" && !value.is_empty() {
                has_id_token = true;
            }
            fields.push((name.to_string(), value.to_string()));
        }
        if has_id_token {
            if let Some(action) = form.value().attr("action") {
                if !action.is_empty() {
                    return Some((action.to_string(), fields));
                }
            }
        }
    }
    None
}

// ===========================================================================
// HLS playlists (master manifest + caption sub-playlist)
// ===========================================================================

/// One caption track referenced by a master HLS manifest.
pub struct CaptionTrackRef {
    /// Absolute serveWebVTT sub-playlist URL (already signed with a wrapped
    /// KS — fetch it verbatim).
    pub uri: String,
    /// EXT-X-MEDIA NAME ("Edited", "English", ...) — the track label.
    pub label: String,
    /// EXT-X-MEDIA LANGUAGE ("en", ...).
    pub language: String,
}

/// Split an HLS attribute list on top-level commas (quoted strings may
/// contain commas), then `KEY=VALUE` into a map with quotes stripped.
fn parse_hls_attrs(attrs: &str) -> HashMap<String, String> {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for c in attrs.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                current.push(c);
            }
            ',' if !in_quotes => {
                parts.push(current.clone());
                current.clear();
            }
            _ => current.push(c),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current);
    }
    let mut map = HashMap::new();
    for part in parts {
        if let Some((key, value)) = part.split_once('=') {
            map.insert(
                key.trim().to_string(),
                value.trim().trim_matches('"').to_string(),
            );
        }
    }
    map
}

/// The SUBTITLES `#EXT-X-MEDIA` track out of a playManifest master playlist:
/// `#EXT-X-MEDIA:TYPE=SUBTITLES,...,NAME="Edited",...,URI="https://..."`.
/// Attribute order is not relied on; a manifest with no SUBTITLES line (a
/// video without captions) yields None. A leading BOM is tolerated.
pub fn caption_track_from_manifest(m3u8: &str) -> Option<CaptionTrackRef> {
    for line in m3u8.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("#EXT-X-MEDIA:") else {
            continue;
        };
        let attrs = parse_hls_attrs(rest);
        if attrs.get("TYPE").map(String::as_str) != Some("SUBTITLES") {
            continue;
        }
        // a SUBTITLES line without a usable URI is skipped, not fatal — a
        // later line may carry the real track
        let Some(uri) = attrs.get("URI").filter(|u| !u.is_empty()) else {
            continue;
        };
        return Some(CaptionTrackRef {
            uri: uri.clone(),
            label: attrs.get("NAME").cloned().unwrap_or_default(),
            language: attrs.get("LANGUAGE").cloned().unwrap_or_default(),
        });
    }
    None
}

/// The `segmentIndex/N.vtt` entries of a caption sub-playlist, in order.
/// Comment/`#EXT` lines and blanks are skipped; anything else is a segment
/// URI (relative to the playlist's own URL). A leading BOM is tolerated.
pub fn segments_from_playlist(playlist: &str) -> Vec<String> {
    playlist
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

// ===========================================================================
// WebVTT
// ===========================================================================

/// Strip inline `<..>` cue tags (<c>, </c>, <v Speaker>, <00:00:01.000>)
/// from a cue text line.
fn strip_inline_tags(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_tag = false;
    for c in line.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Decode the handful of HTML entities WebVTT cue text may carry.
fn decode_entities(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
}

/// Cue text only: strip WEBVTT headers, NOTE blocks, cue identifiers, and
/// timestamp lines, strip inline tags, decode entities, drop consecutive
/// duplicate lines (segment boundaries repeat the boundary cue), and join
/// the surviving lines with single spaces. Robust to arbitrary garbage.
pub fn vtt_to_text(vtt: &str) -> String {
    let lines: Vec<&str> = vtt.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut in_note = false;
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            in_note = false;
            continue;
        }
        if in_note {
            continue;
        }
        if line.starts_with("WEBVTT") {
            continue;
        }
        if line.starts_with("NOTE") {
            in_note = true;
            continue;
        }
        if line.contains("-->") {
            continue;
        }
        // A cue identifier is a line whose successor is the timestamp line.
        let next_is_timing = lines.get(i + 1).is_some_and(|next| next.contains("-->"));
        if next_is_timing {
            continue;
        }
        let text = decode_entities(&strip_inline_tags(line)).trim().to_string();
        if text.is_empty() {
            continue;
        }
        if out.last().is_some_and(|kept| kept == &text) {
            continue;
        }
        out.push(text);
    }
    out.join(" ")
}

/// The cue blocks of one VTT segment with its `WEBVTT` header (and any
/// header metadata up to the first blank line) removed — the concatenable
/// body used to build one single-header transcript file out of N segments.
fn vtt_body_after_header(vtt: &str) -> &str {
    let trimmed = vtt.trim_start();
    if let Some(rest) = trimmed.strip_prefix("WEBVTT") {
        match rest.find("\n\n") {
            Some(pos) => &rest[pos + 2..],
            None => "",
        }
    } else {
        vtt
    }
}

// ===========================================================================
// Kaltura XML asset lists (scraper parses them fine — balanced tags, no
// mixed content; entity decoding comes for free)
// ===========================================================================

/// One caption track asset of an entry.
#[derive(Debug, Clone, Serialize)]
pub struct CaptionAsset {
    pub id: String,
    /// Human language name ("English"); may be empty.
    pub language: String,
    /// Track label ("Edited"); may be empty.
    pub label: String,
    pub is_default: bool,
}

/// One video flavor (rendition) of an entry.
#[derive(Debug, Clone, Serialize)]
pub struct FlavorAsset {
    pub id: String,
    pub width: u32,
    pub height: u32,
    /// From <sizeInBytes> (the <size> sibling is kilobytes); 0 when absent.
    pub size_bytes: u64,
    pub is_original: bool,
    /// Flavor version — required by the CDN download URL.
    pub version: u32,
    /// File extension ("mp4"); empty when absent.
    pub file_ext: String,
}

/// One downloaded transcript: the concatenated VTT and its plain text.
#[derive(Debug, Clone, Serialize)]
pub struct Transcript {
    /// All segment cue blocks under a single WEBVTT header.
    pub vtt: String,
    /// [`vtt_to_text`] of `vtt` (timestamps stripped).
    pub text: String,
    /// Whitespace-separated word count of `text`.
    pub words: usize,
    /// Number of VTT segment files fetched.
    pub segments: usize,
    /// EXT-X-MEDIA NAME of the caption track ("Edited", "English", ...).
    pub label: String,
}

/// Why a transcript could not be fetched. `NoCaptions` is a per-video
/// property (the handler layer reports it as a note, not a call error);
/// Network/Parse carry LLM-readable detail via [`TranscriptError::message`].
#[derive(Debug)]
pub enum TranscriptError {
    /// The manifest had no SUBTITLES line — the video has no captions.
    NoCaptions,
    /// Transport failure or non-200 (auth-shaped detail mentions
    /// ntulearn_login).
    Network(String),
    /// Malformed m3u8/playlist (not an HLS document, or no segments).
    Parse(String),
}

impl TranscriptError {
    /// LLM-readable message; mentions ntulearn_login when the failure
    /// smells like an expired session.
    pub fn message(&self) -> String {
        match self {
            TranscriptError::NoCaptions => {
                "This Kaltura video has no caption track (the HLS manifest has no \
                 #EXT-X-MEDIA SUBTITLES entry), so there is no transcript to fetch. \
                 That is a property of the video, not a problem with the request — \
                 note it for this video and continue with the others."
                    .to_string()
            }
            TranscriptError::Network(detail) => {
                if detail.contains("INVALID_KS")
                    || detail.contains("HTTP 401")
                    || detail.contains("HTTP 403")
                {
                    format!(
                        "Kaltura transcript could not be fetched: {detail} The Kaltura \
                         session is no longer valid — retry the tool call so the LTI \
                         launch runs again, and call the ntulearn_login tool first if \
                         the NTULearn session itself has expired."
                    )
                } else {
                    format!("Kaltura transcript could not be fetched: {detail}")
                }
            }
            TranscriptError::Parse(detail) => {
                format!("Kaltura transcript data was malformed: {detail}")
            }
        }
    }
}

/// Text of the first descendant element named `tag` under `parent`.
fn xml_child_text(parent: &scraper::ElementRef, tag: &str) -> Option<String> {
    let sel = scraper::Selector::parse(tag).ok()?;
    parent
        .select(&sel)
        .next()
        .map(|el| el.text().collect::<String>())
}

fn xml_flag(parent: &scraper::ElementRef, tag: &str) -> bool {
    xml_child_text(parent, tag)
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}

fn xml_u32(parent: &scraper::ElementRef, tag: &str) -> u32 {
    xml_child_text(parent, tag)
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(0)
}

fn xml_u64(parent: &scraper::ElementRef, tag: &str) -> u64 {
    xml_child_text(parent, tag)
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

/// Parse a caption_captionAsset.list XML response into assets. Error XML
/// (INVALID_KS etc.), empty object lists, and items without an id yield
/// fewer or zero assets — never a panic. Check [`kaltura_api_error`] on the
/// raw XML first when an empty vec would be unexpected.
pub fn parse_caption_assets_xml(xml: &str) -> Vec<CaptionAsset> {
    let doc = scraper::Html::parse_document(xml);
    let Ok(item_sel) = scraper::Selector::parse("item") else {
        return Vec::new();
    };
    doc.select(&item_sel)
        .filter_map(|item| {
            Some(CaptionAsset {
                id: xml_child_text(&item, "id")?.trim().to_string(),
                language: xml_child_text(&item, "language")
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                label: xml_child_text(&item, "label")
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                is_default: xml_flag(&item, "isDefault"),
            })
        })
        .collect()
}

/// Parse a flavorAsset.list XML response into flavors. Same robustness
/// rules as [`parse_caption_assets_xml`]: never panics, missing optional
/// fields default to 0/""/false.
pub fn parse_flavor_assets_xml(xml: &str) -> Vec<FlavorAsset> {
    let doc = scraper::Html::parse_document(xml);
    let Ok(item_sel) = scraper::Selector::parse("item") else {
        return Vec::new();
    };
    doc.select(&item_sel)
        .filter_map(|item| {
            Some(FlavorAsset {
                id: xml_child_text(&item, "id")?.trim().to_string(),
                width: xml_u32(&item, "width"),
                height: xml_u32(&item, "height"),
                // sizeInBytes is the modern field but is 0 for original
                // uploads and can be stale vs the CDN (live-verified:
                // 0_z24etxpq reports 150 MB, the CDN serves 68 MB). When
                // it is 0/absent, fall back to the legacy <size> field,
                // which is in KB.
                size_bytes: {
                    let modern = xml_u64(&item, "sizeInBytes");
                    if modern > 0 {
                        modern
                    } else {
                        xml_u64(&item, "size").saturating_mul(1024)
                    }
                },
                is_original: xml_flag(&item, "isOriginal"),
                version: xml_u32(&item, "version"),
                file_ext: xml_child_text(&item, "fileExt")
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
            })
        })
        .collect()
}

/// The `<error><code>..</code><message>..</message></error>` body Kaltura
/// returns for rejected requests (INVALID_KS and friends), as one readable
/// string. None when the XML is a normal list response.
fn kaltura_api_error(xml: &str) -> Option<String> {
    let doc = scraper::Html::parse_document(xml);
    let err_sel = scraper::Selector::parse("error").ok()?;
    let err = doc.select(&err_sel).next()?;
    let code = xml_child_text(&err, "code").unwrap_or_default();
    let message = xml_child_text(&err, "message").unwrap_or_default();
    if code.is_empty() && message.is_empty() {
        return None;
    }
    Some(if code.is_empty() {
        format!("Kaltura API error: {message}")
    } else {
        format!("Kaltura API error {code}: {message}")
    })
}

// ===========================================================================
// URL builders (pure; every session request goes through one of these)
// ===========================================================================

/// Percent-encode a value for a query string (everything non-alphanumeric,
/// like pcv.rs's form encoding — servers decode %XX identically).
fn urlencode(value: &str) -> String {
    use percent_encoding::NON_ALPHANUMERIC;
    use percent_encoding::utf8_percent_encode;
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

/// The playManifest master-playlist URL for one entry. The referrer is the
/// base64 of the KAF origin (the endpoint checks it) and the KS is
/// percent-encoded because base64 uses reserved query characters.
pub fn manifest_url(partner_id: u32, entry_id: &str, ks: &str) -> String {
    use base64::Engine;
    let referrer = base64::engine::general_purpose::STANDARD.encode(KAF_ORIGIN);
    format!(
        "{MANIFEST_BASE}/p/{partner_id}/sp/{partner_id}00/playManifest/entryId/{entry_id}\
         /format/applehttp/protocol/https/a.m3u8?referrer={}&ks={}",
        urlencode(&referrer),
        urlencode(ks)
    )
}

/// The absolute URL of one caption segment inside a sub-playlist: the
/// segment entries are relative (`segmentIndex/N.vtt`) to the playlist's
/// own directory.
pub fn segment_url(playlist_uri: &str, segment: &str) -> String {
    match playlist_uri.rfind('/') {
        Some(pos) => format!("{}{}", &playlist_uri[..=pos], segment),
        None => segment.to_string(),
    }
}

/// The caption_captionAsset.list URL. NOTE the filter is
/// `filter:entryIdEqual` (plain `entryId` fails server-side).
pub fn caption_assets_url(ks: &str, entry_id: &str) -> String {
    format!(
        "{API_XML_BASE}?service=caption_captionAsset&action=list\
         &filter:entryIdEqual={entry_id}&ks={}",
        urlencode(ks)
    )
}

/// The flavorAsset.list URL (same filter shape as the caption list).
pub fn flavor_assets_url(ks: &str, entry_id: &str) -> String {
    format!(
        "{API_XML_BASE}?service=flavorAsset&action=list\
         &filter:entryIdEqual={entry_id}&ks={}",
        urlencode(ks)
    )
}

/// The open-CDN MP4 URL for one flavor. The flavor's version is part of the
/// path; an empty file extension falls back to "mp4" (every real flavor is
/// an mp4, and "a." would 404).
pub fn flavor_url(partner_id: u32, entry_id: &str, flavor: &FlavorAsset) -> String {
    let ext = if flavor.file_ext.is_empty() {
        "mp4"
    } else {
        &flavor.file_ext
    };
    format!(
        "{CDN_BASE}/p/{partner_id}/sp/{partner_id}00/serveFlavor/entryId/{entry_id}\
         /v/{}/flavorId/{}/name/a.{ext}",
        flavor.version, flavor.id
    )
}

// ===========================================================================
// Session cache (per course_id, in-memory; KS has no readable expiry)
// ===========================================================================

/// One cached launch: the player KS is valid for ANY entry of the course,
/// so one entry per course_id is enough. The opaque "v2|..." KS carries no
/// readable expiry — the cache is dropped on the first INVALID_KS/401
/// instead of aged out.
struct CachedKaltura {
    ks: String,
    partner_id: u32,
    cookies: HashMap<String, String>,
}

fn sessions() -> &'static Mutex<HashMap<String, CachedKaltura>> {
    static SESSIONS: OnceLock<Mutex<HashMap<String, CachedKaltura>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

// ===========================================================================
// KalturaSession
// ===========================================================================

/// An authenticated Kaltura session for one course: a player KS minted by
/// the LTI 1.3 launch chain of any Kaltura content item of that course,
/// valid for every entry in it (live-verified). Obtain via
/// [`KalturaSession::launch`]; the launch is cached per course_id so the
/// second tool call in a course is zero extra chain hops.
pub struct KalturaSession {
    course_id: String,
    ks: String,
    partner_id: u32,
    cookies: HashMap<String, String>,
    http: reqwest::Client,
}

impl KalturaSession {
    /// Full LTI 1.3 OIDC launch chain (KALTURA_SPEC.md §1) for one content
    /// item, then the embed page fetch that mints the player KS:
    ///
    /// 1. `GET .../blti/launchPlacement?blti_placement_id={placement_id}
    ///    &wrapped=true&course_id={course_id}&content_id={content_id}`
    ///    (BbRouter cookie, NTULearn origin only) -> 302 KAF oidc-init
    /// 2. oidc-init -> 307 developer.blackboard.com gateway (redirect walk
    ///    observes every Set-Cookie; no BbRouter leaves the NTULearn origin)
    /// 3. gateway -> back to NTULearn launchPlacement?cmd=authenticate
    /// 4. authenticate -> 200 auto-submit form (id_token [+ state])
    /// 5. POST the form to KAF oauth2-launch -> Kaltura session cookies
    /// 6. GET the data-bbfile embed URL with those cookies -> 200 entry
    ///    page whose `var flashvars = {...}` carries the player KS
    ///
    /// A cached session for the course is reused as-is; a fresh launch
    /// ALWAYS restarts from the placement URL (login_hint tokens are
    /// single-use). This is also the refresh path: [`Self::invalidate`]
    /// drops a rejected session, and the next launch re-runs the chain.
    pub async fn launch(
        state: &AppState,
        course_id: &str,
        content_id: &str,
    ) -> Result<Self, String> {
        // Fast path: a live session for this course (KS valid for any entry).
        {
            let cache = sessions();
            let Ok(guard) = cache.lock() else {
                return Err("Kaltura session cache lock poisoned".to_string());
            };
            if let Some(entry) = guard.get(course_id) {
                if !entry.ks.is_empty() && entry.partner_id != 0 {
                    return Self::from_parts(
                        course_id.to_string(),
                        entry.ks.clone(),
                        entry.partner_id,
                        entry.cookies.clone(),
                    );
                }
            }
        }

        let bb_router = state.client.current_cookie_value().await;
        if bb_router.is_empty() {
            return Err(
                "Not logged in to NTULearn (no BbRouter session cookie), so the Kaltura \
                 launch cannot start. Call the ntulearn_login tool (or `ntulearn-mcp \
                 setup`), then retry."
                    .to_string(),
            );
        }
        let bb_base = state.client.base_url().trim_end_matches('/').to_string();
        let bb_host = reqwest::Url::parse(&bb_base)
            .ok()
            .and_then(|u| u.host_str().map(String::from))
            .unwrap_or_default();
        let http = chain_http_client()?;

        // The embed this content item carries: placement id + embed URL.
        let item = state
            .client
            .get_content_item_with_body(course_id, content_id, None)
            .await
            .map_err(|e| format!("Kaltura launch: content item lookup failed: {e}"))?;
        let body = item.get("body").and_then(|v| v.as_str()).ok_or_else(|| {
            format!(
                "Kaltura launch: the REST record of content {content_id} in course \
                     {course_id} has no body HTML, so no Kaltura embed can be found."
            )
        })?;
        let embed = extract_embeds_for(content_id, body)
            .into_iter()
            .next()
            .ok_or_else(|| {
                format!(
                    "Kaltura launch: content {content_id} in course {course_id} contains no \
                     Kaltura video embed (no data-bbfile LTI link to a /browseandembed/ \
                     URL). It may embed a different tool or be a plain document."
                )
            })?;

        // The chain: placement -> KAF oidc-init -> Bb gateway -> the
        // authenticate page with the auto-submit form.
        let placement_url = reqwest::Url::parse(&format!(
            "{bb_base}/webapps/blackboard/execute/blti/launchPlacement\
             ?blti_placement_id={}&wrapped=true&course_id={course_id}&content_id={content_id}",
            embed.placement_id
        ))
        .map_err(|e| format!("Kaltura launch: invalid placement URL: {e}"))?;
        let mut jar = CookieJar::default();
        let form_page = get_following(&http, &mut jar, placement_url, &bb_host, &bb_router)
            .await
            .map_err(|e| format!("{e} (content {content_id}, course {course_id})"))?;
        if form_page.status != 200 {
            return Err(format!(
                "Kaltura launch: NTULearn returned HTTP {} for the launch placement page. \
                 The session may have expired — call the ntulearn_login tool and retry.",
                form_page.status
            ));
        }

        // Submit the auto-submit form; Kaltura session cookies land on KAF.
        let (action, fields) = parse_auto_submit_form(&form_page.body).ok_or_else(|| {
            "Kaltura launch: the NTULearn page did not contain an LTI auto-submit form \
             (hidden id_token input). The BbRouter session is probably expired — call \
             the ntulearn_login tool, then retry."
                .to_string()
        })?;
        let action_url = form_page
            .url
            .join(&action)
            .map_err(|e| format!("Kaltura launch: invalid form action {action:?}: {e}"))?;
        let launch_page =
            post_launch_form(&http, &mut jar, action_url, &fields, &bb_host, &bb_router).await?;
        if launch_page.status != 200 {
            return Err(format!(
                "Kaltura launch: the oauth2-launch POST returned HTTP {}. The launch \
                 failed — retry the tool call, and call ntulearn_login first if it \
                 persists.",
                launch_page.status
            ));
        }

        // The embed page mints the player KS (any embed of the course works).
        let embed_url = reqwest::Url::parse(&embed.embed_url).map_err(|e| {
            format!(
                "Kaltura launch: invalid embed URL {:?}: {e}",
                embed.embed_url
            )
        })?;
        let mut entry_page = get_following(&http, &mut jar, embed_url, &bb_host, &bb_router)
            .await
            .map_err(|e| format!("{e} (embed {})", embed.embed_url))?;
        // The embed URL may serve a KAF cookie-check interstitial that
        // JS-redirects to the media-redirect page with the flashvars. HTTP
        // redirects are already followed; follow the JS hop too (bounded).
        for _ in 0..3 {
            if entry_page.status != 200 {
                return Err(format!(
                    "Kaltura launch: KAF returned HTTP {} for the embed page. The Kaltura \
                     session cookies may be stale — retry the tool call so the launch runs \
                     again.",
                    entry_page.status
                ));
            }
            if parse_flashvars(&entry_page.body).is_some() {
                break;
            }
            let Some(js_target) = js_location_href(&entry_page.body) else {
                break;
            };
            let target = entry_page.url.join(&js_target).map_err(|e| {
                format!("Kaltura launch: invalid JS redirect target {js_target:?}: {e}")
            })?;
            entry_page = get_following(&http, &mut jar, target, &bb_host, &bb_router)
                .await
                .map_err(|e| format!("{e} (embed JS redirect)"))?;
        }
        let flashvars = parse_flashvars(&entry_page.body).ok_or_else(|| {
            "Kaltura launch: the KAF embed page did not contain a `var flashvars = {...}` \
             player configuration, so no player KS could be minted. Retry the tool call; \
             if it persists, call the ntulearn_login tool and retry."
                .to_string()
        })?;
        let ks = flashvars
            .get("ks")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .ok_or_else(|| {
                "Kaltura launch: the embed page's flashvars carry no ks (player session). \
                 Retry the tool call so the launch runs again."
                    .to_string()
            })?;
        let partner_id = flashvars
            .get("wid")
            .and_then(|v| v.as_str())
            .and_then(parse_partner_wid)
            .or_else(|| partner_id_from_page(&entry_page.body))
            .ok_or_else(|| {
                "Kaltura launch: the Kaltura partner id (wid) could not be read off the \
                 embed page, so Kaltura API URLs cannot be built. Retry the tool call."
                    .to_string()
            })?;

        let cookies = jar_cookie_map(&jar, KAF_HOST);
        let session = Self::from_parts(course_id.to_string(), ks, partner_id, cookies.clone())?;

        // Publish for the whole course (KS is entry-agnostic).
        if let Ok(mut guard) = sessions().lock() {
            guard.insert(
                course_id.to_string(),
                CachedKaltura {
                    ks: session.ks.clone(),
                    partner_id,
                    cookies,
                },
            );
        }
        Ok(session)
    }

    /// Assemble from parts (launch success or cache hit).
    fn from_parts(
        course_id: String,
        ks: String,
        partner_id: u32,
        cookies: HashMap<String, String>,
    ) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .user_agent(BROWSER_UA)
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| format!("Kaltura: reqwest client build failed: {e}"))?;
        Ok(KalturaSession {
            course_id,
            ks,
            partner_id,
            cookies,
            http,
        })
    }

    /// Drop the cached session for a course (the KS was rejected). The next
    /// [`Self::launch`] re-runs the whole chain.
    pub fn invalidate(course_id: &str) {
        if let Ok(mut guard) = sessions().lock() {
            guard.remove(course_id);
        }
    }

    /// The player KS minted by the launch chain.
    pub fn ks(&self) -> &str {
        &self.ks
    }

    /// The Kaltura partner id (117 for NTU), from the embed page's wid.
    pub fn partner_id(&self) -> u32 {
        self.partner_id
    }

    /// The course this session launched through.
    pub fn course_id(&self) -> &str {
        &self.course_id
    }

    /// The auth-rejection error: invalidates the cached session (so the
    /// next tool call re-launches) and points the caller at ntulearn_login.
    fn ks_rejected(&self, what: &str) -> String {
        Self::invalidate(&self.course_id);
        format!(
            "Kaltura rejected the session KS while fetching {what}. The cached Kaltura \
             session was dropped — retry the tool call so the LTI launch runs again, \
             and call the ntulearn_login tool first if the NTULearn session itself \
             has expired."
        )
    }

    /// GET one Kaltura URL as text. Auth-shaped failures (401/403,
    /// INVALID_KS error bodies) invalidate the session and return the
    /// re-launch error; other failures return readable detail.
    async fn fetch_text(&self, url: &str, what: &str) -> Result<String, String> {
        let resp = self
            .http
            .get(url)
            .header(reqwest::header::USER_AGENT, BROWSER_UA)
            .send()
            .await
            .map_err(|e| format!("Kaltura {what} request failed: {e}"))?;
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("Kaltura {what}: reading body failed: {e}"))?;
        if status == 401 || status == 403 {
            return Err(self.ks_rejected(what));
        }
        if let Some(error) = kaltura_api_error(&text) {
            if error.contains("INVALID_KS") {
                return Err(self.ks_rejected(what));
            }
            return Err(format!("Kaltura {what}: {error}"));
        }
        if !(200..300).contains(&status) {
            return Err(format!(
                "Kaltura {what} returned HTTP {status}: {}",
                snippet(&text)
            ));
        }
        Ok(text)
    }

    /// Caption track assets of one entry (metadata only — the signed
    /// serveWebVTT route inside [`Self::transcript`] is the only verified
    /// caption-download path; captionAsset.serve 200s empty with a player
    /// KS).
    pub async fn caption_assets(&self, entry_id: &str) -> Result<Vec<CaptionAsset>, String> {
        let xml = self
            .fetch_text(
                &caption_assets_url(&self.ks, entry_id),
                "caption asset list",
            )
            .await?;
        Ok(parse_caption_assets_xml(&xml))
    }

    /// Video flavors (renditions) of one entry, metadata for quality choice
    /// ahead of [`Self::download_video`].
    pub async fn flavors(&self, entry_id: &str) -> Result<Vec<FlavorAsset>, String> {
        let xml = self
            .fetch_text(&flavor_assets_url(&self.ks, entry_id), "flavor list")
            .await?;
        Ok(parse_flavor_assets_xml(&xml))
    }

    /// Fetch the transcript of one entry via the verified m3u8 route:
    /// playManifest master -> SUBTITLES sub-playlist -> every VTT segment.
    /// A video without captions is [`TranscriptError::NoCaptions`] (the
    /// handler layer maps it to a per-video note, not a call error).
    pub async fn transcript(&self, entry_id: &str) -> Result<Transcript, TranscriptError> {
        let what = format!("transcript manifest for entry {entry_id}");
        let manifest = self
            .fetch_text(&manifest_url(self.partner_id, entry_id, &self.ks), &what)
            .await
            .map_err(TranscriptError::Network)?;
        if !manifest.contains("#EXTM3U") {
            return Err(TranscriptError::Parse(format!(
                "the playManifest response for entry {entry_id} is not an HLS playlist: {}",
                snippet(&manifest)
            )));
        }
        let track = caption_track_from_manifest(&manifest).ok_or(TranscriptError::NoCaptions)?;
        let playlist = self
            .fetch_text(&track.uri, "caption sub-playlist")
            .await
            .map_err(TranscriptError::Network)?;
        if !playlist.contains("#EXTM3U") {
            return Err(TranscriptError::Parse(format!(
                "the caption sub-playlist for entry {entry_id} is not an HLS playlist: {}",
                snippet(&playlist)
            )));
        }
        let segments = segments_from_playlist(&playlist);
        if segments.is_empty() {
            return Err(TranscriptError::Parse(format!(
                "the caption sub-playlist for entry {entry_id} lists no VTT segments: {}",
                snippet(&playlist)
            )));
        }
        let mut vtt = String::from("WEBVTT\n\n");
        for segment in &segments {
            let url = segment_url(&track.uri, segment);
            let what = format!("caption segment {segment}");
            let body = self
                .fetch_text(&url, &what)
                .await
                .map_err(TranscriptError::Network)?;
            vtt.push_str(vtt_body_after_header(&body));
        }
        let text = vtt_to_text(&vtt);
        let words = text.split_whitespace().count();
        Ok(Transcript {
            vtt,
            text,
            words,
            segments: segments.len(),
            label: track.label,
        })
    }

    /// Stream the MP4 of one flavor to `path` (the CDN is open — no auth).
    /// Returns the number of bytes written. Streams chunk-by-chunk; a
    /// 1080p original can be hundreds of megabytes.
    pub async fn download_video(
        &self,
        entry_id: &str,
        flavor: &FlavorAsset,
        path: &Path,
    ) -> Result<u64, String> {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;
        let url = flavor_url(self.partner_id, entry_id, flavor);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    format!(
                        "Kaltura download: cannot create directory {}: {e}",
                        parent.display()
                    )
                })?;
            }
        }
        let resp = self
            .http
            .get(&url)
            .header(reqwest::header::USER_AGENT, BROWSER_UA)
            .send()
            .await
            .map_err(|e| format!("Kaltura video download request failed: {e}"))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(format!(
                "Kaltura video download from the CDN returned HTTP {status} for flavor \
                 {} of entry {entry_id}; the flavor may no longer exist — list flavors \
                 again and retry.",
                flavor.id
            ));
        }
        // The CDN sometimes resets long-lived connections mid-body
        // (live-verified: a 266 MB original died at 198 MB). It honors
        // Range, so resume from the bytes already on disk instead of
        // failing the whole download. Small retry budget, then give up
        // with a message that says how much landed on disk.
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .await
            .map_err(|e| format!("Kaltura download: cannot create {}: {e}", path.display()))?;
        let mut written: u64 = 0;
        const RESUME_ATTEMPTS: usize = 5;
        let mut attempt = 0;
        let mut stream = resp.bytes_stream();
        loop {
            let mut failed: Option<String> = None;
            loop {
                match stream.next().await {
                    Some(Ok(chunk)) => {
                        file.write_all(&chunk).await.map_err(|e| {
                            format!(
                                "Kaltura download: writing {} failed after {written} bytes: {e}",
                                path.display()
                            )
                        })?;
                        written += chunk.len() as u64;
                    }
                    Some(Err(e)) => {
                        failed = Some(e.to_string());
                        break;
                    }
                    None => break,
                }
            }
            match failed {
                None => break,
                Some(err) => {
                    file.flush().await.map_err(|e| {
                        format!("Kaltura download: flushing {} failed: {e}", path.display())
                    })?;
                    attempt += 1;
                    if attempt > RESUME_ATTEMPTS {
                        return Err(format!(
                            "Kaltura download: stream failed after {written} bytes of {} and \
                             resuming failed {RESUME_ATTEMPTS} times: {err}",
                            path.display()
                        ));
                    }
                    let resp = self
                        .http
                        .get(&url)
                        .header(reqwest::header::USER_AGENT, BROWSER_UA)
                        .header(reqwest::header::RANGE, format!("bytes={written}-"))
                        .send()
                        .await
                        .map_err(|e| {
                            format!(
                                "Kaltura download: resume request failed after {written} \
                                 bytes of {}: {e}",
                                path.display()
                            )
                        })?;
                    let status = resp.status().as_u16();
                    // 416 = the range start is past EOF: everything is on
                    // disk already (the cut was exactly at the end).
                    if status == 416 {
                        break;
                    }
                    if !(200..300).contains(&status) {
                        return Err(format!(
                            "Kaltura download: resume (Range from {written}) returned HTTP \
                             {status} for {}: {err}",
                            path.display()
                        ));
                    }
                    stream = resp.bytes_stream();
                }
            }
        }
        file.flush()
            .await
            .map_err(|e| format!("Kaltura download: flushing {} failed: {e}", path.display()))?;
        Ok(written)
    }
}

// ===========================================================================
// Tests — fixture-driven, no network (see KALTURA_SPEC.md fixtures)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/kaltura/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap_or_else(|e| panic!("missing fixture {name}: {e}"))
    }

    fn real_segments() -> Vec<String> {
        (1..=4)
            .map(|n| fixture(&format!("segment_{n}.vtt")))
            .collect()
    }

    // -- extract_embeds ------------------------------------------------------

    fn real_body_html() -> String {
        let item: Value =
            serde_json::from_str(&fixture("content_body_topic6_part1.json")).expect("fixture JSON");
        item.get("body")
            .and_then(|v| v.as_str())
            .expect("body")
            .to_string()
    }

    fn bbfile_anchor(bbfile_json: &str) -> String {
        // data-bbfile values are HTML-entity-escaped in the real bodies.
        format!(
            r#"<a href="undefined" data-bbtype="embedded-app" data-bbfile="{}">undefined</a>"#,
            bbfile_json.replace('&', "&amp;").replace('"', "&quot;")
        )
    }

    #[test]
    fn extract_embeds_real_body() {
        let embeds = extract_embeds_for("_5976141_1", &real_body_html());
        assert_eq!(embeds.len(), 1, "{embeds:?}");
        let embed = &embeds[0];
        assert_eq!(embed.content_id, "_5976141_1");
        assert_eq!(embed.entry_id, "0_88lztt5a");
        assert_eq!(embed.placement_id, "_1326_1");
        assert_eq!(
            embed.title,
            "Topic 6: Energy and Climate Change (II) Policy Instruments and Economics \
             of Climate Change Part 1"
        );
        assert!(
            embed
                .embed_url
                .starts_with("https://ntulearnv1.ntu.edu.sg/browseandembed/")
        );
        assert!(embed.embed_url.contains("entryid/0_88lztt5a/"));
        // the unstamped variant returns the same embed with an empty content_id
        let unstamped = extract_embeds(&real_body_html());
        assert_eq!(unstamped.len(), 1);
        assert_eq!(unstamped[0].content_id, "");
        assert_eq!(unstamped[0].entry_id, "0_88lztt5a");
    }

    #[test]
    fn extract_embeds_multiple_in_one_body() {
        let body = format!(
            "<div>{}{}</div><p>{}</p>",
            bbfile_anchor(
                r#"{"linkType":"resource/x-bb-blti-link","linkRefId":"_1_1","url":"https://ntulearnv1.ntu.edu.sg/browseandembed/index/media/entryid/0_aaa111/showDescription/false/","title":"First"}"#
            ),
            bbfile_anchor(
                r#"{"linkType":"resource/x-bb-blti-link","linkRefId":"_2_1","url":"https://ntulearnv1.ntu.edu.sg/browseandembed/index/media/entryid/0_bbb222/","title":"Second"}"#
            ),
            bbfile_anchor(
                r#"{"linkType":"resource/x-bb-blti-link","linkRefId":"_3_1","url":"https://ntulearnv1.ntu.edu.sg/browseandembed/index/media/entryid/0_ccc333/","title":"Third"}"#
            ),
        );
        let embeds = extract_embeds_for("_1_1", &body);
        let entries: Vec<&str> = embeds.iter().map(|e| e.entry_id.as_str()).collect();
        assert_eq!(entries, vec!["0_aaa111", "0_bbb222", "0_ccc333"]);
        assert_eq!(embeds[1].placement_id, "_2_1");
        assert_eq!(embeds[2].title, "Third");
    }

    #[test]
    fn extract_embeds_skips_malformed_and_foreign_bbfiles() {
        let body = format!(
            "<div>{}{}{}</div>",
            // malformed JSON in the bbfile
            bbfile_anchor(r#"{"linkType":"resource/x-bb-blti-link","url":"https://ntulearnv1"#),
            // well-formed LTI link to a non-Kaltura (FeedbackFruits) URL
            bbfile_anchor(
                r#"{"linkType":"resource/x-bb-blti-link","linkRefId":"_9_1","url":"https://ntu.feedbackfruits.io/abc/assignment","title":"FBF"}"#
            ),
            // a non-LTI bbfile (regular embedded file)
            bbfile_anchor(
                r#"{"linkType":"resource/x-bb-file","linkRefId":"_8_1","url":"https://ntulearnv1.ntu.edu.sg/browseandembed/index/media/entryid/0_ddd444/","title":"file"}"#
            ),
        );
        assert!(extract_embeds(&body).is_empty());
    }

    #[test]
    fn extract_embeds_bbcswebdav_body_is_empty() {
        let body = r#"<div><a href="/webapps/bb-basiclti-BBLEARN/x-bb-blti-undefined?Kq3cZcYS15=1" data-bbtype="file" data-bbfile="{&quot;linkType&quot;:&quot;resource/x-bb-file&quot;,&quot;url&quot;:&quot;/bbcswebdav/courses/CC0006/lecture.pdf&quot;}">lecture.pdf</a></div>"#;
        assert!(extract_embeds(body).is_empty());
        // a body with no anchors at all
        assert!(extract_embeds("<p>plain text only</p>").is_empty());
        assert!(extract_embeds("").is_empty());
    }

    #[test]
    fn extract_embeds_unicode_emoji_and_missing_title() {
        let body = format!(
            "{}{}",
            bbfile_anchor(
                r#"{"linkType":"resource/x-bb-blti-link","linkRefId":"_5_1","url":"https://ntulearnv1.ntu.edu.sg/browseandembed/index/media/entryid/0_eee555/","title":"🎥 Topic 3: Food Security — 全球粮食"}"#
            ),
            bbfile_anchor(
                r#"{"linkType":"resource/x-bb-blti-link","linkRefId":"_6_1","url":"https://ntulearnv1.ntu.edu.sg/browseandembed/index/media/entryid/0_fff666/"}"#
            ),
        );
        let embeds = extract_embeds(&body);
        assert_eq!(embeds.len(), 2);
        assert_eq!(embeds[0].title, "🎥 Topic 3: Food Security — 全球粮食");
        assert_eq!(embeds[1].title, "");
        assert_eq!(embeds[1].entry_id, "0_fff666");
    }

    #[test]
    fn extract_embeds_duplicate_embeds_returned_once() {
        let one = bbfile_anchor(
            r#"{"linkType":"resource/x-bb-blti-link","linkRefId":"_7_1","url":"https://ntulearnv1.ntu.edu.sg/browseandembed/index/media/entryid/0_dup0000/","title":"dup"}"#,
        );
        let body = format!("<div>{one}{one}</div>");
        let embeds = extract_embeds(&body);
        assert_eq!(embeds.len(), 1, "identical embed twice -> once: {embeds:?}");
        // same entry under a DIFFERENT placement stays two embeds
        let other = bbfile_anchor(
            r#"{"linkType":"resource/x-bb-blti-link","linkRefId":"_8_1","url":"https://ntulearnv1.ntu.edu.sg/browseandembed/index/media/entryid/0_dup0000/","title":"dup"}"#,
        );
        let body = format!("<div>{one}{other}</div>");
        assert_eq!(extract_embeds(&body).len(), 2);
    }

    #[test]
    fn entry_id_extraction_shapes() {
        let good = "https://ntulearnv1.ntu.edu.sg/browseandembed/index/media/entryid/0_88lztt5a/showDescription/false/";
        assert_eq!(entry_id_from_url(good).as_deref(), Some("0_88lztt5a"));
        // case-insensitive path marker and hash
        let upper = "https://kaf.example/ENTRYID/0_ABC123/x";
        assert_eq!(entry_id_from_url(upper).as_deref(), Some("0_ABC123"));
        // non-0_ prefixes and junk do not match
        assert_eq!(
            entry_id_from_url("https://kaf.example/entryid/1_abcdef/"),
            None
        );
        assert_eq!(entry_id_from_url("https://kaf.example/entryid/0_/"), None);
        assert_eq!(
            entry_id_from_url("https://kaf.example/media/0_abcdef/"),
            None
        );
        assert_eq!(entry_id_from_url(""), None);
    }

    // -- flashvars + partner id ---------------------------------------------

    #[test]
    fn parse_flashvars_real_entry_page() {
        let page = fixture("entry_page_flashvars.html");
        let flashvars = parse_flashvars(&page).expect("flashvars");
        let ks = flashvars.get("ks").and_then(|v| v.as_str()).expect("ks");
        assert!(!ks.is_empty());
        // the NTU player KS is the opaque v2 token
        assert!(ks.starts_with("djJ8"), "{ks}");
        // the real pages carry wid NEXT TO flashvars, not inside it
        assert!(flashvars.get("wid").is_none());
        assert_eq!(partner_id_from_page(&page), Some(117));
    }

    #[test]
    fn parse_flashvars_missing_or_ksless() {
        assert!(parse_flashvars("<html><body>no player here</body></html>").is_none());
        // flashvars without a ks still parses (the ks check is the caller's)
        let page = r#"<script>var flashvars = {"autoPlay":"false"};</script>"#;
        let flashvars = parse_flashvars(page).expect("flashvars");
        assert!(flashvars.get("ks").is_none());
        // an unparseable object (truncated) yields None, not a panic
        assert!(parse_flashvars("var flashvars = {\"ks\": ").is_none());
        // nested braces and escaped quotes inside strings survive the scan
        let nested = r#"var flashvars = {"a":{"b":"}"},"c":"x\"y"};"#;
        let flashvars = parse_flashvars(nested).expect("nested");
        assert_eq!(flashvars.pointer("/a/b"), Some(&serde_json::json!("}")));
        assert_eq!(flashvars.get("c"), Some(&serde_json::json!("x\"y")));
    }

    #[test]
    fn partner_id_from_page_variants() {
        assert_eq!(
            partner_id_from_page("var x = {'wid': '_42', 'entry_id': '0_a'};"),
            Some(42)
        );
        assert_eq!(partner_id_from_page(r#"{"wid":"_7"}"#), Some(7));
        assert_eq!(partner_id_from_page(r#"{"wid":"117"}"#), Some(117));
        // CSS `width:` / `device-width` must not match (no ':' right after wid)
        assert_eq!(
            partner_id_from_page(r#"<meta name="viewport" content="width=device-width">"#),
            None
        );
        assert_eq!(partner_id_from_page("nothing here"), None);
        // non-numeric wid values are skipped, not guessed
        assert_eq!(partner_id_from_page("{'wid': 'abc'}"), None);
    }

    // -- auto-submit form -----------------------------------------------------

    #[test]
    fn parse_auto_submit_form_real_page() {
        let (action, fields) =
            parse_auto_submit_form(&fixture("authenticate_form.html")).expect("form");
        assert_eq!(
            action,
            "https://ntulearnv1.ntu.edu.sg/hosted/index/oauth2-launch"
        );
        let id_token = fields
            .iter()
            .find(|(name, _)| name == "id_token")
            .map(|(_, value)| value.clone())
            .expect("id_token field");
        assert!(
            id_token.starts_with("eyJ"),
            "id_token should be a JWT: {id_token}"
        );
        assert!(fields.iter().any(|(name, _)| name == "state"), "{fields:?}");
    }

    #[test]
    fn parse_auto_submit_form_compact_page() {
        // The compact capture is TRUNCATED mid-attribute-value (its
        // value="... is never closed), so a spec-compliant HTML parser
        // drops the unfinished <input> at EOF and no form fields survive:
        // None, not a panic. The full authenticate_form.html fixture is
        // the authoritative positive case.
        assert!(parse_auto_submit_form(&fixture("authenticate_form_compact.html")).is_none());
    }

    #[test]
    fn parse_auto_submit_form_missing_pieces() {
        // no form at all
        assert!(parse_auto_submit_form("<html><body>Session Expired</body></html>").is_none());
        // a form without id_token is not the LTI form
        let no_token = r#"<form action="https://x/y" method="POST">
            <input type="hidden" name="state" value="s">
        </form>"#;
        assert!(parse_auto_submit_form(no_token).is_none());
        // id_token present but empty: not usable either
        let empty_token = r#"<form action="https://x/y">
            <input type="hidden" name="id_token" value="">
        </form>"#;
        assert!(parse_auto_submit_form(empty_token).is_none());
        // a form without an action cannot be submitted
        let no_action = r#"<form><input type="hidden" name="id_token" value="eyJx.y.z"></form>"#;
        assert!(parse_auto_submit_form(no_action).is_none());
    }

    // -- master manifest / caption track -------------------------------------

    #[test]
    fn caption_track_from_real_manifest() {
        let track =
            caption_track_from_manifest(&fixture("manifest_with_captions.m3u8")).expect("track");
        assert_eq!(track.label, "Edited");
        assert_eq!(track.language, "en");
        assert!(
            track
                .uri
                .starts_with("https://vodcdn.sg.kaltura.com/api_v3/index.php/")
        );
        assert!(
            track
                .uri
                .contains("service/caption_captionasset/action/serveWebVTT")
        );
        assert!(track.uri.contains("captionAssetId/0_o5nopm5y"));
    }

    #[test]
    fn caption_track_absent_when_no_subtitles_line() {
        let manifest = "#EXTM3U\n\
            #EXT-X-STREAM-INF:PROGRAM-ID=1,BANDWIDTH=508450,RESOLUTION=960x540\n\
            https://cfvod.sgp2.ovp.kaltura.com/hls/a.mp4/index.m3u8\n";
        assert!(caption_track_from_manifest(manifest).is_none());
        // an AUDIO media line is not a caption track
        let audio = "#EXTM3U\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",LANGUAGE=\"en\",URI=\"https://x/a.m3u8\"\n";
        assert!(caption_track_from_manifest(audio).is_none());
    }

    #[test]
    fn caption_track_attribute_order_and_odd_uris() {
        // URI before NAME, LANGUAGE first — order must not matter
        let reordered = "#EXTM3U\n\
            #EXT-X-MEDIA:LANGUAGE=\"en\",URI=\"https://cdn/x.m3u8?a=1&b=2\",TYPE=SUBTITLES,NAME=\"English\",DEFAULT=YES\n";
        let track = caption_track_from_manifest(reordered).expect("track");
        assert_eq!(track.label, "English");
        assert_eq!(track.language, "en");
        assert_eq!(track.uri, "https://cdn/x.m3u8?a=1&b=2");
        // commas INSIDE the quoted URI must not split the attribute list
        let commas = "#EXT-X-MEDIA:TYPE=SUBTITLES,NAME=\"A, B\",LANGUAGE=\"en\",URI=\"https://cdn/a,b/c.m3u8\"\n";
        let track = caption_track_from_manifest(commas).expect("track");
        assert_eq!(track.label, "A, B");
        assert_eq!(track.uri, "https://cdn/a,b/c.m3u8");
        // a SUBTITLES line without a usable URI keeps looking at later lines
        let two = "#EXTM3U\n\
            #EXT-X-MEDIA:TYPE=SUBTITLES,NAME=\"empty\"\n\
            #EXT-X-MEDIA:TYPE=SUBTITLES,NAME=\"real\",LANGUAGE=\"en\",URI=\"https://cdn/r.m3u8\"\n";
        let track = caption_track_from_manifest(two).expect("track");
        assert_eq!(track.label, "real");
    }

    #[test]
    fn caption_track_garbage_manifests() {
        assert!(caption_track_from_manifest("").is_none());
        assert!(caption_track_from_manifest("random text\nnot a playlist").is_none());
        assert!(caption_track_from_manifest("<html>error page</html>").is_none());
        // BOM-prefixed manifest with a track still parses
        let bom = "\u{feff}#EXTM3U\n\
            #EXT-X-MEDIA:TYPE=SUBTITLES,NAME=\"Edited\",LANGUAGE=\"en\",URI=\"https://cdn/x.m3u8\"\n";
        assert!(caption_track_from_manifest(bom).is_some());
    }

    // -- caption sub-playlist -------------------------------------------------

    #[test]
    fn segments_from_real_playlist() {
        let segments = segments_from_playlist(&fixture("caption_playlist.m3u8"));
        assert_eq!(
            segments,
            vec![
                "segmentIndex/1.vtt",
                "segmentIndex/2.vtt",
                "segmentIndex/3.vtt",
                "segmentIndex/4.vtt"
            ]
        );
    }

    #[test]
    fn segments_from_playlist_edge_shapes() {
        let single = "#EXTM3U\n#EXTINF:42.0,\nsegmentIndex/1.vtt\n#EXT-X-ENDLIST\n";
        assert_eq!(segments_from_playlist(single), vec!["segmentIndex/1.vtt"]);
        // ENDLIST only: zero segments, not an error
        let empty = "#EXTM3U\n#EXT-X-TARGETDURATION:300\n#EXT-X-ENDLIST\n";
        assert!(segments_from_playlist(empty).is_empty());
        // interleaved comments and stray # lines are skipped
        let comments = "#EXTM3U\n# a note\n#EXTINF:1.0,\nsegmentIndex/1.vtt\n##\n#EXTINF:1.0,\nsegmentIndex/2.vtt\n";
        assert_eq!(
            segments_from_playlist(comments),
            vec!["segmentIndex/1.vtt", "segmentIndex/2.vtt"]
        );
        assert!(segments_from_playlist("").is_empty());
        // BOM tolerated
        assert_eq!(
            segments_from_playlist("\u{feff}#EXTM3U\nsegmentIndex/9.vtt\n"),
            vec!["segmentIndex/9.vtt"]
        );
    }

    // -- vtt_to_text ------------------------------------------------------------

    #[test]
    fn vtt_to_text_real_transcript_matches_reference_word_count() {
        let joined = real_segments().join("\n");
        let text = vtt_to_text(&joined);
        // the exact opening sentence of the reference capture
        assert!(
            text.starts_with(
                "A very warm welcome to Part 2 of this lecture on energy and climate change."
            ),
            "opening: {}",
            &text[..text.len().min(120)]
        );
        // the reference word count (reference_transcript_counts.json) is exact
        let counts: Value =
            serde_json::from_str(&fixture("reference_transcript_counts.json")).expect("counts");
        let expected = counts
            .as_array()
            .expect("counts array")
            .iter()
            .find(|e| e.get("entry").and_then(|v| v.as_str()) == Some("0_88lztt5a"))
            .and_then(|e| e.get("words").and_then(|v| v.as_u64()))
            .expect("reference words");
        assert_eq!(text.split_whitespace().count() as u64, expected);
        // the closing sentence survived the segment boundaries too
        assert!(
            text.ends_with("the economics of climate change."),
            "ending: {}",
            &text[text.len().saturating_sub(80)..]
        );
    }

    #[test]
    fn vtt_to_text_strips_headers_notes_tags_and_dups() {
        let vtt = "WEBVTT\n\n\
            NOTE this is a comment block\nspanning lines\n\n\
            1\n00:00:01.000 --> 00:00:02.000\n<c>Hello</c> world\n\n\
            00:00:02.000 --> 00:00:03.000 align:start position:0%\nHello world\n\n\
            00:00:03.000 --> 00:00:04.000\n<v Speaker>Multi-line\ncue continues</v>\n\n";
        assert_eq!(vtt_to_text(vtt), "Hello world Multi-line cue continues");
        // inline timestamps (word timing) are stripped
        let timed =
            "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n<00:00:01.000>word <00:00:01.500>timed\n";
        assert_eq!(vtt_to_text(timed), "word timed");
        // empty and header-only input
        assert_eq!(vtt_to_text(""), "");
        assert_eq!(vtt_to_text("WEBVTT\n\n"), "");
        // entities decode
        let ent = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nA &amp; B &lt;tags&gt; &quot;q&quot;\n";
        assert_eq!(vtt_to_text(ent), "A & B <tags> \"q\"");
    }

    #[test]
    fn vtt_to_text_collapses_segment_boundary_duplicates() {
        // real shape: the last cue of segment N repeats as the first cue of
        // segment N+1 (both segments carry the same boundary timestamp)
        let seg_a = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nhello\n\n00:00:02.000 --> 00:00:03.000\nworld\n\n";
        let seg_b = "WEBVTT\n\n00:00:02.000 --> 00:00:03.000\nworld\n\n00:00:03.000 --> 00:00:04.000\nagain\n\n";
        assert_eq!(vtt_to_text(&format!("{seg_a}{seg_b}")), "hello world again");
        // duplicates are only collapsed when CONSECUTIVE
        let vtt = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\na\n\n00:00:02.000 --> 00:00:03.000\nb\n\n00:00:03.000 --> 00:00:04.000\na\n";
        assert_eq!(vtt_to_text(vtt), "a b a");
    }

    #[test]
    fn vtt_body_after_header_strips_only_the_header() {
        let seg = "WEBVTT - some header\nRegion: x\n\n00:00:01.000 --> 00:00:02.000\nhi\n\n";
        assert_eq!(
            vtt_body_after_header(seg),
            "00:00:01.000 --> 00:00:02.000\nhi\n\n"
        );
        // a body that is already cue blocks passes through
        assert_eq!(
            vtt_body_after_header("00:00:01.000 --> 00:00:02.000\nhi\n"),
            "00:00:01.000 --> 00:00:02.000\nhi\n"
        );
        // header with no blank line after it: no cue blocks to keep
        assert_eq!(vtt_body_after_header("WEBVTT\n"), "");
    }

    // -- XML asset lists --------------------------------------------------------

    #[test]
    fn parse_caption_assets_real_xml() {
        let assets = parse_caption_assets_xml(&fixture("caption_assets_one.xml"));
        assert_eq!(assets.len(), 1);
        let asset = &assets[0];
        assert_eq!(asset.id, "0_o5nopm5y");
        assert_eq!(asset.language, "English");
        assert_eq!(asset.label, "Edited");
        // <isDefault></isDefault> (empty) is false
        assert!(!asset.is_default);
    }

    #[test]
    fn parse_caption_assets_empty_error_and_sparse() {
        // empty list
        assert!(parse_caption_assets_xml(
            "<?xml version=\"1.0\"?><xml><result><objects></objects><totalCount>0</totalCount></result></xml>"
        )
        .is_empty());
        // Kaltura error XML (INVALID_KS): empty vec, never a panic
        let error_xml = "<?xml version=\"1.0\"?><xml><error><code>INVALID_KS</code>\
            <message>Invalid KS. Error 1041</message></error></xml>";
        assert!(parse_caption_assets_xml(error_xml).is_empty());
        assert_eq!(
            kaltura_api_error(error_xml).as_deref(),
            Some("Kaltura API error INVALID_KS: Invalid KS. Error 1041")
        );
        // missing optional fields default; missing id drops the item
        let sparse = "<xml><result><objects><item><id>0_x</id></item>\
            <item><label>no id</label></item></objects></result></xml>";
        let assets = parse_caption_assets_xml(sparse);
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].id, "0_x");
        assert_eq!(assets[0].language, "");
        assert!(!assets[0].is_default);
        // isDefault=1 parses true, whitespace trims
        let flagged = "<xml><result><objects><item><id>0_y</id><isDefault>1</isDefault>\
            <language> French </language></item></objects></result></xml>";
        let assets = parse_caption_assets_xml(flagged);
        assert!(assets[0].is_default);
        assert_eq!(assets[0].language, "French");
    }

    #[test]
    fn parse_flavor_assets_real_xmls() {
        let two = parse_flavor_assets_xml(&fixture("flavor_assets_two.xml"));
        assert_eq!(two.len(), 2);
        let original = &two[0];
        assert_eq!(original.id, "0_mzsjnuj6");
        assert!(original.is_original);
        assert_eq!(original.width, 1920);
        assert_eq!(original.height, 1080);
        assert_eq!(original.size_bytes, 1_696_369_677);
        assert_eq!(original.version, 2);
        assert_eq!(original.file_ext, "mp4");
        let mobile = &two[1];
        assert!(!mobile.is_original);
        assert_eq!(mobile.size_bytes, 123_694_567);
        assert_eq!(mobile.width, 960);

        let three = parse_flavor_assets_xml(&fixture("flavor_assets_three.xml"));
        assert_eq!(three.len(), 3);
        // sizes include the 191MB non-original 1080p re-encode
        assert_eq!(three[2].size_bytes, 191_748_125);
        assert_eq!(three[2].version, 2);
        assert!(three.iter().any(|f| f.is_original));
    }

    #[test]
    fn flavor_sizes_fall_back_to_legacy_kb_when_sizeinbytes_is_zero() {
        // the real 0_88lztt5a list: original reports sizeInBytes=0, legacy
        // <size> 260436 (KB) -> 260436*1024; the 540p keeps its modern
        // sizeInBytes even though the CDN serves less (stale field).
        let flavors = parse_flavor_assets_xml(&fixture("flavor_assets_three.xml"));
        let orig = flavors.iter().find(|f| f.is_original).expect("original");
        assert_eq!(orig.size_bytes, 260_436 * 1024);
        let small = flavors
            .iter()
            .filter(|f| !f.is_original)
            .min_by_key(|f| f.size_bytes);
        assert_eq!(small.map(|f| f.id.as_str()), Some("0_z24etxpq"));
        assert_eq!(small.map(|f| f.size_bytes), Some(150_718_384));
    }

    #[test]
    fn js_location_href_from_real_cookie_check_page() {
        let skeleton = fixture("embed_cookie_check.html");
        let target = js_location_href(&skeleton).expect("js redirect target");
        assert!(
            target.starts_with(
                "https://ntulearnv1.ntu.edu.sg/browseandembed/index/media-redirect/entryid/0_88lztt5a/"
            ),
            "{target}"
        );
        // no flashvars on the interstitial itself, and no JS hop on the real page
        assert!(parse_flashvars(&skeleton).is_none());
        let real = fixture("entry_page_flashvars.html");
        assert!(js_location_href(&real).is_none());
        assert!(js_location_href("<html>nothing</html>").is_none());
    }

    #[test]
    fn parse_flavor_assets_empty_error_and_defaults() {
        assert!(
            parse_flavor_assets_xml(
                "<xml><result><objects/><totalCount>0</totalCount></result></xml>"
            )
            .is_empty()
        );
        let error_xml = "<xml><error><code>INVALID_KS</code><message>x</message></error></xml>";
        assert!(parse_flavor_assets_xml(error_xml).is_empty());
        // an item with only an id: every optional field defaults
        let sparse = "<xml><result><objects><item><id>0_z</id></item></objects></result></xml>";
        let flavors = parse_flavor_assets_xml(sparse);
        assert_eq!(flavors.len(), 1);
        let f = &flavors[0];
        assert_eq!(
            (
                f.width,
                f.height,
                f.size_bytes,
                f.version,
                f.file_ext.as_str()
            ),
            (0, 0, 0, 0, "")
        );
        assert!(!f.is_original);
        // garbage never panics
        assert!(parse_flavor_assets_xml("not xml at all <<").is_empty());
        // a normal list has no <error>
        assert!(kaltura_api_error(&fixture("flavor_assets_two.xml")).is_none());
    }

    // -- URL builders -------------------------------------------------------------

    #[test]
    fn manifest_url_shape() {
        let ks = "djJ8MTE3fA+b/C=d_e-f";
        let url = manifest_url(117, "0_88lztt5a", ks);
        assert!(url.starts_with("https://api.sgp2.ovp.kaltura.com/p/117/sp/11700/playManifest/"));
        assert!(url.contains("entryId/0_88lztt5a"));
        assert!(url.contains("/format/applehttp/protocol/https/a.m3u8?"));
        // the referrer is the base64 of the KAF origin, '=' percent-encoded
        assert!(url.contains("referrer=aHR0cHM6Ly9udHVsZWFybnYxLm50dS5lZHUuc2c%3D&"));
        // the KS is percent-encoded (reserved base64 chars escaped)
        assert!(url.contains("ks=djJ8MTE3fA%2Bb%2FC%3Dd%5Fe%2Df"));
        assert!(
            !url.contains("ks=djJ8MTE3fA+b"),
            "raw ks must not appear: {url}"
        );
    }

    #[test]
    fn segment_and_asset_urls() {
        assert_eq!(
            segment_url(
                "https://vodcdn.sg.kaltura.com/api_v3/index.php/a/b/captionAssetId/0_x/segmentDuration/300/ks/K/version/12/a.m3u8",
                "segmentIndex/3.vtt"
            ),
            "https://vodcdn.sg.kaltura.com/api_v3/index.php/a/b/captionAssetId/0_x/segmentDuration/300/ks/K/version/12/segmentIndex/3.vtt"
        );
        // a segment path with no directory resolves to itself
        assert_eq!(
            segment_url("a.m3u8", "segmentIndex/1.vtt"),
            "segmentIndex/1.vtt"
        );

        let cap = caption_assets_url("ks+/=", "0_88lztt5a");
        assert!(cap.starts_with("https://api.sg.kaltura.com/api_v3?"));
        assert!(cap.contains("service=caption_captionAsset&action=list"));
        assert!(cap.contains("filter:entryIdEqual=0_88lztt5a"));
        assert!(cap.contains("ks=ks%2B%2F%3D"));

        let flav = flavor_assets_url("plainks", "0_td7lgutt");
        assert!(
            flav.contains(
                "service=flavorAsset&action=list&filter:entryIdEqual=0_td7lgutt&ks=plainks"
            )
        );
    }

    #[test]
    fn flavor_url_shape() {
        let flavor = FlavorAsset {
            id: "0_z24etxpq".to_string(),
            width: 960,
            height: 540,
            size_bytes: 150_718_384,
            is_original: false,
            version: 12,
            file_ext: "mp4".to_string(),
        };
        let url = flavor_url(117, "0_88lztt5a", &flavor);
        assert_eq!(
            url,
            "https://cfvod.sgp2.ovp.kaltura.com/p/117/sp/11700/serveFlavor/entryId/0_88lztt5a/v/12/flavorId/0_z24etxpq/name/a.mp4"
        );
        // an empty file extension falls back to mp4 (a bare "a." would 404)
        let bare = FlavorAsset {
            file_ext: String::new(),
            ..flavor
        };
        assert!(flavor_url(117, "0_88lztt5a", &bare).ends_with("/name/a.mp4"));
    }

    // -- TranscriptError messages ---------------------------------------------------

    #[test]
    fn transcript_error_messages_are_llm_readable() {
        let no_caps = TranscriptError::NoCaptions.message();
        assert!(no_caps.contains("no caption track"), "{no_caps}");
        assert!(
            !no_caps.contains("ntulearn_login"),
            "not auth-shaped: {no_caps}"
        );

        let net =
            TranscriptError::Network("Kaltura transcript manifest request failed".into()).message();
        assert!(net.starts_with("Kaltura transcript could not be fetched"));
        assert!(!net.contains("ntulearn_login"), "{net}");

        let auth =
            TranscriptError::Network("Kaltura API error INVALID_KS: Invalid KS. Error 1041".into())
                .message();
        assert!(auth.contains("ntulearn_login"), "{auth}");
        assert!(auth.contains("retry the tool call"), "{auth}");
        let unauthorized = TranscriptError::Network("x returned HTTP 401: y".into()).message();
        assert!(unauthorized.contains("ntulearn_login"), "{unauthorized}");

        let parse =
            TranscriptError::Parse("the playManifest response is not an HLS playlist".into())
                .message();
        assert!(parse.contains("malformed"), "{parse}");
    }

    // -- session cache (no network) ---------------------------------------------------

    fn offline_session() -> KalturaSession {
        KalturaSession::from_parts(
            "_2707070_1".to_string(),
            "djJ8MTE3fsample".to_string(),
            117,
            HashMap::from([("KMS_SESSION".to_string(), "abc".to_string())]),
        )
        .expect("session")
    }

    #[test]
    fn offline_session_accessors() {
        let session = offline_session();
        assert_eq!(session.ks(), "djJ8MTE3fsample");
        assert_eq!(session.partner_id(), 117);
        assert_eq!(session.course_id(), "_2707070_1");
    }

    #[test]
    fn session_cache_publish_and_invalidate() {
        let course = format!("cache-test-{}", std::process::id());
        {
            let mut guard = sessions().lock().expect("lock");
            guard.insert(
                course.clone(),
                CachedKaltura {
                    ks: "djJ8MTE3fcached".to_string(),
                    partner_id: 117,
                    cookies: HashMap::new(),
                },
            );
        }
        {
            let guard = sessions().lock().expect("lock");
            let entry = guard.get(&course).expect("cached entry");
            assert_eq!(entry.ks, "djJ8MTE3fcached");
        }
        // invalidate drops exactly that course; unknown ids are a no-op
        KalturaSession::invalidate(&course);
        KalturaSession::invalidate("never-cached-course");
        let guard = sessions().lock().expect("lock");
        assert!(guard.get(&course).is_none());
    }

    // -- form encoding -----------------------------------------------------------------

    #[test]
    fn form_urlencode_escapes_values() {
        let body = form_urlencode(&[
            ("id_token".to_string(), "a.b+c~d".to_string()),
            ("state".to_string(), "x y".to_string()),
        ]);
        assert_eq!(body, "id%5Ftoken=a%2Eb%2Bc%7Ed&state=x%20y");
    }

    // -- cookie jar --------------------------------------------------------------------

    #[test]
    fn cookie_jar_keeps_kaf_cookies_off_ntulearn() {
        let mut jar = CookieJar::default();
        // host-only KAF session cookie
        jar.ingest("ntulearnv1.ntu.edu.sg", &["KMS_SESSION=abc".to_string()]);
        jar.ingest(
            "ntulearn.ntu.edu.sg",
            &["JSESSIONID=j1; Path=/".to_string()],
        );
        // KAF sees its host-only cookie
        let kaf = jar.pairs_for("ntulearnv1.ntu.edu.sg");
        assert!(kaf.iter().any(|p| p == "KMS_SESSION=abc"), "{kaf:?}");
        // NTULearn sees only its own cookie, never the KAF one
        let bb = jar.pairs_for("ntulearn.ntu.edu.sg");
        assert!(bb.iter().any(|p| p == "JSESSIONID=j1"), "{bb:?}");
        assert!(!bb.iter().any(|p| p.contains("KMS_SESSION")), "{bb:?}");
        // the BbRouter header is only attached on the NTULearn origin
        let on_bb = cookie_header(
            &jar,
            "ntulearn.ntu.edu.sg",
            "ntulearn.ntu.edu.sg",
            "expires:1,id:x",
        );
        assert_eq!(
            on_bb.as_deref(),
            Some("JSESSIONID=j1; BbRouter=expires:1,id:x")
        );
        let on_kaf = cookie_header(
            &jar,
            "ntulearnv1.ntu.edu.sg",
            "ntulearn.ntu.edu.sg",
            "expires:1,id:x",
        );
        assert_eq!(on_kaf.as_deref(), Some("KMS_SESSION=abc"));
        // third-party origins get nothing at all
        assert_eq!(
            cookie_header(
                &jar,
                "developer.blackboard.com",
                "ntulearn.ntu.edu.sg",
                "e:1"
            ),
            None
        );
        // the KAF cookie map for the session
        let map = jar_cookie_map(&jar, KAF_HOST);
        assert_eq!(map.get("KMS_SESSION").map(String::as_str), Some("abc"));
        assert!(!map.contains_key("JSESSIONID"));
        // a Domain=.ntu.edu.sg cookie legitimately spans BOTH ntu.edu.sg
        // subdomains (RFC 6265) — browser semantics, kept on purpose
        jar.ingest(
            "ntulearnv1.ntu.edu.sg",
            &["SHARED=1; Domain=.ntu.edu.sg; Path=/; Secure".to_string()],
        );
        for host in ["ntulearnv1.ntu.edu.sg", "ntulearn.ntu.edu.sg"] {
            let pairs = jar.pairs_for(host);
            assert!(pairs.iter().any(|p| p == "SHARED=1"), "{host}: {pairs:?}");
        }
        // but never a foreign domain
        assert!(
            !jar.pairs_for("kaltura.com")
                .iter()
                .any(|p| p.contains("SHARED"))
        );
    }
}
