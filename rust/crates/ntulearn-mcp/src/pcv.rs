//! Peerceptiv (PCV) peer-evaluation integration.
//!
//! Peerceptiv is embedded in NTULearn as an LTI 1.3 tool: there is no direct
//! login, the only way in is the BbRouter-authenticated launch chain (see
//! PCV_SPEC.md §1). This module owns that chain plus the thin typed
//! read/write surface over the JSON API the Peerceptiv SPA itself uses
//! (PCV_SPEC.md §2–§3). It mirrors fbf.rs in structure and style; the
//! differences that matter:
//!
//! * The entry executor is `launchPlacement` (not `launchLink`), and the REST
//!   record of the content item does NOT carry the placement id — it must be
//!   scraped from the classic listContent.jsp page of the PARENT content id.
//! * The landing SPA runs on `asia.app.peerceptiv.com` while the API lives on
//!   `sg.api.peerceptiv.com`. The API ids come from the POST's 302 Location
//!   (`.../course/{pcvCourseId}/assignment/{assignmentId}/dashboard`).
//! * The API authenticates with cookies (`oidc_token` + `access_token`, both
//!   JWTs, set on the peerceptiv domains by the launch POST) — no Bearer
//!   header, no refresh token endpoint.
//! * login_hint tokens in the oidc/authorize URL are SINGLE-USE: a cached
//!   session is reused as-is, but a fresh launch ALWAYS restarts from the
//!   placement URL. No intermediate chain URL is ever persisted or replayed.
//!
//! Design notes shared with fbf.rs:
//! * The workspace reqwest does NOT enable the `cookies` feature, so the LTI
//!   chain runs on a small hand-rolled cookie jar with manual redirect
//!   following. That also lets us observe every Set-Cookie hop.
//! * The BbRouter header is only attached to the NTULearn origin. It is an
//!   NTULearn session credential; the redirect chain passes through
//!   peerceptiv.com and developer.blackboard.com, and leaking a session
//!   cookie to third parties is exactly what a browser would not do.
//! * "Refresh" is a full re-launch: the cheapest correct refresh is redoing
//!   the chain with the (auto-refreshed) BbRouter cookie, per the spec.
//! * JSON is parsed with `serde_json::Value` + manual extraction — the
//!   Peerceptiv payloads are wide and only a handful of fields matter.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::cookie;
use crate::AppState;

/// A desktop Chrome UA for the LTI chain. The chain impersonates the browser
/// the user would launch the tool from; some hops behave differently for
/// non-browser user agents. API calls after landing also carry it (harmless).
const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
                          AppleWebKit/537.36 (KHTML, like Gecko) \
                          Chrome/130.0.0.0 Safari/537.36";
/// Cap on manually-followed redirects. The observed chain is 4 GET hops + 1
/// POST; anything beyond this is a redirect loop, not a launch.
const MAX_REDIRECTS: usize = 12;
/// Treat a session as expired this many seconds early, so a cached session is
/// not handed out seconds before its JWT cookies die.
const TOKEN_EXPIRY_MARGIN_SECS: i64 = 60;
/// Cache file name inside the config dir (next to the cookie file).
const PCV_SESSION_FILE: &str = "pcv.json";
/// The Peerceptiv API origin (the SPA lands on asia.app, but every API call
/// the SPA makes goes here — PCV_SPEC.md §1/§2).
const API_BASE: &str = "https://sg.api.peerceptiv.com";
/// Host the API cookies are collected for (Domain=.sg.api.peerceptiv.com or
/// host-only — the jar's pairs_for handles both).
const API_HOST: &str = "sg.api.peerceptiv.com";
/// contentHandler.id prefix that marks a content item as an LTI *placement*
/// (FeedbackFruits items are `resource/x-bb-blti-link`, so this is a clean
/// filter between the two peer-eval tools).
const PLACEMENT_HANDLER_PREFIX: &str = "resource/x-bb-bltiplacement";

// ===========================================================================
// JWT (decode only — never verify; we minted no keys, we just read claims)
// ===========================================================================

/// Decode the payload claims of a JWT. Base64url, no signature verification:
/// the token comes from Peerceptiv over TLS and we only read `sub`/`exp`
/// from it, we do not make auth decisions on its integrity.
fn jwt_claims(token: &str) -> Result<Value, String> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| "Peerceptiv token is not a JWT (no '.' separators)".to_string())?;
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|e| format!("Peerceptiv JWT payload is not valid base64url: {e}"))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| format!("Peerceptiv JWT payload is not valid JSON: {e}"))
}

fn jwt_sub(token: &str) -> Option<String> {
    jwt_claims(token)
        .ok()?
        .get("sub")?
        .as_str()
        .map(|s| s.to_string())
}

fn jwt_exp(token: &str) -> Option<i64> {
    jwt_claims(token).ok()?.get("exp")?.as_i64()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ===========================================================================
// Cookie jar (reqwest here has no `cookies` feature; see module docs)
// ===========================================================================

/// Minimal cookie jar: domain -> (name -> (value, host_only)). Enough for the
/// LTI chain, where cookies are set on the peerceptiv domains and re-sent on
/// the next hop. No expiry handling — the chain lives seconds, and stale
/// cookies are overwritten by fresh Set-Cookie headers.
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
            let Some((name, value)) = first.split_once('=') else { continue };
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

/// The API cookie map for `API_HOST` out of a launch jar: every cookie the
/// chain collected that applies to the API origin (`oidc_token`,
/// `access_token`, and anything else Peerceptiv sets there).
fn jar_cookie_map(jar: &CookieJar) -> HashMap<String, String> {
    jar.pairs_for(API_HOST)
        .into_iter()
        .filter_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            Some((name.to_string(), value.to_string()))
        })
        .collect()
}

// ===========================================================================
// Session cache ({config_dir}/ntulearn-mcp/pcv.json)
// ===========================================================================

/// One cached launch, keyed by "{course_id}/{content_id}" in
/// [`CachedPcvSessions`]. The cookie values are the API session; the expiry
/// is a guess — the earliest `exp` claim among the JWT cookies.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct CachedPcvEntry {
    #[serde(default)]
    pcv_course_id: String,
    #[serde(default)]
    assignment_id: String,
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    cookies: HashMap<String, String>,
    #[serde(default)]
    expires_at: Option<i64>,
}

/// Persisted launch state, mirroring fbf.json: while the cookie JWTs live,
/// `launch()` for a known (course_id, content_id) pair is zero network.
/// Nothing from the middle of the chain is ever stored — login_hint tokens
/// are single-use, so a fresh launch must always restart from the placement
/// URL, never a cached authorize URL.
#[derive(Debug, Default, Serialize, Deserialize)]
struct CachedPcvSessions {
    #[serde(default)]
    sessions: HashMap<String, CachedPcvEntry>,
}

/// Same config dir as the cookie file (cookie.rs::cookie_file_path) — one
/// place for everything this server persists about the NTULearn session.
fn pcv_session_file() -> PathBuf {
    cookie::cookie_file_path()
        .parent()
        .map(|p| p.join(PCV_SESSION_FILE))
        .unwrap_or_else(|| PathBuf::from(PCV_SESSION_FILE))
}

/// Load the cache; a missing or corrupt file is an empty session, not an
/// error — the launch chain will simply rewrite it.
fn load_cached(path: &Path) -> CachedPcvSessions {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist the cache (best-effort, like cookie.rs::write_cookie).
fn save_cached(path: &Path, sessions: &CachedPcvSessions) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(sessions) {
        let _ = std::fs::write(path, text);
    }
}

// ===========================================================================
// HTML / URL parsing (scraper)
// ===========================================================================

/// Extract the LTI auto-submit form: (action, id_token, state). Blackboard's
/// launchPlacement page contains exactly one form with hidden `id_token` and
/// `state` inputs posting to the Peerceptiv launch endpoint; we locate it by
/// its inputs rather than by name/id so a renamed form still works.
fn parse_launch_form(html: &str) -> Result<(String, String, String), String> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let form_sel =
        Selector::parse("form").map_err(|e| format!("scraper form selector error: {e:?}"))?;
    let input_sel =
        Selector::parse("input").map_err(|e| format!("scraper input selector error: {e:?}"))?;
    for form in doc.select(&form_sel) {
        let mut id_token: Option<String> = None;
        let mut state: Option<String> = None;
        for input in form.select(&input_sel) {
            let name = input.value().attr("name").unwrap_or("");
            let value = input.value().attr("value").unwrap_or("");
            match name {
                "id_token" => id_token = Some(value.to_string()),
                "state" => state = Some(value.to_string()),
                _ => {}
            }
        }
        if let (Some(action), Some(token), Some(st)) =
            (form.value().attr("action"), id_token, state)
        {
            if !action.is_empty() && !token.is_empty() && !st.is_empty() {
                return Ok((action.to_string(), token, st));
            }
        }
    }
    Err(
        "Peerceptiv launch: the NTULearn page did not contain an LTI auto-submit form \
         (hidden id_token + state inputs). The BbRouter session is probably expired — \
         call the ntulearn_login tool, then retry."
            .to_string(),
    )
}

/// The launchPlacement anchor href for `content_id` out of a classic
/// listContent.jsp page: `/webapps/blackboard/execute/blti/launchPlacement?
/// blti_placement_id=_N_1&content_id=_X_1&course_id=_Y_1`. The REST record of
/// the item does not carry the placement id, so this page is the only source.
/// The anchor must reference THIS content item exactly (the parent page can
/// list placements for sibling items too).
fn placement_href_from_page(html: &str, content_id: &str) -> Option<String> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let sel = Selector::parse("a[href]").ok()?;
    let needle = format!("content_id={content_id}");
    doc.select(&sel)
        .filter_map(|a| a.value().attr("href"))
        .find(|href| {
            href.contains("/webapps/blackboard/execute/blti/launchPlacement")
                // exact query-parameter match, not a prefix of another id
                && href.split(['?', '&']).any(|param| param == needle)
        })
        .map(|href| href.to_string())
}

/// Does a REST content record describe an LTI placement (a Peerceptiv-style
/// tool entry) rather than a document, folder, or FeedbackFruits link?
fn is_blti_placement_content(item: &Value) -> bool {
    item.pointer("/contentHandler/id")
        .and_then(|v| v.as_str())
        .is_some_and(|id| id.starts_with(PLACEMENT_HANDLER_PREFIX))
}

/// (pcvCourseId, assignmentId) out of the POST's 302 Location
/// `.../course/{pcvCourseId}/assignment/{assignmentId}/dashboard` — these
/// path segments ARE the API ids (PCV_SPEC.md §1 step 6).
fn ids_from_dashboard_url(url: &reqwest::Url) -> Option<(String, String)> {
    let segments: Vec<&str> = url.path_segments()?.collect();
    let mut course: Option<String> = None;
    let mut assignment: Option<String> = None;
    for w in segments.windows(2) {
        if w[0] == "course" {
            course = Some(w[1].to_string());
        }
        if w[0] == "assignment" {
            assignment = Some(w[1].to_string());
        }
    }
    match (course, assignment) {
        (Some(c), Some(a)) if !c.is_empty() && !a.is_empty() => Some((c, a)),
        _ => None,
    }
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
fn launch_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(BROWSER_UA)
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("Peerceptiv launch: reqwest client build failed: {e}"))
}

/// GET with browser-ish headers, following redirects manually and feeding
/// every hop's Set-Cookie into the jar. The chain is short and linear; a loop
/// beyond MAX_REDIRECTS is reported instead of spun forever.
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
            .map_err(|e| format!("Peerceptiv launch: GET {url} failed: {e}"))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        jar.ingest(&host, &set_cookie_values(&headers));
        if (300..400).contains(&status) {
            let location = headers
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    format!("Peerceptiv launch: HTTP {status} from {url} has no Location header")
                })?;
            url = url.join(location).map_err(|e| {
                format!(
                    "Peerceptiv launch: cannot resolve redirect target {location:?} \
                     against {url}: {e}"
                )
            })?;
            continue;
        }
        let body = resp
            .text()
            .await
            .map_err(|e| format!("Peerceptiv launch: reading body of {url} failed: {e}"))?;
        return Ok(Page { url, status, body });
    }
    Err(format!(
        "Peerceptiv launch: more than {MAX_REDIRECTS} redirects starting at {start}"
    ))
}

/// POST the auto-submit form fields (id_token + state) to the action URL and
/// return the 302 Location: `.../course/{id}/assignment/{id}/dashboard`.
/// That Location is where the API ids and the session cookies both come
/// from, so the walk STOPS at the first redirect that matches the dashboard
/// shape — the dashboard HTML itself is never needed (the API lives on
/// sg.api, not asia.app). Only the first request is the form POST; any
/// further hops before the dashboard match are followed as GETs (browser
/// semantics for 301/302/303 after a POST). Origin/Referer describe the page
/// the form lives on: the NTULearn launchPlacement page.
async fn post_launch_form(
    http: &reqwest::Client,
    jar: &mut CookieJar,
    action: reqwest::Url,
    fields: &[(String, String)],
    referer: &reqwest::Url,
    bb_host: &str,
    bb_router: &str,
) -> Result<reqwest::Url, String> {
    let body = form_urlencode(fields);
    let origin = match (referer.scheme(), referer.host_str()) {
        (scheme, Some(h)) => format!("{scheme}://{h}"),
        _ => String::new(),
    };
    let mut url = action.clone();
    let mut first_post = true;
    for _ in 0..=MAX_REDIRECTS {
        let host = url.host_str().unwrap_or_default().to_string();
        let mut req = if first_post {
            let mut req = http
                .post(url.clone())
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
                .body(body.clone());
            if !origin.is_empty() {
                req = req
                    .header(reqwest::header::ORIGIN, origin.as_str())
                    .header(reqwest::header::REFERER, referer.as_str());
            }
            req
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
            .map_err(|e| format!("Peerceptiv launch: POST to {url} failed: {e}"))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        jar.ingest(&host, &set_cookie_values(&headers));
        if (300..400).contains(&status) {
            let location = headers
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    format!("Peerceptiv launch: HTTP {status} from {url} has no Location header")
                })?;
            let next = url.join(location).map_err(|e| {
                format!(
                    "Peerceptiv launch: cannot resolve redirect target {location:?} \
                     against {url}: {e}"
                )
            })?;
            if ids_from_dashboard_url(&next).is_some() {
                return Ok(next);
            }
            url = next;
            continue;
        }
        let text = resp
            .text()
            .await
            .map_err(|e| format!("Peerceptiv launch: reading body of {url} failed: {e}"))?;
        if ids_from_dashboard_url(&url).is_some() {
            return Ok(url);
        }
        return Err(format!(
            "Peerceptiv launch: the launch POST returned HTTP {status} without a \
             course/assignment/dashboard redirect: {}",
            snippet(&text)
        ));
    }
    Err(format!(
        "Peerceptiv launch: more than {MAX_REDIRECTS} redirects after POST to {action}"
    ))
}

/// application/x-www-form-urlencoded body for the launch fields. Encoded by
/// hand (percent-encoding of everything non-alphanumeric) so the JWT/state
/// values need no escaping assumptions; a server decodes %XX identically.
fn form_urlencode(fields: &[(String, String)]) -> String {
    use percent_encoding::utf8_percent_encode;
    use percent_encoding::NON_ALPHANUMERIC;
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
    text.chars().take(300).collect::<String>().replace('\n', " ")
}

// ===========================================================================
// JSON extraction (serde_json::Value + manual pulls; see module docs)
// ===========================================================================

fn str_field(v: &Value, name: &str) -> Option<String> {
    v.get(name).and_then(|x| x.as_str()).map(|s| s.to_string())
}

/// Caller identity from `GET /me`.
fn parse_me(v: &Value) -> Result<PcvUser, String> {
    Ok(PcvUser {
        user_id: str_field(v, "userId")
            .ok_or_else(|| "Peerceptiv /me response has no userId".to_string())?,
        first_name: str_field(v, "firstName").unwrap_or_default(),
        last_name: str_field(v, "lastName").unwrap_or_default(),
        email: str_field(v, "email").unwrap_or_default(),
        role: str_field(v, "role").unwrap_or_default(),
    })
}

/// PeerEvaluations from `GET /assignment/{aid}/evals/user/{uid}` (given) and
/// its `/received` twin — the shapes are identical, only who `peer` is
/// changes (the evaluated teammate vs. the evaluator).
fn parse_evals(v: &Value) -> Vec<PcvEval> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let peer = e.get("peer").cloned().unwrap_or(Value::Null);
            Some(PcvEval {
                peer_eval_id: e.get("peerEvalId")?.as_str()?.to_string(),
                peer: PcvPeer {
                    user_id: str_field(&peer, "userId").unwrap_or_default(),
                    first_name: str_field(&peer, "firstName").unwrap_or_default(),
                    last_name: str_field(&peer, "lastName").unwrap_or_default(),
                },
                target: str_field(e, "target").unwrap_or_default(),
                review_ratings: e
                    .get("reviewRatings")
                    .and_then(|x| x.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|r| {
                                Some(PcvEvalRating {
                                    rating_id: r.get("ratingId")?.as_str()?.to_string(),
                                    score: r.get("score").and_then(|x| x.as_i64()).unwrap_or(0),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                review_comments: e
                    .get("reviewComments")
                    .and_then(|x| x.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|c| {
                                Some(PcvEvalComment {
                                    comment_id: c.get("commentId")?.as_str()?.to_string(),
                                    comment_number: c
                                        .get("commentNumber")
                                        .and_then(|x| x.as_i64())
                                        .unwrap_or(0),
                                    comment: str_field(c, "comment").unwrap_or_default(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// Rubric from `GET /assignment/{aid}/eval/{target}/rubric`: rating items
/// (with their 1..10 levels) and comment items, split by shape.
fn parse_rubric(v: &Value) -> Result<PcvRubric, String> {
    let items = v
        .get("items")
        .and_then(|x| x.as_array())
        .ok_or_else(|| "Peerceptiv rubric response has no items array".to_string())?;
    let mut rubric = PcvRubric::default();
    for item in items {
        if let Some(rating_id) = str_field(item, "ratingId") {
            rubric.rating_items.push(PcvRatingItem {
                rating_id,
                name: str_field(item, "name").unwrap_or_default(),
                rating_levels: item
                    .get("ratingLevels")
                    .and_then(|x| x.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|l| {
                                Some(PcvRatingLevel {
                                    rating_level_id: l.get("ratingLevelId")?.as_str()?.to_string(),
                                    score: l.get("score").and_then(|x| x.as_i64()).unwrap_or(0),
                                    level_description: str_field(l, "levelDescription")
                                        .unwrap_or_default(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            });
        } else if let Some(comment_id) = str_field(item, "commentId") {
            rubric.comment_items.push(PcvCommentItem {
                comment_id,
                comment_name: str_field(item, "commentName").unwrap_or_default(),
            });
        }
    }
    Ok(rubric)
}

/// `{ratingId: score}` from `GET /eval/{peerEvalId}/map/rating`. Non-integer
/// scores are skipped rather than fatal — the write path re-PUTs the whole
/// map and must not drop entries it cannot round-trip.
fn parse_ratings_map(v: &Value) -> Result<HashMap<String, i64>, String> {
    let obj = v
        .as_object()
        .ok_or_else(|| "Peerceptiv rating map response is not a JSON object".to_string())?;
    let mut map = HashMap::with_capacity(obj.len());
    for (k, val) in obj {
        if let Some(score) = val.as_i64() {
            map.insert(k.clone(), score);
        }
    }
    Ok(map)
}

/// `{commentId: [texts...]}` from `GET /eval/{peerEvalId}/comments/numbered`.
fn parse_comments_map(v: &Value) -> Result<HashMap<String, Vec<String>>, String> {
    let obj = v
        .as_object()
        .ok_or_else(|| "Peerceptiv comments map response is not a JSON object".to_string())?;
    let mut map = HashMap::with_capacity(obj.len());
    for (k, val) in obj {
        let texts = val
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        map.insert(k.clone(), texts);
    }
    Ok(map)
}

/// The caller's group out of `GET /assignment/{aid}/groups`, found by
/// `groupMembers[].user.userId == uid`.
fn find_my_group(groups: &[Value], uid: &str) -> Option<Value> {
    groups
        .iter()
        .find(|g| {
            g.get("groupMembers")
                .and_then(|x| x.as_array())
                .is_some_and(|members| {
                    members
                        .iter()
                        .any(|m| m.pointer("/user/userId").and_then(|x| x.as_str()) == Some(uid))
                })
        })
        .cloned()
}

/// The handful of progress fields the status report needs, pulled out of the
/// wide `GET /assignment/{aid}/user/{uid}/progress` body.
pub fn progress_summary(v: &Value) -> Value {
    json!({
        "evaluationCount": v.get("evaluationCount").cloned().unwrap_or(Value::Null),
        "requiredEvaluationCount": v.get("requiredEvaluationCount").cloned().unwrap_or(Value::Null),
        "countEvaluationsComplete": v.get("countEvaluationsComplete").cloned().unwrap_or(Value::Null),
        "countEvaluationsIncomplete": v.get("countEvaluationsIncomplete").cloned().unwrap_or(Value::Null),
        "completedAllEvaluations": v.pointer("/status/completedAllEvaluations").cloned().unwrap_or(Value::Null),
        "missedEvaluating": v.pointer("/status/missedEvaluating").cloned().unwrap_or(Value::Null),
        "groupName": v.pointer("/group/groupName").cloned().unwrap_or(Value::Null),
    })
}

/// The full ratings map for a PUT after setting one rating. Full-map
/// replace semantics (PCV_SPEC.md §3): every existing entry is kept.
fn merged_rating_map(
    existing: HashMap<String, i64>,
    rating_id: &str,
    score: i64,
) -> HashMap<String, i64> {
    let mut merged = existing;
    merged.insert(rating_id.to_string(), score);
    merged
}

/// The full comments map for a PUT after writing one comment prompt: the
/// text lands at index 0 (commentNumber 1); a prompt with an existing
/// multi-text list keeps the remaining entries, and a missing prompt gets a
/// fresh single-entry list.
fn merged_comment_map(
    existing: HashMap<String, Vec<String>>,
    comment_id: &str,
    text: &str,
) -> HashMap<String, Vec<String>> {
    let mut merged = existing;
    let entry = merged.entry(comment_id.to_string()).or_default();
    if entry.is_empty() {
        entry.push(text.to_string());
    } else {
        entry[0] = text.to_string();
    }
    merged
}

// ===========================================================================
// Public types
// ===========================================================================

/// Caller identity from `GET /me`. NOTE: group member users carry
/// `firstName` = school ("CCDS") and `lastName` = name — display as
/// `lastName` (PCV_SPEC.md §2).
#[derive(Debug, Clone)]
pub struct PcvUser {
    pub user_id: String,
    pub first_name: String,
    pub last_name: String,
    pub email: String,
    pub role: String,
}

/// The teammate an eval was given to (or, on received evals, the evaluator).
#[derive(Debug, Clone, Default)]
pub struct PcvPeer {
    pub user_id: String,
    pub first_name: String,
    pub last_name: String,
}

/// One saved rating on an eval: which rubric rating item got which raw level
/// (1..10; `scaleScore` in the raw payload is derived, not needed here).
#[derive(Debug, Clone)]
pub struct PcvEvalRating {
    pub rating_id: String,
    pub score: i64,
}

/// One saved comment on an eval: which comment prompt got which text at
/// which number (array index + 1).
#[derive(Debug, Clone)]
pub struct PcvEvalComment {
    pub comment_id: String,
    pub comment_number: i64,
    pub comment: String,
}

/// One PeerEvaluation (given or received).
#[derive(Debug, Clone)]
pub struct PcvEval {
    pub peer_eval_id: String,
    /// The other side of the eval: the evaluated teammate on evals given,
    /// the evaluator on evals received.
    pub peer: PcvPeer,
    /// "MEMBER" for teammate evals.
    pub target: String,
    pub review_ratings: Vec<PcvEvalRating>,
    pub review_comments: Vec<PcvEvalComment>,
}

/// One rating level of a rubric item ("Never"=1 .. "Always"=9 style).
#[derive(Debug, Clone)]
pub struct PcvRatingLevel {
    pub rating_level_id: String,
    pub score: i64,
    pub level_description: String,
}

/// One rating item of a rubric (e.g. "Fulfilling one's responsibilities duly
/// (30%)") with its ordered levels.
#[derive(Debug, Clone, Default)]
pub struct PcvRatingItem {
    pub rating_id: String,
    pub name: String,
    pub rating_levels: Vec<PcvRatingLevel>,
}

impl PcvRatingItem {
    /// Resolve a level to its raw score: a number ("9") must be one of the
    /// item's levels; anything else is matched as a case-insensitive
    /// substring of a level description ("Always" -> 9). Used by the rate
    /// tool so users can name the level in plain English.
    pub fn resolve_level(&self, level: &str) -> Result<i64, String> {
        let query = level.trim();
        if let Ok(n) = query.parse::<i64>() {
            if self.rating_levels.iter().any(|l| l.score == n) {
                return Ok(n);
            }
        }
        let lower = query.to_lowercase();
        if let Some(hit) = self
            .rating_levels
            .iter()
            .find(|l| l.level_description.to_lowercase().contains(&lower))
        {
            return Ok(hit.score);
        }
        let levels = self
            .rating_levels
            .iter()
            .map(|l| format!("{}={}", l.score, l.level_description))
            .collect::<Vec<_>>()
            .join(", ");
        Err(format!(
            "Peerceptiv rubric item {:?} has no level {level:?}; levels: {levels}",
            self.name
        ))
    }
}

/// One comment item of a rubric (a written-feedback prompt).
#[derive(Debug, Clone)]
pub struct PcvCommentItem {
    pub comment_id: String,
    pub comment_name: String,
}

/// A rubric split by shape: rating items to score, comment items to write.
#[derive(Debug, Clone, Default)]
pub struct PcvRubric {
    pub rating_items: Vec<PcvRatingItem>,
    pub comment_items: Vec<PcvCommentItem>,
}

/// An authenticated Peerceptiv API session for one placed peer evaluation,
/// obtained by [`PcvSession::launch`]. All read/write methods hit the JSON
/// API with the `oidc_token`/`access_token` cookies captured at launch.
pub struct PcvSession {
    api_base: String,
    pcv_course_id: String,
    assignment_id: String,
    user_id: String,
    cookies: HashMap<String, String>,
    expires_at: Option<i64>,
    http: reqwest::Client,
}

impl PcvSession {
    /// Full LTI 1.3 launch chain from the existing BbRouter session, or a
    /// cached session when one is live for this (course_id, content_id).
    ///
    /// Cache hit = zero network. Cache miss = REST lookup of the content
    /// item (must be `resource/x-bb-bltiplacement-*`, gives the parentId),
    /// scrape the launchPlacement anchor off the parent's classic page, GET
    /// it (redirect walk; BbRouter only on the NTULearn origin), submit the
    /// auto-submit form, then read the API ids out of the 302 Location.
    /// A fresh launch ALWAYS restarts from the placement URL — the
    /// login_hint tokens inside the chain are single-use and are never
    /// cached (PCV_SPEC.md §1).
    ///
    /// This is also the refresh path: an expired cached session simply means
    /// the chain runs again (re-launching re-mints everything).
    pub async fn launch(
        state: &AppState,
        course_id: &str,
        content_id: &str,
    ) -> Result<Self, String> {
        let file = pcv_session_file();
        let cached = load_cached(&file);
        let cache_key = format!("{course_id}/{content_id}");

        // Fast path: live cookies + known ids for this (course, content).
        if let Some(entry) = cached.sessions.get(&cache_key) {
            if !entry.pcv_course_id.is_empty()
                && !entry.assignment_id.is_empty()
                && !entry.cookies.is_empty()
                && entry
                    .expires_at
                    .is_some_and(|exp| exp > now_secs() + TOKEN_EXPIRY_MARGIN_SECS)
            {
                tracing::debug!(
                    "Peerceptiv session reused from cache ({cache_key}, {} sessions known)",
                    cached.sessions.len()
                );
                return Self::from_parts(
                    entry.pcv_course_id.clone(),
                    entry.assignment_id.clone(),
                    entry.user_id.clone(),
                    entry.cookies.clone(),
                    entry.expires_at,
                );
            }
        }

        let bb_router = state.client.current_cookie_value().await;
        if bb_router.is_empty() {
            return Err(
                "Not logged in to NTULearn (no BbRouter session cookie), so the Peerceptiv \
                 launch cannot start. Call the ntulearn_login tool (or `ntulearn-mcp setup`), \
                 then retry."
                    .to_string(),
            );
        }
        let bb_base = state.client.base_url().trim_end_matches('/').to_string();
        let bb_host = reqwest::Url::parse(&bb_base)
            .ok()
            .and_then(|u| u.host_str().map(String::from))
            .unwrap_or_default();
        let http = launch_http_client()?;

        // Step 1: the REST record of the content item — it must be an LTI
        // placement, and it carries the parent whose classic page lists the
        // launch anchor (the REST record itself does not have the id).
        let item = state
            .client
            .get_json(
                &format!("/learn/api/public/v1/courses/{course_id}/contents/{content_id}"),
                &[("fields", "id,parentId,contentHandler")],
                None,
            )
            .await
            .map_err(|e| format!("Peerceptiv launch: content item lookup failed: {e}"))?;
        if !is_blti_placement_content(&item) {
            return Err(format!(
                "Peerceptiv launch: content {content_id} in course {course_id} is not an LTI \
                 placement (contentHandler.id = {:?}); Peerceptiv tools need a \
                 {PLACEMENT_HANDLER_PREFIX}* item.",
                item.pointer("/contentHandler/id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<missing>")
            ));
        }
        let parent_id = item
            .get("parentId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                format!(
                    "Peerceptiv launch: the REST record of content {content_id} has no \
                     parentId, so the parent page carrying the launchPlacement anchor cannot \
                     be fetched."
                )
            })?
            .to_string();

        // Step 2: the classic listContent page of the parent — the
        // launchPlacement anchor for this item lives there.
        let list_url = reqwest::Url::parse(&format!(
            "{bb_base}/webapps/blackboard/content/listContent.jsp?course_id={course_id}&content_id={parent_id}"
        ))
        .map_err(|e| format!("Peerceptiv launch: invalid listContent URL: {e}"))?;
        let mut jar = CookieJar::default();
        let list_page = get_following(&http, &mut jar, list_url, &bb_host, &bb_router).await?;
        if list_page.status != 200 {
            return Err(format!(
                "Peerceptiv launch: NTULearn returned HTTP {} for the parent content page. \
                 The session may have expired — call ntulearn_login and retry.",
                list_page.status
            ));
        }
        let placement_href = placement_href_from_page(&list_page.body, content_id).ok_or_else(
            || {
                format!(
                    "Peerceptiv launch: the parent page of content {content_id} has no \
                     launchPlacement anchor for it. Either the item moved, or the BbRouter \
                     session is stale — call ntulearn_login and retry."
                )
            },
        )?;
        let placement_url = list_page.url.join(&placement_href).map_err(|e| {
            format!("Peerceptiv launch: invalid placement href {placement_href:?}: {e}")
        })?;

        // Step 3: the redirect walk — placement -> sg.api peerceptiv
        // oidc/authorize -> developer.blackboard.com -> back to NTULearn
        // launchPlacement?cmd=authenticate -> the auto-submit form page.
        let form_page = get_following(&http, &mut jar, placement_url, &bb_host, &bb_router)
            .await?;
        if form_page.status != 200 {
            return Err(format!(
                "Peerceptiv launch: NTULearn returned HTTP {} for the launch placement page. \
                 The session may have expired — call ntulearn_login and retry.",
                form_page.status
            ));
        }

        // Step 4: submit the form (id_token + state); the 302 Location
        // carries the Peerceptiv course + assignment ids.
        let (action, id_token, lti_state) = parse_launch_form(&form_page.body)?;
        let action_url = form_page
            .url
            .join(&action)
            .map_err(|e| format!("Peerceptiv launch: invalid form action {action:?}: {e}"))?;
        let fields = vec![
            ("id_token".to_string(), id_token),
            ("state".to_string(), lti_state),
        ];
        let dashboard = post_launch_form(
            &http,
            &mut jar,
            action_url,
            &fields,
            &form_page.url,
            &bb_host,
            &bb_router,
        )
        .await?;
        let (pcv_course_id, assignment_id) = ids_from_dashboard_url(&dashboard).ok_or_else(
            || {
                format!(
                    "Peerceptiv launch: landing URL {dashboard} has no \
                     course/{{id}}/assignment/{{id}} segments, so the API ids cannot be read"
                )
            },
        )?;

        // Step 5: the API session cookies set along the chain.
        let cookies = jar_cookie_map(&jar);
        if cookies.is_empty() {
            return Err(
                "Peerceptiv launch: no cookies were set on sg.api.peerceptiv.com (expected \
                 oidc_token + access_token). The launch POST likely failed — retry, and if \
                 it persists call ntulearn_login."
                    .to_string(),
            );
        }
        let expires_at = cookies.values().filter_map(|v| jwt_exp(v)).min();

        let mut session = Self::from_parts(
            pcv_course_id.clone(),
            assignment_id.clone(),
            String::new(),
            cookies,
            expires_at,
        )?;

        // /me pins the caller's Peerceptiv user id — the write paths address
        // evals by it (POST .../eval/user/{myUid}/peer/{peerUid}/member).
        let me = session.me().await?;
        session.user_id = me.user_id.clone();

        // Persist, keeping other (course, content) sessions intact.
        let mut merged = cached;
        merged.sessions.insert(
            cache_key,
            CachedPcvEntry {
                pcv_course_id,
                assignment_id,
                user_id: session.user_id.clone(),
                cookies: session.cookies.clone(),
                expires_at,
            },
        );
        save_cached(&file, &merged);

        Ok(session)
    }

    /// Assemble from parts (launch success or cache hit). Building the HTTP
    /// client only fails on TLS backend init errors; surfaced as a normal
    /// error string like every other failure here.
    fn from_parts(
        pcv_course_id: String,
        assignment_id: String,
        user_id: String,
        cookies: HashMap<String, String>,
        expires_at: Option<i64>,
    ) -> Result<Self, String> {
        Ok(PcvSession {
            api_base: API_BASE.to_string(),
            pcv_course_id,
            assignment_id,
            user_id,
            cookies,
            expires_at,
            http: launch_http_client()?,
        })
    }

    /// The Peerceptiv course uuid this session launched into.
    pub fn pcv_course_id(&self) -> &str {
        &self.pcv_course_id
    }

    /// The Peerceptiv assignment uuid this session launched into.
    pub fn assignment_id(&self) -> &str {
        &self.assignment_id
    }

    /// The caller's Peerceptiv user id (filled by the launch chain's /me).
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// Unix seconds the session cookies are guessed to expire at (earliest
    /// JWT `exp` among them; None = none decodable).
    pub fn expires_at(&self) -> Option<i64> {
        self.expires_at
    }

    // -- transport ----------------------------------------------------------

    /// Cookie header for an API call: every session cookie, `name=value`,
    /// `; `-joined.
    fn api_cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// The session-dead error for 401/403: the handler layer re-launches on
    /// this; writes are never silently retried (PCV_SPEC.md §3).
    fn session_rejected(method: &str, url: &str, status: u16) -> String {
        format!(
            "Peerceptiv session rejected (HTTP {status} on {method} {url}). The Peerceptiv \
             session expired — retry the tool call so the LTI launch runs again; if it \
             persists, call ntulearn_login first."
        )
    }

    /// Authenticated API GET. On 401/403 the session is dead: the caller
    /// should re-run `launch` (which re-launches the whole chain).
    async fn api_get(&self, path: &str) -> Result<Value, String> {
        let url = format!("{}{}", self.api_base, path);
        let resp = self
            .http
            .get(&url)
            .header(reqwest::header::USER_AGENT, BROWSER_UA)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::COOKIE, self.api_cookie_header())
            .send()
            .await
            .map_err(|e| format!("Peerceptiv API GET {url} failed: {e}"))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status == 401 || status == 403 {
            return Err(Self::session_rejected("GET", &url, status));
        }
        if !(200..300).contains(&status) {
            return Err(format!(
                "Peerceptiv API GET {url} returned HTTP {status}: {}",
                snippet(&text)
            ));
        }
        serde_json::from_str(&text)
            .map_err(|e| format!("Peerceptiv API GET {url}: response is not valid JSON: {e}"))
    }

    /// Authenticated API write (POST/PUT). Returns (status, body). No
    /// retries: a rejected write surfaces to the caller as an error.
    async fn api_send(
        &self,
        method: &str,
        url: &str,
        payload: &Value,
    ) -> Result<(u16, Value), String> {
        let mut req = match method {
            "POST" => self.http.post(url),
            "PUT" => self.http.put(url),
            other => return Err(format!("Peerceptiv API: unsupported method {other}")),
        };
        req = req
            .header(reqwest::header::USER_AGENT, BROWSER_UA)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::COOKIE, self.api_cookie_header())
            .json(payload);
        let resp = req
            .send()
            .await
            .map_err(|e| format!("Peerceptiv API {method} {url} failed: {e}"))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status == 401 || status == 403 {
            return Err(Self::session_rejected(method, url, status));
        }
        if !(200..300).contains(&status) {
            return Err(format!(
                "Peerceptiv API {method} {url} returned HTTP {status}: {}",
                snippet(&text)
            ));
        }
        let body = if text.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&text).unwrap_or(json!({"raw": text}))
        };
        Ok((status, body))
    }

    /// `self.user_id`, or a readable error when the launch never filled it.
    fn require_user_id(&self) -> Result<&str, String> {
        if self.user_id.is_empty() {
            return Err(
                "Peerceptiv session has no user id (the launch chain did not complete /me). \
                 Retry the tool call so the launch runs again."
                    .to_string(),
            );
        }
        Ok(&self.user_id)
    }

    /// The dry-run result shape shared by all writes.
    fn dry_run_result(method: &str, url: &str, payload: &Value) -> Value {
        json!({
            "dryRun": true,
            "method": method,
            "url": url,
            "payload": payload,
            "note": "dry run: nothing was sent",
        })
    }

    // -- reads (PCV_SPEC.md §2) ---------------------------------------------

    /// Caller identity: `GET /me`. The launch chain calls this to pin the
    /// session's user id.
    pub async fn me(&self) -> Result<PcvUser, String> {
        let v = self.api_get("/me").await?;
        parse_me(&v)
    }

    /// The assignment record (name, deadlines, status flags; `user` is the
    /// OWNER'S user object, not the caller — use `me()` for self).
    pub async fn assignment(&self, aid: &str) -> Result<Value, String> {
        self.api_get(&format!("/assignment/{aid}")).await
    }

    /// Progress of one user on the assignment (counts, status flags, group).
    pub async fn progress(&self, aid: &str, uid: &str) -> Result<Value, String> {
        self.api_get(&format!("/assignment/{aid}/user/{uid}/progress"))
            .await
    }

    /// Evals the user has GIVEN (one per teammate; empty array when none).
    pub async fn evals_given(&self, aid: &str, uid: &str) -> Result<Vec<PcvEval>, String> {
        let v = self
            .api_get(&format!("/assignment/{aid}/evals/user/{uid}"))
            .await?;
        Ok(parse_evals(&v))
    }

    /// Evals the user has RECEIVED. Availability depends on release settings
    /// — an empty or gated response is the normal pre-release state, not an
    /// error.
    pub async fn evals_received(&self, aid: &str, uid: &str) -> Result<Vec<PcvEval>, String> {
        let v = self
            .api_get(&format!("/assignment/{aid}/evals/user/{uid}/received"))
            .await?;
        Ok(parse_evals(&v))
    }

    /// All groups of the assignment.
    pub async fn groups(&self, aid: &str) -> Result<Vec<Value>, String> {
        let v = self.api_get(&format!("/assignment/{aid}/groups")).await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// The group of one user: found by `groupMembers[].user.userId == uid`.
    pub async fn my_group(&self, aid: &str, uid: &str) -> Result<Value, String> {
        let groups = self.groups(aid).await?;
        find_my_group(&groups, uid).ok_or_else(|| {
            format!("Peerceptiv: user {uid} is not in any group of assignment {aid}")
        })
    }

    /// The rubric for one target kind ("MEMBER" for teammate evals):
    /// rating items (with levels) + comment items.
    pub async fn rubric(&self, aid: &str, target: &str) -> Result<PcvRubric, String> {
        let v = self
            .api_get(&format!("/assignment/{aid}/eval/{target}/rubric"))
            .await?;
        parse_rubric(&v)
    }

    /// My saved scores for one eval: `{ratingId: score}`.
    pub async fn ratings_map(&self, peer_eval_id: &str) -> Result<HashMap<String, i64>, String> {
        let v = self.api_get(&format!("/eval/{peer_eval_id}/map/rating")).await?;
        parse_ratings_map(&v)
    }

    /// My saved comment texts for one eval: `{commentId: [texts...]}`.
    pub async fn comments_numbered(
        &self,
        peer_eval_id: &str,
    ) -> Result<HashMap<String, Vec<String>>, String> {
        let v = self
            .api_get(&format!("/eval/{peer_eval_id}/comments/numbered"))
            .await?;
        parse_comments_map(&v)
    }

    // -- writes (PCV_SPEC.md §3) --------------------------------------------

    /// Create the (currently missing) eval for one peer:
    /// `POST /assignment/{aid}/eval/user/{myUid}/peer/{peerUid}/member`.
    ///
    /// `dry_run = true` returns the would-be request WITHOUT sending.
    pub async fn create_eval(
        &self,
        aid: &str,
        peer_uid: &str,
        dry_run: bool,
    ) -> Result<Value, String> {
        let my_uid = self.require_user_id()?;
        let url = format!(
            "{}/assignment/{aid}/eval/user/{my_uid}/peer/{peer_uid}/member",
            self.api_base
        );
        let payload = json!({});
        if dry_run {
            return Ok(Self::dry_run_result("POST", &url, &payload));
        }
        let (status, response) = self.api_send("POST", &url, &payload).await?;
        Ok(json!({
            "dryRun": false,
            "method": "POST",
            "url": url,
            "payload": payload,
            "status": status,
            "response": response,
        }))
    }

    /// Set one rating of an eval to a raw score: GET the map, set the key,
    /// PUT the FULL map back (full-replace semantics — every rating item
    /// must be sent, so existing entries are preserved).
    ///
    /// `dry_run = true` performs the lookup GET (that is how it resolves the
    /// map) and returns the would-be PUT WITHOUT sending.
    pub async fn upsert_rating(
        &self,
        peer_eval_id: &str,
        rating_id: &str,
        score: i64,
        dry_run: bool,
    ) -> Result<Value, String> {
        let existing = self.ratings_map(peer_eval_id).await?;
        let payload = json!(merged_rating_map(existing, rating_id, score));
        let url = format!("{}/eval/{peer_eval_id}/ratings", self.api_base);
        if dry_run {
            return Ok(Self::dry_run_result("PUT", &url, &payload));
        }
        let (status, response) = self.api_send("PUT", &url, &payload).await?;
        Ok(json!({
            "dryRun": false,
            "method": "PUT",
            "url": url,
            "payload": payload,
            "status": status,
            "response": response,
        }))
    }

    /// Write the text of one comment prompt of an eval: GET the map, set
    /// index 0 of that commentId's list (creating the list when missing,
    /// keeping any further entries), PUT the FULL map back.
    ///
    /// `dry_run = true` performs the lookup GET and returns the would-be PUT
    /// WITHOUT sending.
    pub async fn upsert_comment(
        &self,
        peer_eval_id: &str,
        comment_id: &str,
        text: &str,
        dry_run: bool,
    ) -> Result<Value, String> {
        let existing = self.comments_numbered(peer_eval_id).await?;
        let payload = json!(merged_comment_map(existing, comment_id, text));
        let url = format!("{}/eval/{peer_eval_id}/comments/numbered", self.api_base);
        if dry_run {
            return Ok(Self::dry_run_result("PUT", &url, &payload));
        }
        let (status, response) = self.api_send("PUT", &url, &payload).await?;
        Ok(json!({
            "dryRun": false,
            "method": "PUT",
            "url": url,
            "payload": payload,
            "status": status,
            "response": response,
        }))
    }
}

// ===========================================================================
// Tests — fixture-driven, no network (see PCV_SPEC.md fixtures)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/pcv/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap_or_else(|e| panic!("missing fixture {name}: {e}"))
    }

    fn fixture_json(name: &str) -> Value {
        serde_json::from_str(&fixture(name))
            .unwrap_or_else(|e| panic!("fixture {name} is not valid JSON: {e}"))
    }

    // -- launch form + URL parsing ------------------------------------------

    #[test]
    fn launch_form_extracts_action_and_hidden_fields() {
        let (action, id_token, state) = parse_launch_form(&fixture("launch_form.html")).expect("form");
        assert_eq!(action, "https://sg.api.peerceptiv.com/lti/launch");
        assert!(id_token.starts_with("eyJ"), "id_token should be a JWT");
        assert_eq!(state, "54dc7ff2984a");
        // The form's id_token is a Blackboard-minted JWT: decode its claims.
        let claims = jwt_claims(&id_token).expect("id_token claims");
        assert_eq!(
            claims.get("sub").and_then(|v| v.as_str()),
            Some("7a5d06c5ec3d4627b7738c3a4f421593")
        );
        assert_eq!(claims.get("exp").and_then(|v| v.as_i64()), Some(1793984450));
    }

    #[test]
    fn launch_form_missing_fields_is_an_llm_readable_error() {
        let err = parse_launch_form("<html><body><p>Session Expired</p></body></html>")
            .expect_err("no form should fail");
        assert!(err.contains("auto-submit form"), "error: {err}");
        assert!(err.contains("ntulearn_login"), "error: {err}");
    }

    #[test]
    fn placement_href_found_in_content_page_fixture() {
        let href = placement_href_from_page(&fixture("content_page.html"), "_6241854_1")
            .expect("placement href");
        assert_eq!(
            href,
            "/webapps/blackboard/execute/blti/launchPlacement?blti_placement_id=_1311_1&content_id=_6241854_1&course_id=_2707079_1"
        );
        // A different content item: the anchor must NOT match.
        assert_eq!(
            placement_href_from_page(&fixture("content_page.html"), "_9999999_1"),
            None
        );
    }

    #[test]
    fn placement_href_requires_exact_content_id_and_launch_executor() {
        // Two placements on one parent page: only the one for _6241854_1 may
        // match, and _6241854_11 must not match by prefix.
        let html = r#"<html><body>
            <a href="/webapps/blackboard/execute/blti/launchLink?content_id=_6241854_1&course_id=_1">FBF</a>
            <a href="/webapps/blackboard/execute/blti/launchPlacement?blti_placement_id=_1_1&content_id=_6241854_11&course_id=_1">other</a>
            <a href="/webapps/blackboard/execute/blti/launchPlacement?blti_placement_id=_2_1&content_id=_6241854_1&course_id=_1">mine</a>
        </body></html>"#;
        let href = placement_href_from_page(html, "_6241854_1").expect("href");
        assert!(href.contains("blti_placement_id=_2_1"), "{href}");
        assert!(href.ends_with("content_id=_6241854_1&course_id=_1"), "{href}");
    }

    #[test]
    fn blti_placement_handler_filter() {
        let placement = json!({"id": "_6241854_1", "contentHandler": {"id": "resource/x-bb-bltiplacement-4d3f4a6c2f"}});
        assert!(is_blti_placement_content(&placement));
        // FeedbackFruits items are blti-link, not bltiplacement
        let fbf = json!({"contentHandler": {"id": "resource/x-bb-blti-link"}});
        assert!(!is_blti_placement_content(&fbf));
        let doc = json!({"contentHandler": {"id": "resource/x-bb-document"}});
        assert!(!is_blti_placement_content(&doc));
        let none = json!({"id": "_1_1"});
        assert!(!is_blti_placement_content(&none));
    }

    #[test]
    fn dashboard_url_ids_extraction() {
        let url = reqwest::Url::parse(
            "https://asia.app.peerceptiv.com/course/59a86f0b-8807-4450-ae04-84e10a10bda6/assignment/63ba2209-566e-42d1-918a-9bd70056b2c8/dashboard?x=1",
        )
        .expect("url");
        assert_eq!(
            ids_from_dashboard_url(&url),
            Some((
                "59a86f0b-8807-4450-ae04-84e10a10bda6".to_string(),
                "63ba2209-566e-42d1-918a-9bd70056b2c8".to_string()
            ))
        );
        // no assignment segment -> no ids
        let bad = reqwest::Url::parse("https://asia.app.peerceptiv.com/course/c-1/dashboard")
            .expect("url");
        assert_eq!(ids_from_dashboard_url(&bad), None);
        // foreign host with the same shape still works — the ids come from
        // the path, not the host
        let other = reqwest::Url::parse("https://example.org/course/c-2/assignment/a-2").expect("url");
        assert_eq!(
            ids_from_dashboard_url(&other),
            Some(("c-2".to_string(), "a-2".to_string()))
        );
    }

    // -- JWT -----------------------------------------------------------------

    fn make_jwt(payload: &str) -> String {
        use base64::Engine;
        let enc = |s: &str| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s.as_bytes())
        };
        format!("{}.{}.sig", enc(r#"{"alg":"none"}"#), enc(payload))
    }

    #[test]
    fn jwt_decode_reads_sub_and_exp() {
        let token = make_jwt(r#"{"sub":"user-123","exp":1893456000}"#);
        assert_eq!(jwt_sub(&token).as_deref(), Some("user-123"));
        assert_eq!(jwt_exp(&token), Some(1893456000));
    }

    #[test]
    fn jwt_decode_rejects_malformed_tokens() {
        assert!(jwt_claims("not-a-jwt").is_err());
        assert!(jwt_claims("a.!!!not-base64!!!.c").is_err());
        // valid base64url but not JSON
        let bad = {
            use base64::Engine;
            format!(
                "x.{}.y",
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{not json")
            )
        };
        assert!(jwt_claims(&bad).is_err());
    }

    // -- typed parses against the real fixtures ------------------------------

    #[test]
    fn me_fixture_parses() {
        let me = parse_me(&fixture_json("me.json")).expect("me");
        assert_eq!(me.user_id, "644826d1-1cae-4660-b45d-e5db709475fb");
        // firstName is the school, lastName the name (PCV_SPEC.md §2)
        assert_eq!(me.first_name, "CCDS");
        assert_eq!(me.last_name, "GANGULA KARTHIK");
        assert_eq!(me.role, "STUDENT");
        assert_eq!(me.email, "KARTHIK027@e.ntu.edu.sg");
    }

    #[test]
    fn evals_given_fixture_parses() {
        let evals = parse_evals(&fixture_json("evals_given.json"));
        assert_eq!(evals.len(), 5);
        assert!(evals.iter().all(|e| e.target == "MEMBER"));
        let first = &evals[0];
        assert_eq!(first.peer_eval_id, "090b2722-02bc-41c8-a02a-6cb44eafc852");
        assert_eq!(first.peer.user_id, "3587cfc6-fce6-4e44-a1fb-892f1df84df1");
        assert_eq!(first.peer.last_name, "GAN YU XUAN ISABELLE");
        // 3 ratings, all saved at level 9, one "very good" comment at number 1
        assert_eq!(first.review_ratings.len(), 3);
        assert!(first.review_ratings.iter().all(|r| r.score == 9));
        assert!(first
            .review_ratings
            .iter()
            .any(|r| r.rating_id == "68e2e5e6-f141-4fa5-9066-38b241f4196d"));
        assert_eq!(first.review_comments.len(), 1);
        assert_eq!(first.review_comments[0].comment, "very good");
        assert_eq!(first.review_comments[0].comment_number, 1);
        assert_eq!(
            first.review_comments[0].comment_id,
            "9e0d9531-811d-46c6-a89a-a95599e9af36"
        );
        // every eval has a distinct peer
        let peers: std::collections::HashSet<&str> =
            evals.iter().map(|e| e.peer.user_id.as_str()).collect();
        assert_eq!(peers.len(), 5);
    }

    #[test]
    fn evals_parse_handles_empty_and_sparse_records() {
        // no evals yet: empty array is the normal state
        assert!(parse_evals(&json!([])).is_empty());
        // an eval with no peer / ratings / comments still parses (defaults)
        let evals = parse_evals(&json!([{"peerEvalId": "pe-1", "target": "MEMBER"}]));
        assert_eq!(evals.len(), 1);
        assert_eq!(evals[0].peer_eval_id, "pe-1");
        assert_eq!(evals[0].peer.user_id, "");
        assert!(evals[0].review_ratings.is_empty());
        assert!(evals[0].review_comments.is_empty());
    }

    #[test]
    fn rubric_fixture_parses() {
        let rubric = parse_rubric(&fixture_json("rubric_member.json")).expect("rubric");
        assert_eq!(rubric.rating_items.len(), 3);
        assert_eq!(rubric.comment_items.len(), 1);
        let first = &rubric.rating_items[0];
        assert_eq!(first.rating_id, "68e2e5e6-f141-4fa5-9066-38b241f4196d");
        assert!(first.name.contains("responsibilities duly (30%)"), "{}", first.name);
        assert_eq!(first.rating_levels.len(), 9);
        assert_eq!(first.rating_levels[0].score, 1);
        assert_eq!(first.rating_levels[0].level_description, "Never");
        assert_eq!(first.rating_levels[8].score, 9);
        assert_eq!(first.rating_levels[8].level_description, "Always");
        let comment = &rubric.comment_items[0];
        assert_eq!(comment.comment_id, "9e0d9531-811d-46c6-a89a-a95599e9af36");
        assert!(comment.comment_name.contains("justification for ratings"));
    }

    #[test]
    fn rubric_level_resolution() {
        let rubric = parse_rubric(&fixture_json("rubric_member.json")).expect("rubric");
        let item = &rubric.rating_items[0];
        // level description (case-insensitive substring)
        assert_eq!(item.resolve_level("Always").expect("Always"), 9);
        assert_eq!(item.resolve_level("rarely").expect("rarely"), 3);
        assert_eq!(item.resolve_level("Between 5 and 7").expect("between"), 6);
        // raw numeric level
        assert_eq!(item.resolve_level("9").expect("9"), 9);
        assert_eq!(item.resolve_level(" 1 ").expect("1"), 1);
        // unknown -> readable error listing the levels
        let err = item.resolve_level("bogus").expect_err("bogus");
        assert!(err.contains("9=Always"), "error: {err}");
        let err = item.resolve_level("42").expect_err("42");
        assert!(err.contains("no level"), "error: {err}");
    }

    #[test]
    fn ratings_map_parse_and_merge() {
        let existing = parse_ratings_map(&fixture_json("ratings_map.json")).expect("map");
        assert_eq!(existing.len(), 3);
        assert_eq!(existing.get("68e2e5e6-f141-4fa5-9066-38b241f4196d"), Some(&9));
        // merge: overwrite an existing rating, keep the other two
        let merged = merged_rating_map(existing, "68e2e5e6-f141-4fa5-9066-38b241f4196d", 7);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged.get("68e2e5e6-f141-4fa5-9066-38b241f4196d"), Some(&7));
        assert_eq!(merged.get("ccba709a-43d5-4638-95ce-d968b574586e"), Some(&9));
        // merge: add a brand-new rating
        let merged = merged_rating_map(merged, "new-rating", 5);
        assert_eq!(merged.len(), 4);
        assert_eq!(merged.get("new-rating"), Some(&5));
        // the PUT payload is the full map as a JSON object
        let payload = json!(merged);
        assert_eq!(payload.get("new-rating"), Some(&json!(5)));
        assert_eq!(payload.as_object().map(|o| o.len()), Some(4));
    }

    #[test]
    fn comments_map_merge_sets_index_zero() {
        let existing = parse_comments_map(&fixture_json("comments_numbered.json")).expect("map");
        assert_eq!(
            existing.get("9e0d9531-811d-46c6-a89a-a95599e9af36"),
            Some(&vec!["very good".to_string()])
        );
        // existing prompt: replace index 0
        let merged = merged_comment_map(existing, "9e0d9531-811d-46c6-a89a-a95599e9af36", "excellent");
        assert_eq!(
            merged.get("9e0d9531-811d-46c6-a89a-a95599e9af36"),
            Some(&vec!["excellent".to_string()])
        );
        // multi-text list: index 0 replaced, the rest kept
        let multi = HashMap::from([(
            "c-2".to_string(),
            vec!["old".to_string(), "second".to_string()],
        )]);
        let merged = merged_comment_map(multi, "c-2", "new");
        assert_eq!(
            merged.get("c-2"),
            Some(&vec!["new".to_string(), "second".to_string()])
        );
        // missing prompt: fresh single-entry list
        let merged = merged_comment_map(merged, "c-3", "fresh");
        assert_eq!(merged.get("c-3"), Some(&vec!["fresh".to_string()]));
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn progress_field_extraction() {
        let summary = progress_summary(&fixture_json("progress.json"));
        assert_eq!(summary.get("evaluationCount"), Some(&json!(5)));
        assert_eq!(summary.get("requiredEvaluationCount"), Some(&json!(5)));
        assert_eq!(summary.get("countEvaluationsComplete"), Some(&json!(5)));
        assert_eq!(summary.get("countEvaluationsIncomplete"), Some(&json!(0)));
        assert_eq!(summary.get("completedAllEvaluations"), Some(&json!(true)));
        assert_eq!(summary.get("missedEvaluating"), Some(&json!(false)));
        assert_eq!(summary.get("groupName"), Some(&json!("Group C27")));
        // missing fields surface as null, not errors
        let empty = progress_summary(&json!({}));
        assert_eq!(empty.get("completedAllEvaluations"), Some(&Value::Null));
    }

    #[test]
    fn my_group_find() {
        let groups = fixture_json("groups.json");
        let groups = groups.as_array().expect("groups array");
        let mine = find_my_group(groups, "644826d1-1cae-4660-b45d-e5db709475fb").expect("group");
        assert_eq!(mine.get("groupName"), Some(&json!("Group C27")));
        assert_eq!(
            mine.get("groupId"),
            Some(&json!("cca0303f-fd96-4f82-8fb9-b159dc3f83fe"))
        );
        // unknown user -> None (my_group turns this into an error string)
        assert!(find_my_group(groups, "no-such-user").is_none());
        // a member record without a user object must not panic the search
        let weird = json!([{"groupName": "X", "groupMembers": [{"role": "MEMBER"}]}]);
        let weird = weird.as_array().unwrap();
        assert!(find_my_group(weird, "anyone").is_none());
    }

    // -- cookie jar ----------------------------------------------------------

    #[test]
    fn cookie_jar_domain_matching() {
        let mut jar = CookieJar::default();
        jar.ingest(
            "sg.api.peerceptiv.com",
            &[
                "access_token=abc; Domain=.sg.api.peerceptiv.com; Path=/; Secure".to_string(),
                "hostonly=xyz".to_string(),
            ],
        );
        jar.ingest("ntulearn.ntu.edu.sg", &["JSESSIONID=j1; Path=/".to_string()]);
        // Domain cookie matches the API host
        let api = jar.pairs_for("sg.api.peerceptiv.com");
        assert!(api.iter().any(|p| p == "access_token=abc"), "{api:?}");
        assert!(api.iter().any(|p| p == "hostonly=xyz"), "{api:?}");
        // a subdomain of the cookie's Domain sees the Domain cookie but
        // not the host-only one (a sibling like other.api.peerceptiv.com
        // must NOT match Domain=.sg.api.peerceptiv.com — RFC 6265 §5.1.3)
        let sibling = jar.pairs_for("other.sg.api.peerceptiv.com");
        assert!(sibling.iter().any(|p| p == "access_token=abc"), "{sibling:?}");
        assert!(!sibling.iter().any(|p| p == "hostonly=xyz"), "{sibling:?}");
        // NTULearn host sees only its own cookie — never the peerceptiv ones
        let ntulearn = jar.pairs_for("ntulearn.ntu.edu.sg");
        assert!(ntulearn.iter().any(|p| p == "JSESSIONID=j1"), "{ntulearn:?}");
        assert!(!ntulearn.iter().any(|p| p.contains("access_token")), "{ntulearn:?}");
        // and the API cookie map for the launch
        let map = jar_cookie_map(&jar);
        assert_eq!(map.get("access_token").map(String::as_str), Some("abc"));
        assert_eq!(map.get("hostonly").map(String::as_str), Some("xyz"));
        assert!(!map.contains_key("JSESSIONID"));
    }

    #[test]
    fn cookie_header_adds_bb_router_only_on_ntulearn_host() {
        let mut jar = CookieJar::default();
        jar.ingest(
            "sg.api.peerceptiv.com",
            &["access_token=abc; Domain=.sg.api.peerceptiv.com".to_string()],
        );
        let on_bb =
            cookie_header(&jar, "ntulearn.ntu.edu.sg", "ntulearn.ntu.edu.sg", "expires:1,id:x");
        assert_eq!(on_bb.as_deref(), Some("BbRouter=expires:1,id:x"));
        let on_pcv =
            cookie_header(&jar, "sg.api.peerceptiv.com", "ntulearn.ntu.edu.sg", "expires:1,id:x");
        assert_eq!(on_pcv.as_deref(), Some("access_token=abc"));
        // developer.blackboard.com is a third party too — no BbRouter there
        let on_bbdev = cookie_header(&jar, "developer.blackboard.com", "ntulearn.ntu.edu.sg", "e:1");
        assert_eq!(on_bbdev, None);
    }

    #[test]
    fn form_urlencode_escapes_values() {
        let body = form_urlencode(&[
            ("id_token".to_string(), "a.b+c~d".to_string()),
            ("state".to_string(), "x y".to_string()),
        ]);
        // NON_ALPHANUMERIC escapes '.' and '_' too — still valid form
        // encoding, servers decode %XX identically.
        assert_eq!(body, "id%5Ftoken=a%2Eb%2Bc%7Ed&state=x%20y");
    }

    // -- session cache round-trip ---------------------------------------------

    #[test]
    fn session_file_round_trip_and_merge() {
        let dir = std::env::temp_dir().join(format!("ntulearn-pcv-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(PCV_SESSION_FILE);

        // missing file -> empty default
        assert_eq!(load_cached(&path).sessions.len(), 0);

        let mut sessions = CachedPcvSessions::default();
        sessions.sessions.insert(
            "_2707079_1/_6241854_1".to_string(),
            CachedPcvEntry {
                pcv_course_id: "59a86f0b".to_string(),
                assignment_id: "63ba2209".to_string(),
                user_id: "644826d1".to_string(),
                cookies: HashMap::from([
                    ("oidc_token".to_string(), make_jwt(r#"{"sub":"u","exp":1893456000}"#)),
                    ("access_token".to_string(), "at".to_string()),
                ]),
                expires_at: Some(now_secs() + 3600),
            },
        );
        save_cached(&path, &sessions);

        let loaded = load_cached(&path);
        let entry = loaded.sessions.get("_2707079_1/_6241854_1").expect("entry");
        assert_eq!(entry.pcv_course_id, "59a86f0b");
        assert_eq!(entry.assignment_id, "63ba2209");
        assert_eq!(entry.user_id, "644826d1");
        assert_eq!(entry.cookies.get("access_token").map(String::as_str), Some("at"));
        // the stored oidc_token is a decodable JWT with the right claims
        assert_eq!(
            jwt_exp(entry.cookies.get("oidc_token").unwrap()),
            Some(1893456000)
        );

        // corrupt file -> empty default, not an error
        std::fs::write(&path, "{not json").expect("write");
        assert_eq!(load_cached(&path).sessions.len(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_cache_shape_is_stable() {
        // The on-disk keys are a persistence contract with older/newer
        // binaries of this crate — pin them.
        let sessions = CachedPcvSessions {
            sessions: HashMap::from([(
                "c/x".to_string(),
                CachedPcvEntry {
                    pcv_course_id: "p".into(),
                    assignment_id: "a".into(),
                    user_id: "u".into(),
                    cookies: HashMap::from([("oidc_token".to_string(), "t".to_string())]),
                    expires_at: Some(1),
                },
            )]),
        };
        let text = serde_json::to_string(&sessions).expect("serialize");
        for key in [
            "sessions",
            "pcv_course_id",
            "assignment_id",
            "user_id",
            "cookies",
            "expires_at",
        ] {
            assert!(text.contains(&format!("\"{key}\"")), "missing {key} in {text}");
        }
    }

    // -- dry-run write shapes (no network) ------------------------------------

    fn offline_session() -> PcvSession {
        PcvSession::from_parts(
            "pcv-course-1".to_string(),
            "assignment-1".to_string(),
            "user-1".to_string(),
            HashMap::from([
                ("oidc_token".to_string(), "ot".to_string()),
                ("access_token".to_string(), "at".to_string()),
            ]),
            None,
        )
        .expect("session")
    }

    #[tokio::test]
    async fn create_eval_dry_run_shape() {
        let session = offline_session();
        let out = session
            .create_eval("aid-1", "peer-1", true)
            .await
            .expect("dry run");
        assert_eq!(out["dryRun"], json!(true));
        assert_eq!(out["method"], json!("POST"));
        assert_eq!(
            out["url"],
            json!("https://sg.api.peerceptiv.com/assignment/aid-1/eval/user/user-1/peer/peer-1/member")
        );
        assert_eq!(out["payload"], json!({}));
        assert_eq!(out["note"], json!("dry run: nothing was sent"));
    }
}
