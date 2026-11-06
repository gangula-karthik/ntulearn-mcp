//! FeedbackFruits (FBF) peer-evaluation integration.
//!
//! FeedbackFruits is embedded in NTULearn as an LTI 1.3 tool: there is no
//! direct login, the only way in is the BbRouter-authenticated launch chain
//! (see FBF_SPEC.md §1). This module owns that chain plus the thin typed
//! read/write surface over the JSON:API the FBF SPA itself uses
//! (FBF_SPEC.md §2–§3).
//!
//! Design notes:
//! * The workspace reqwest does NOT enable the `cookies` feature, so the LTI
//!   chain runs on a small hand-rolled cookie jar with manual redirect
//!   following. That also lets us observe every Set-Cookie hop (the FBF
//!   `access_token` arrives as a cookie on the POST response).
//! * The BbRouter header is only attached to the NTULearn origin. It is an
//!   NTULearn session credential; the redirect chain passes through
//!   feedbackfruits.com and developer.blackboard.com, and leaking a session
//!   cookie to third parties is exactly what a browser would not do.
//! * "Refresh" is a full re-launch: the cheapest correct refresh is redoing
//!   the 5-request chain with the (auto-refreshed) BbRouter cookie, per the
//!   spec. The refresh_token is persisted for future use but not exercised.
//! * A launch is per content item (the activity uuid comes from the landing
//!   URL), so the cache maps content_id -> activity_id and keeps one shared
//!   token — `launch()` is free for a known content_id with a live token.
//! * JSON is parsed with `serde_json::Value` + manual extraction (the same
//!   pragmatic style as handlers.rs) — the FBF payloads are large and only a
//!   handful of fields matter, so full serde modelling would be over-modelling.

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
/// Cap on manually-followed redirects. The observed chain is 4 hops + 1 POST;
/// anything beyond this is a redirect loop, not a launch.
const MAX_REDIRECTS: usize = 12;
/// Treat a token as expired this many seconds early, so a cached session is
/// not handed out seconds before its JWT dies.
const TOKEN_EXPIRY_MARGIN_SECS: i64 = 60;
/// Cache file name inside the config dir (next to the cookie file).
const FBF_SESSION_FILE: &str = "fbf.json";

// ===========================================================================
// JWT (decode only — never verify; we minted no keys, we just read claims)
// ===========================================================================

/// Decode the payload claims of a JWT. Base64url, no signature verification:
/// the token comes from FeedbackFruits over TLS and we only read `sub`/`exp`
/// from it, we do not make auth decisions on its integrity.
fn jwt_claims(token: &str) -> Result<Value, String> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| "FeedbackFruits token is not a JWT (no '.' separators)".to_string())?;
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|e| format!("FeedbackFruits JWT payload is not valid base64url: {e}"))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| format!("FeedbackFruits JWT payload is not valid JSON: {e}"))
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
/// LTI chain, where cookies are set on `.feedbackfruits.com` and re-sent on
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

    /// First cookie with this name on any domain. Used for the FBF auth
    /// cookies (`access_token`, …) whose exact setting domain we do not care
    /// to track — nothing else in the jar shares those names.
    fn get_anywhere(&self, name: &str) -> Option<String> {
        self.cookies
            .values()
            .find_map(|c| c.get(name).map(|(v, _)| v.clone()))
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

// ===========================================================================
// Session cache ({config_dir}/ntulearn-mcp/fbf.json)
// ===========================================================================

/// Persisted launch state. The token is shared across content items; the
/// activity map remembers which NTULearn content_id maps to which FBF
/// activity so a relaunch can skip the chain entirely while the token lives.
#[derive(Debug, Default, Serialize, Deserialize)]
struct CachedSession {
    #[serde(default)]
    api_base: String,
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    api_session: String,
    #[serde(default)]
    activity_ids: HashMap<String, String>,
}

/// Same config dir as the cookie file (cookie.rs::cookie_file_path) — one
/// place for everything this server persists about the NTULearn session.
fn fbf_session_file() -> PathBuf {
    cookie::cookie_file_path()
        .parent()
        .map(|p| p.join(FBF_SESSION_FILE))
        .unwrap_or_else(|| PathBuf::from(FBF_SESSION_FILE))
}

/// Load the cache; a missing or corrupt file is an empty session, not an
/// error — the launch chain will simply rewrite it.
fn load_cached(path: &Path) -> CachedSession {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist the cache (best-effort, like cookie.rs::write_cookie).
fn save_cached(path: &Path, session: &CachedSession) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(session) {
        let _ = std::fs::write(path, text);
    }
}

// ===========================================================================
// HTML / URL parsing (scraper)
// ===========================================================================

/// Extract the LTI auto-submit form: (action, id_token, state). Blackboard's
/// launchPlacement page contains exactly one form with hidden `id_token` and
/// `state` inputs posting to the FBF launch endpoint; we locate it by its
/// inputs rather than by name/id so a renamed form still works.
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
        "FeedbackFruits launch: the NTULearn page did not contain an LTI auto-submit form \
         (hidden id_token + state inputs). The BbRouter session is probably expired — \
         call the ntulearn_login tool, then retry."
            .to_string(),
    )
}

/// `<meta name="api-url" content="...">` from the FBF landing page — the
/// authoritative API base (region-prefixed host).
fn parse_api_url_meta(html: &str) -> Option<String> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let sel = Selector::parse(r#"meta[name="api-url"]"#).ok()?;
    doc.select(&sel)
        .next()?
        .value()
        .attr("content")
        .map(|s| s.trim().to_string())
}

/// FBF activity uuid from the landing URL path (`.../activities/{uuid}`).
/// Path segments are NOT percent-decoded first: the course segment is
/// `lti%2Fcourse/{uuid}` and decoding it would split on the embedded slash.
fn activity_id_from_url(url: &reqwest::Url) -> Option<String> {
    let segments: Vec<&str> = url.path_segments()?.collect();
    segments
        .windows(2)
        .find(|w| w[0] == "activities")
        .map(|w| w[1].to_string())
        .filter(|id| !id.is_empty())
}

/// Fallback API base when the landing page has no api-url meta: the FBF SPA
/// host `au.feedbackfruits.com` (or `au-api.…`) implies
/// `https://api.au.feedbackfruits.com`.
fn derive_api_base(url: &reqwest::Url) -> Option<String> {
    let host = url.host_str()?;
    if !host.ends_with("feedbackfruits.com") {
        return None;
    }
    let region = host.split('.').next()?.trim_end_matches("-api");
    if region.is_empty() {
        return None;
    }
    Some(format!("https://api.{region}.feedbackfruits.com"))
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
        .map_err(|e| format!("FeedbackFruits launch: reqwest client build failed: {e}"))
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
            .map_err(|e| format!("FeedbackFruits launch: GET {url} failed: {e}"))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        jar.ingest(&host, &set_cookie_values(&headers));
        if (300..400).contains(&status) {
            let location = headers
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    format!("FeedbackFruits launch: HTTP {status} from {url} has no Location header")
                })?;
            url = url.join(location).map_err(|e| {
                format!(
                    "FeedbackFruits launch: cannot resolve redirect target {location:?} \
                     against {url}: {e}"
                )
            })?;
            continue;
        }
        let body = resp
            .text()
            .await
            .map_err(|e| format!("FeedbackFruits launch: reading body of {url} failed: {e}"))?;
        return Ok(Page { url, status, body });
    }
    Err(format!(
        "FeedbackFruits launch: more than {MAX_REDIRECTS} redirects starting at {start}"
    ))
}

/// POST the auto-submit form fields (id_token + state) to the action URL,
/// then follow any redirects as GETs until the landing page. The response to
/// the POST sets the FBF auth cookies on `.feedbackfruits.com`; the redirect
/// after it lands on the SPA URL that carries the activity uuid.
async fn post_form_following(
    http: &reqwest::Client,
    jar: &mut CookieJar,
    action: reqwest::Url,
    fields: &[(String, String)],
    referer: &reqwest::Url,
    bb_host: &str,
    bb_router: &str,
) -> Result<Page, String> {
    let body = form_urlencode(fields);
    let origin = match (action.scheme(), action.host_str()) {
        (scheme, Some(h)) => format!("{scheme}://{h}"),
        _ => String::new(),
    };
    // The launch chain POSTs twice: the form action (au-api) answers 307 and
    // the SAME body must be re-POSTed to the redirected URL (api.au) — a GET
    // there is a 404, and a browser would replay the POST. 301/302/303 after
    // a POST become GETs, which get_following handles.
    let mut url = action.clone();
    let mut first_post = true;
    for _ in 0..=MAX_REDIRECTS {
        let host = url.host_str().unwrap_or_default().to_string();
        let mut req = http
            .post(url.clone())
            .header(reqwest::header::USER_AGENT, BROWSER_UA)
            .header(
                reqwest::header::ACCEPT,
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
            .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body.clone());
        // Origin/Referer describe the page the form lives on — only the
        // first POST is that form submission; replayed hops carry neither.
        if first_post {
            req = req
                .header(reqwest::header::ORIGIN, origin.as_str())
                .header(reqwest::header::REFERER, referer.as_str());
            first_post = false;
        }
        if let Some(cookies) = cookie_header(jar, &host, bb_host, bb_router) {
            req = req.header(reqwest::header::COOKIE, cookies);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("FeedbackFruits launch: POST to {url} failed: {e}"))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        jar.ingest(&host, &set_cookie_values(&headers));
        if status == 307 || status == 308 {
            let location = headers
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    format!("FeedbackFruits launch: HTTP {status} from {url} has no Location header")
                })?;
            url = url.join(location).map_err(|e| {
                format!(
                    "FeedbackFruits launch: cannot resolve redirect target {location:?} \
                     against {url}: {e}"
                )
            })?;
            continue;
        }
        if (300..400).contains(&status) {
            let location = headers
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    format!("FeedbackFruits launch: HTTP {status} from {url} has no Location header")
                })?;
            let next = url.join(location).map_err(|e| {
                format!(
                    "FeedbackFruits launch: cannot resolve redirect target {location:?} \
                     against {url}: {e}"
                )
            })?;
            return get_following(http, jar, next, bb_host, bb_router).await;
        }
        let text = resp
            .text()
            .await
            .map_err(|e| format!("FeedbackFruits launch: reading body of {url} failed: {e}"))?;
        return Ok(Page { url, status, body: text });
    }
    Err(format!(
        "FeedbackFruits launch: more than {MAX_REDIRECTS} redirects after POST to {action}"
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
// JSON:API extraction (serde_json::Value + manual pulls; see module docs)
// ===========================================================================

/// id of a to-one relationship (`relationships.{rel}.data.id`).
fn rel_id(v: &Value, rel: &str) -> Option<String> {
    v.get("relationships")?
        .get(rel)?
        .get("data")?
        .get("id")?
        .as_str()
        .map(|s| s.to_string())
}

fn attr_str(v: &Value, name: &str) -> Option<String> {
    v.pointer(&format!("/attributes/{name}"))?
        .as_str()
        .map(|s| s.to_string())
}

fn data_array(v: &Value) -> Vec<Value> {
    v.get("data")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default()
}

fn parse_activity(v: &Value) -> Result<FbfActivity, String> {
    let data = v
        .get("data")
        .ok_or_else(|| "FeedbackFruits activity response has no data object".to_string())?;
    let id = data
        .get("id")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "FeedbackFruits activity response has no data.id".to_string())?
        .to_string();
    let title = attr_str(data, "title").unwrap_or_default();
    Ok(FbfActivity {
        id,
        title,
        progress_id: rel_id(data, "progress"),
    })
}

fn task_from_value(t: &Value) -> Option<FbfTask> {
    Some(FbfTask {
        id: t.get("id")?.as_str()?.to_string(),
        title: attr_str(t, "title"),
        kind: t.get("type")?.as_str()?.to_string(),
        deadline: attr_str(t, "deadline-at"),
        progress_id: rel_id(t, "progress"),
        reviewed_count: t
            .pointer("/attributes/reviewed-count")
            .and_then(|x| x.as_u64())
            .map(|n| n as u32),
    })
}

/// Parse `GET /v1/activities/{id}?include=tasks`: membership and order come
/// from the activity's own `relationships.tasks.data[]` (the source of
/// truth), bodies from `included`.
fn parse_tasks(v: &Value) -> Vec<FbfTask> {
    let included = v
        .get("included")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let order: Vec<String> = v
        .pointer("/data/relationships/tasks/data")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| r.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let by_id: HashMap<String, &Value> = included
        .iter()
        .filter_map(|t| {
            t.get("id")
                .and_then(|i| i.as_str())
                .map(|id| (id.to_string(), t))
        })
        .collect();
    if order.is_empty() {
        return included.iter().filter_map(task_from_value).collect();
    }
    order
        .iter()
        .filter_map(|id| by_id.get(id).and_then(|t| task_from_value(t)))
        .collect()
}

/// Progress % (0..100) from a progresses object.
fn parse_progress(v: &Value) -> Result<u8, String> {
    let raw = v
        .pointer("/data/attributes/progress")
        .ok_or_else(|| "FeedbackFruits progress response has no attributes.progress".to_string())?;
    let n = raw
        .as_f64()
        .ok_or_else(|| format!("FeedbackFruits progress is not a number: {raw}"))?;
    Ok(n.round().clamp(0.0, 100.0) as u8)
}

/// Allocations as (alloc_id, other_user_id). For `direction == "giver"` the
/// other side is `allocable` (the ratee); for `"receiver"` it is `user` (the
/// person whose feedback I am receiving).
fn parse_allocations(v: &Value, direction: &str) -> Vec<(String, String)> {
    let other_rel = if direction == "receiver" {
        "user"
    } else {
        "allocable"
    };
    data_array(v)
        .iter()
        .filter_map(|a| {
            let id = a.get("id")?.as_str()?.to_string();
            let other = rel_id(a, other_rel).unwrap_or_default();
            Some((id, other))
        })
        .collect()
}

fn parse_ratings(v: &Value) -> Vec<FbfRating> {
    data_array(v)
        .iter()
        .filter_map(|r| {
            Some(FbfRating {
                id: r.get("id")?.as_str()?.to_string(),
                criterion_id: rel_id(r, "criterion").unwrap_or_default(),
                option_id: rel_id(r, "option").unwrap_or_default(),
            })
        })
        .collect()
}

fn parse_criteria(v: &Value) -> Vec<FbfCriterion> {
    data_array(v)
        .iter()
        .filter_map(|c| {
            let option_ids = c
                .pointer("/relationships/options/data")
                .and_then(|x| x.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|o| o.get("id").and_then(|i| i.as_str()).map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            Some(FbfCriterion {
                id: c.get("id")?.as_str()?.to_string(),
                title: attr_str(c, "title").unwrap_or_default(),
                facet_weight: c
                    .pointer("/attributes/facet-weight")
                    .and_then(|x| x.as_f64())
                    .unwrap_or(0.0),
                option_ids,
            })
        })
        .collect()
}

fn parse_option(v: &Value) -> Result<FbfOption, String> {
    let data = v
        .get("data")
        .ok_or_else(|| "FeedbackFruits option response has no data object".to_string())?;
    let id = data
        .get("id")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "FeedbackFruits option response has no data.id".to_string())?
        .to_string();
    Ok(FbfOption {
        id,
        title: attr_str(data, "title").unwrap_or_default(),
        weight: data
            .pointer("/attributes/weight")
            .and_then(|x| x.as_i64())
            .unwrap_or(0),
        description: attr_str(data, "description"),
    })
}

fn parse_user_name(v: &Value) -> Result<String, String> {
    v.pointer("/data/attributes/name")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "FeedbackFruits user response has no attributes.name".to_string())
}

fn parse_reviews(v: &Value) -> Vec<FbfReview> {
    data_array(v)
        .iter()
        .filter_map(|r| {
            Some(FbfReview {
                id: r.get("id")?.as_str()?.to_string(),
                body: attr_str(r, "body").unwrap_or_default(),
                kind: attr_str(r, "kind"),
                user_id: rel_id(r, "user"),
                criterion_id: rel_id(r, "criterion"),
            })
        })
        .collect()
}

/// Release state from the tasks-inlined activity response:
/// grades-published-at lives on the summative-grade-tasks, ratings/reviews
/// published-at on the user-review-view-tasks. One request, three fields.
fn parse_release_state(v: &Value) -> Value {
    let mut grades: Value = Value::Null;
    let mut ratings: Value = Value::Null;
    let mut reviews: Value = Value::Null;
    for t in v
        .get("included")
        .and_then(|x| x.as_array())
        .into_iter()
        .flatten()
    {
        match t.get("type").and_then(|x| x.as_str()).unwrap_or("") {
            "summative-grade-tasks" => {
                grades = t
                    .pointer("/attributes/grades-published-at")
                    .cloned()
                    .unwrap_or(Value::Null);
            }
            "user-review-view-tasks" => {
                ratings = t
                    .pointer("/attributes/ratings-published-at")
                    .cloned()
                    .unwrap_or(Value::Null);
                reviews = t
                    .pointer("/attributes/reviews-published-at")
                    .cloned()
                    .unwrap_or(Value::Null);
            }
            _ => {}
        }
    }
    json!({
        "gradesPublishedAt": grades,
        "ratingsPublishedAt": ratings,
        "reviewsPublishedAt": reviews,
    })
}

// ===========================================================================
// Public types
// ===========================================================================

/// One FeedbackFruits activity (a placed peer-evaluation in a course).
#[derive(Debug, Clone)]
pub struct FbfActivity {
    pub id: String,
    pub title: String,
    /// Activity-level progress object; feed to [`FbfSession::progress`].
    pub progress_id: Option<String>,
}

/// One task inside an activity. `kind` is the JSON:API resource type
/// ("instruct-tasks", "user-review-tasks", "user-review-view-tasks",
/// "summative-grade-tasks"); `title` distinguishes e.g. "Give ratings"
/// (rubric) from "Overall comments" (written reviews) on the same kind.
#[derive(Debug, Clone)]
pub struct FbfTask {
    pub id: String,
    pub title: Option<String>,
    pub kind: String,
    /// RFC3339 deadline-at, if the task has one.
    pub deadline: Option<String>,
    pub progress_id: Option<String>,
    /// How many required reviews/ratings on this task are already done.
    pub reviewed_count: Option<u32>,
}

/// One rubric rating: which option was picked for which criterion.
#[derive(Debug, Clone)]
pub struct FbfRating {
    pub id: String,
    pub criterion_id: String,
    pub option_id: String,
}

/// One rubric criterion; `option_ids` are the ordered rating levels
/// (fetch each via [`FbfSession::option`]).
#[derive(Debug, Clone)]
pub struct FbfCriterion {
    pub id: String,
    pub title: String,
    pub facet_weight: f64,
    pub option_ids: Vec<String>,
}

/// One rating level of a criterion ("Never" .. "Always" style).
#[derive(Debug, Clone)]
pub struct FbfOption {
    pub id: String,
    pub title: String,
    pub weight: i64,
    pub description: Option<String>,
}

/// One written review on an allocation.
#[derive(Debug, Clone)]
pub struct FbfReview {
    pub id: String,
    pub body: String,
    /// e.g. "overall_comment"; None when the API omits it.
    pub kind: Option<String>,
    /// Author of the review.
    pub user_id: Option<String>,
    /// Criterion the review is attached to (tasks may require one review
    /// per criterion).
    pub criterion_id: Option<String>,
}

/// An authenticated FeedbackFruits API session for one activity, obtained by
/// [`FbfSession::launch`]. All read/write methods hit the JSON:API with the
/// Bearer access_token captured at launch.
pub struct FbfSession {
    api_base: String,
    access_token: String,
    api_session: String,
    user_id: String,
    activity_id: String,
    exp: Option<i64>,
    http: reqwest::Client,
}

impl FbfSession {
    /// Full LTI 1.3 launch chain from the existing BbRouter session, or a
    /// cached session when one is live for this content item.
    ///
    /// Cache hit = zero network. Cache miss = GET launchLink, follow ~4
    /// redirects (BbRouter only on the NTULearn origin), parse the
    /// auto-submit form, POST id_token+state, then read the landing URL for
    /// the activity uuid and the page's api-url meta for the API base.
    ///
    /// This is also the refresh path: an expired cached token simply means
    /// the chain runs again (re-launching re-mints everything).
    pub async fn launch(
        state: &AppState,
        course_id: &str,
        content_id: &str,
    ) -> Result<Self, String> {
        let file = fbf_session_file();
        let cached = load_cached(&file);

        // Fast path: live token + known content item.
        if let Some(activity_id) = cached.activity_ids.get(content_id).cloned() {
            if !cached.api_base.is_empty() && !cached.access_token.is_empty() {
                if let Some(exp) = jwt_exp(&cached.access_token) {
                    if exp > now_secs() + TOKEN_EXPIRY_MARGIN_SECS {
                        if let Some(user_id) = jwt_sub(&cached.access_token) {
                            tracing::debug!(
                                "FBF session reused from cache ({} content items known)",
                                cached.activity_ids.len()
                            );
                            return Self::from_parts(
                                cached.api_base,
                                cached.access_token,
                                cached.api_session,
                                user_id,
                                activity_id,
                                Some(exp),
                            );
                        }
                    }
                }
            }
        }

        let bb_router = state.client.current_cookie_value().await;
        if bb_router.is_empty() {
            return Err(
                "Not logged in to NTULearn (no BbRouter session cookie), so the FeedbackFruits \
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

        // Step 1: the LTI launch link. Redirects hop out to feedbackfruits,
        // back through developer.blackboard.com, and end on the NTULearn
        // launchPlacement page carrying the auto-submit form.
        let start = reqwest::Url::parse(&format!(
            "{bb_base}/webapps/blackboard/execute/blti/launchLink?course_id={course_id}&content_id={content_id}"
        ))
        .map_err(|e| format!("FeedbackFruits launch: invalid launch link URL: {e}"))?;
        let mut jar = CookieJar::default();
        let form_page = get_following(&http, &mut jar, start, &bb_host, &bb_router).await?;
        if form_page.status != 200 {
            return Err(format!(
                "FeedbackFruits launch: NTULearn returned HTTP {} for the launch link. \
                 The session may have expired — call ntulearn_login and retry.",
                form_page.status
            ));
        }

        // Step 2: submit the auto-submit form (id_token + state).
        let (action, id_token, lti_state) = parse_launch_form(&form_page.body)?;
        let action_url = form_page
            .url
            .join(&action)
            .map_err(|e| format!("FeedbackFruits launch: invalid form action {action:?}: {e}"))?;
        let fields = vec![
            ("id_token".to_string(), id_token),
            ("state".to_string(), lti_state),
        ];
        let landing = post_form_following(
            &http,
            &mut jar,
            action_url,
            &fields,
            &form_page.url,
            &bb_host,
            &bb_router,
        )
        .await?;

        // Step 3: read the session out of what landed.
        let activity_id = activity_id_from_url(&landing.url).ok_or_else(|| {
            format!(
                "FeedbackFruits launch: landing URL {} has no activities/{{uuid}} segment, \
                 so the activity id cannot be read",
                landing.url
            )
        })?;
        let api_base = parse_api_url_meta(&landing.body)
            .or_else(|| derive_api_base(&landing.url))
            .ok_or_else(|| {
                "FeedbackFruits launch: the landing page has no <meta name=\"api-url\"> and \
                 the API base could not be derived from its URL"
                    .to_string()
            })?
            .trim_end_matches('/')
            .to_string();
        let access_token = jar.get_anywhere("access_token").ok_or_else(|| {
            "FeedbackFruits launch: no access_token cookie was set on .feedbackfruits.com. \
             The launch POST likely failed — retry, and if it persists call ntulearn_login."
                .to_string()
        })?;
        let refresh_token = jar.get_anywhere("refresh_token").unwrap_or_default();
        let api_session = jar
            .get_anywhere("_feedbackfruits-api_session")
            .unwrap_or_default();
        let user_id = jwt_sub(&access_token).ok_or_else(|| {
            "FeedbackFruits launch: the access_token JWT has no sub claim".to_string()
        })?;
        let exp = jwt_exp(&access_token);

        // Persist, keeping the content->activity map for other items.
        let mut merged = cached;
        merged.api_base = api_base.clone();
        merged.access_token = access_token.clone();
        merged.refresh_token = refresh_token;
        merged.api_session = api_session.clone();
        merged.activity_ids.insert(content_id.to_string(), activity_id.clone());
        save_cached(&file, &merged);

        Self::from_parts(
            api_base,
            access_token,
            api_session,
            user_id,
            activity_id,
            exp,
        )
    }

    /// Assemble from parts (launch success or cache hit). Building the HTTP
    /// client only fails on TLS backend init errors; surfaced as a normal
    /// error string like every other failure here.
    fn from_parts(
        api_base: String,
        access_token: String,
        api_session: String,
        user_id: String,
        activity_id: String,
        exp: Option<i64>,
    ) -> Result<Self, String> {
        Ok(FbfSession {
            api_base,
            access_token,
            api_session,
            user_id,
            activity_id,
            exp,
            http: launch_http_client()?,
        })
    }

    /// JWT `sub` of the access_token — the FBF user id ("my_user_id").
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// The activity this session launched into (from the landing URL).
    pub fn activity_id(&self) -> &str {
        &self.activity_id
    }

    /// API base (e.g. "https://api.au.feedbackfruits.com").
    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// Unix seconds the access_token expires at (None = unparsable).
    pub fn token_expires_at(&self) -> Option<i64> {
        self.exp
    }

    // -- transport ----------------------------------------------------------

    /// Authenticated JSON:API GET. On 401 the session is dead: the caller
    /// should re-run `launch` (which re-launches the whole chain).
    async fn api_get(&self, path: &str) -> Result<Value, String> {
        let url = format!("{}{}", self.api_base, path);
        let mut req = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Accept", "application/vnd.api+json");
        if !self.api_session.is_empty() {
            // The API honors the Bearer token; the session cookie is sent as
            // well because the SPA always carries both.
            req = req.header(
                reqwest::header::COOKIE,
                format!("_feedbackfruits-api_session={}", self.api_session),
            );
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("FeedbackFruits API GET {url} failed: {e}"))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status == 401 {
            return Err(format!(
                "FeedbackFruits access token rejected (HTTP 401 on GET {url}). The session \
                 expired — retry the tool call so the LTI launch runs again; if it persists, \
                 call ntulearn_login first."
            ));
        }
        if !(200..300).contains(&status) {
            return Err(format!(
                "FeedbackFruits API GET {url} returned HTTP {status}: {}",
                snippet(&text)
            ));
        }
        serde_json::from_str(&text)
            .map_err(|e| format!("FeedbackFruits API GET {url}: response is not valid JSON: {e}"))
    }

    /// Authenticated JSON:API write (POST/PATCH). Returns (status, body).
    async fn api_send(&self, method: &str, url: &str, payload: &Value) -> Result<(u16, Value), String> {
        let mut req = match method {
            "POST" => self.http.post(url),
            "PATCH" => self.http.patch(url),
            other => return Err(format!("FeedbackFruits API: unsupported method {other}")),
        };
        req = req
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Accept", "application/vnd.api+json")
            .header(reqwest::header::CONTENT_TYPE, "application/vnd.api+json")
            .json(payload);
        let resp = req
            .send()
            .await
            .map_err(|e| format!("FeedbackFruits API {method} {url} failed: {e}"))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status == 401 {
            return Err(format!(
                "FeedbackFruits access token rejected (HTTP 401 on {method} {url}). The session \
                 expired — retry the tool call so the LTI launch runs again."
            ));
        }
        if !(200..300).contains(&status) {
            return Err(format!(
                "FeedbackFruits API {method} {url} returned HTTP {status}: {}",
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

    // -- reads (FBF_SPEC.md §2) ---------------------------------------------

    /// The activity (title + activity-level progress relationship).
    pub async fn activity(&self, activity_id: &str) -> Result<FbfActivity, String> {
        let v = self
            .api_get(&format!("/v1/activities/{activity_id}"))
            .await?;
        parse_activity(&v)
    }

    /// All tasks of the activity, in the activity's own order.
    pub async fn tasks(&self, activity_id: &str) -> Result<Vec<FbfTask>, String> {
        let v = self
            .api_get(&format!("/v1/activities/{activity_id}?include=tasks"))
            .await?;
        Ok(parse_tasks(&v))
    }

    /// Progress percent (0..100) of a task or activity.
    pub async fn progress(&self, progress_id: &str) -> Result<u8, String> {
        let v = self.api_get(&format!("/v1/progresses/{progress_id}")).await?;
        parse_progress(&v)
    }

    /// Allocations I have to review on a "Give ratings"/"Overall comments"
    /// task: (allocation_id, ratee user id), one per teammate.
    pub async fn allocations_given(
        &self,
        task_id: &str,
    ) -> Result<Vec<(String, String)>, String> {
        // The filter[task_id]+filter[person_id] combo is the verified one;
        // other filters on this endpoint 403 for students.
        let v = self
            .api_get(&format!(
                "/v1/assignments/allocations?filter[task_id]={task_id}&filter[person_id]={}",
                self.user_id
            ))
            .await?;
        Ok(parse_allocations(&v, "giver"))
    }

    /// Allocations where I am the reviewee on a view task:
    /// (allocation_id, giver user id). `data: []` until results are released
    /// — that is the normal unpublished state, not an error.
    pub async fn allocations_received(
        &self,
        task_id: &str,
    ) -> Result<Vec<(String, String)>, String> {
        let v = self
            .api_get(&format!(
                "/v1/assignments/tasks/{task_id}/allocations?direction=receiver"
            ))
            .await?;
        Ok(parse_allocations(&v, "receiver"))
    }

    /// My ratings on one allocation.
    pub async fn ratings(&self, allocation_id: &str) -> Result<Vec<FbfRating>, String> {
        let v = self
            .api_get(&format!("/v1/assignments/allocations/{allocation_id}/ratings"))
            .await?;
        Ok(parse_ratings(&v))
    }

    /// Rubric criteria of an allocation, each with its ordered option ids.
    pub async fn criteria(&self, allocation_id: &str) -> Result<Vec<FbfCriterion>, String> {
        let v = self
            .api_get(&format!(
                "/v1/assignments/allocations/{allocation_id}/criteria"
            ))
            .await?;
        Ok(parse_criteria(&v))
    }

    /// One criterion option (rating level).
    pub async fn option(&self, option_id: &str) -> Result<FbfOption, String> {
        let v = self
            .api_get(&format!("/v1/assignments/criterion_options/{option_id}"))
            .await?;
        parse_option(&v)
    }

    /// Display name of a FeedbackFruits user.
    pub async fn user_name(&self, user_id: &str) -> Result<String, String> {
        let v = self.api_get(&format!("/v1/users/{user_id}")).await?;
        parse_user_name(&v)
    }

    /// Written reviews on one allocation (bodies for given/received
    /// rendering, and the lookup used by `upsert_review`).
    pub async fn reviews(&self, allocation_id: &str) -> Result<Vec<FbfReview>, String> {
        let v = self
            .api_get(&format!("/v1/assignments/allocations/{allocation_id}/reviews"))
            .await?;
        Ok(parse_reviews(&v))
    }

    /// Release state of the activity: `{"gradesPublishedAt": …,
    /// "ratingsPublishedAt": …, "reviewsPublishedAt": …}` (null = not yet
    /// released). One request; read off the included tasks.
    pub async fn release_state(&self, activity_id: &str) -> Result<Value, String> {
        let v = self
            .api_get(&format!("/v1/activities/{activity_id}?include=tasks"))
            .await?;
        Ok(parse_release_state(&v))
    }

    // -- writes (FBF_SPEC.md §3) --------------------------------------------

    /// Set the rating for one (allocation, criterion): PATCH the existing
    /// rating when one is already there, else POST a new one.
    ///
    /// `dry_run = true` resolves and returns the would-be request WITHOUT
    /// sending (the lookup GET still happens — that is how it resolves).
    pub async fn upsert_rating(
        &self,
        allocation_id: &str,
        criterion_id: &str,
        option_id: &str,
        activity_id: &str,
        dry_run: bool,
    ) -> Result<Value, String> {
        let existing = self.ratings(allocation_id).await?;
        let existing = existing
            .iter()
            .find(|r| r.criterion_id == criterion_id)
            .cloned();
        // The API validates the FULL record on every write (a bare option-only
        // PATCH is a 422: criterion/parent/parent-type all "can't be empty"),
        // so both create and update mirror what the SPA serializes for
        // `createRecord("rating", {parent, course, allocation, criterion,
        // option})`.
        let course_id = self.course_id_of(activity_id).await?;
        let (method, url, payload) = match existing {
            Some(rating) => (
                "PATCH",
                format!("{}/v1/assignments/ratings/{}", self.api_base, rating.id),
                json!({
                    "data": {
                        "id": rating.id,
                        "type": "ratings",
                        "relationships": {
                            "allocation": {"data": {"id": allocation_id, "type": "assignments/allocation"}},
                            "criterion": {"data": {"id": criterion_id, "type": "assignments/rubric-criterion"}},
                            "option": {"data": {"id": option_id, "type": "assignments/criterion-option"}},
                            "parent": {"data": {"id": activity_id, "type": "activities"}},
                            "course": {"data": {"id": course_id, "type": "lti/course"}}
                        }
                    }
                }),
            ),
            None => (
                "POST",
                format!("{}/v1/assignments/ratings", self.api_base),
                json!({
                    "data": {
                        "type": "ratings",
                        "relationships": {
                            "allocation": {"data": {"id": allocation_id, "type": "assignments/allocation"}},
                            "criterion": {"data": {"id": criterion_id, "type": "assignments/rubric-criterion"}},
                            "option": {"data": {"id": option_id, "type": "assignments/criterion-option"}},
                            "parent": {"data": {"id": activity_id, "type": "activities"}},
                            "course": {"data": {"id": course_id, "type": "lti/course"}}
                        }
                    }
                }),
            ),
        };
        if dry_run {
            return Ok(json!({
                "dryRun": true,
                "method": method,
                "url": url,
                "payload": payload,
                "note": "dry run: nothing was sent",
            }));
        }
        let (status, response) = self.api_send(method, &url, &payload).await?;
        Ok(json!({
            "dryRun": false,
            "method": method,
            "url": url,
            "payload": payload,
            "status": status,
            "response": response,
        }))
    }

    /// FBF course uuid of an activity (`relationships.course.data.id`) — the
    /// SPA links every review it creates to the course as well.
    async fn course_id_of(&self, activity_id: &str) -> Result<String, String> {
        let v = self
            .api_get(&format!("/v1/activities/{activity_id}"))
            .await?;
        let data = v.get("data").unwrap_or(&v);
        rel_id(data, "course").ok_or_else(|| {
            format!("FeedbackFruits activity {activity_id} has no course relationship")
        })
    }

    /// Write the "Overall comments" reviews for one allocation. The task may
    /// require one written review PER CRITERION (criteria-count can exceed
    /// 1), so every criterion of the allocation receives `body`: existing
    /// reviews of mine are PATCHed to it, missing ones are POSTed. Payloads
    /// mirror the SPA's `createRecord("review", …)` — body + allocation,
    /// criterion, parent and course relationships, no `kind` attribute. The
    /// POST branch is live-verified; PATCH needs the full record too.
    pub async fn upsert_review(
        &self,
        allocation_id: &str,
        activity_id: &str,
        body: &str,
        improve_text: &str,
        dry_run: bool,
    ) -> Result<Value, String> {
        let mine: Vec<FbfReview> = self
            .reviews(allocation_id)
            .await?
            .into_iter()
            .filter(|r| r.user_id.as_deref() == Some(self.user_id.as_str()))
            .collect();
        let criteria = self.criteria(allocation_id).await?;
        if criteria.is_empty() {
            return Err(format!(
                "allocation {allocation_id} has no criteria; cannot attach a review"
            ));
        }
        let course_id = self.course_id_of(activity_id).await?;

        let improve_body = if improve_text.is_empty() {
            body.to_string()
        } else {
            improve_text.to_string()
        };
        let mut actions: Vec<Value> = Vec::new();
        for crit in &criteria {
            let body = if crit.title.to_lowercase().contains("improve") {
                &improve_body
            } else {
                body
            };
            let existing = mine
                .iter()
                .find(|r| r.criterion_id.as_deref() == Some(crit.id.as_str()));
            let (method, url, data) = match existing {
                Some(review) => (
                    "PATCH",
                    format!("{}/v1/assignments/reviews/{}", self.api_base, review.id),
                    json!({
                        "id": review.id,
                        "type": "reviews",
                        "attributes": {"body": body},
                        "relationships": {
                            "allocation": {"data": {"id": allocation_id, "type": "assignments/allocation"}},
                            "criterion": {"data": {"id": crit.id, "type": "assignments/rubric-criterion"}},
                            "parent": {"data": {"id": activity_id, "type": "activities"}},
                            "course": {"data": {"id": course_id, "type": "lti/course"}}
                        }
                    }),
                ),
                None => (
                    "POST",
                    format!("{}/v1/assignments/reviews", self.api_base),
                    json!({
                        "type": "reviews",
                        "attributes": {"body": body},
                        "relationships": {
                            "allocation": {"data": {"id": allocation_id, "type": "assignments/allocation"}},
                            "criterion": {"data": {"id": crit.id, "type": "assignments/rubric-criterion"}},
                            "parent": {"data": {"id": activity_id, "type": "activities"}},
                            "course": {"data": {"id": course_id, "type": "lti/course"}}
                        }
                    }),
                ),
            };
            actions.push(json!({
                "method": method,
                "url": url,
                "criterion": crit.title.clone(),
                "payload": {"data": data},
            }));
        }

        if dry_run {
            return Ok(json!({
                "dryRun": true,
                "actions": actions,
                "note": "dry run: nothing was sent",
            }));
        }

        let mut results: Vec<Value> = Vec::new();
        for action in &actions {
            let payload = action.get("payload").cloned().unwrap_or(Value::Null);
            let (status, response) = self
                .api_send(action["method"].as_str().unwrap_or("POST"), action["url"].as_str().unwrap_or_default(), &payload)
                .await?;
            results.push(json!({
                "method": action["method"],
                "url": action["url"],
                "criterion": action["criterion"],
                "status": status,
                "response": response,
            }));
        }
        Ok(json!({
            "dryRun": false,
            "actions": results,
        }))
    }
}


// ===========================================================================
// Tests — fixture-driven, no network (see FBF_SPEC.md fixtures)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/fbf/{name}",
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
        assert_eq!(action, "https://au-api.feedbackfruits.com/v1/lti/launch");
        assert!(id_token.starts_with("eyJ"), "id_token should be a JWT");
        assert!(!state.is_empty());
        // The form's id_token is a Blackboard-minted JWT: decode its claims.
        let claims = jwt_claims(&id_token).expect("id_token claims");
        assert_eq!(
            claims.get("sub").and_then(|v| v.as_str()),
            Some("7a5d06c5ec3d4627b7738c3a4f421593")
        );
        assert_eq!(claims.get("exp").and_then(|v| v.as_i64()), Some(1793977261));
    }

    #[test]
    fn launch_form_missing_fields_is_an_llm_readable_error() {
        let err = parse_launch_form("<html><body><p>Session Expired</p></body></html>")
            .expect_err("no form should fail");
        assert!(err.contains("auto-submit form"), "error: {err}");
        assert!(err.contains("ntulearn_login"), "error: {err}");
    }

    #[test]
    fn activity_id_from_landing_url_with_percent_encoded_course() {
        let url = reqwest::Url::parse(
            "https://au.feedbackfruits.com/courses/lti%2Fcourse/fee00823-f520-45e7-8b9a-381086efc6bb/activities/01b7f224-af0f-463b-9b28-63e721e7f37d?filter=x",
        )
        .expect("url");
        assert_eq!(
            activity_id_from_url(&url).as_deref(),
            Some("01b7f224-af0f-463b-9b28-63e721e7f37d")
        );
        let no_activity = reqwest::Url::parse("https://au.feedbackfruits.com/courses").expect("url");
        assert_eq!(activity_id_from_url(&no_activity), None);
    }

    #[test]
    fn api_url_meta_and_derived_base() {
        assert_eq!(
            parse_api_url_meta(r#"<html><head><meta name="api-url" content="https://api.au.feedbackfruits.com"></head></html>"#).as_deref(),
            Some("https://api.au.feedbackfruits.com")
        );
        assert_eq!(parse_api_url_meta("<html></html>"), None);
        let spa = reqwest::Url::parse("https://au.feedbackfruits.com/courses/x").expect("url");
        assert_eq!(
            derive_api_base(&spa).as_deref(),
            Some("https://api.au.feedbackfruits.com")
        );
        let api_host = reqwest::Url::parse("https://au-api.feedbackfruits.com/v1/lti/launch").expect("url");
        assert_eq!(
            derive_api_base(&api_host).as_deref(),
            Some("https://api.au.feedbackfruits.com")
        );
        let foreign = reqwest::Url::parse("https://example.com/activities/x").expect("url");
        assert_eq!(derive_api_base(&foreign), None);
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
    fn activity_fixture_parses() {
        let a = parse_activity(&fixture_json("activity.json")).expect("activity");
        assert_eq!(a.id, "01b7f224-af0f-463b-9b28-63e721e7f37d");
        assert_eq!(a.title, "Week 13 Final Peer Evaluation: FeedbackFruits");
        assert_eq!(
            a.progress_id.as_deref(),
            Some("a49113bf-43b0-4e62-9f8a-96d2f5644e1b")
        );
    }

    #[test]
    fn tasks_fixture_parses_in_activity_order() {
        let tasks = parse_tasks(&fixture_json("activity_tasks.json"));
        assert_eq!(tasks.len(), 6);
        assert_eq!(tasks[0].kind, "instruct-tasks");
        assert_eq!(tasks[0].title, None);
        let give = tasks.iter().find(|t| t.title.as_deref() == Some("Give ratings"))
            .expect("Give ratings task");
        assert_eq!(give.kind, "user-review-tasks");
        assert_eq!(give.deadline.as_deref(), Some("2026-11-17T01:00:00.000Z"));
        assert_eq!(give.reviewed_count, Some(5));
        assert_eq!(
            give.progress_id.as_deref(),
            Some("a6a8d10e-06d9-4f61-beeb-eeaaf690194e")
        );
        let comments = tasks
            .iter()
            .find(|t| t.title.as_deref() == Some("Overall comments"))
            .expect("Overall comments task");
        assert_eq!(comments.reviewed_count, Some(0));
        assert_eq!(comments.kind, "user-review-tasks");
        // ids match the activity's own relationship order
        assert_eq!(tasks[1].id, "05ea2270-a202-4dae-a342-f5af28fbef42");
        assert_eq!(tasks[5].id, "7a32e18d-3922-45fb-bf91-8cc301279209");
    }

    #[test]
    fn progress_fixture_is_100_percent() {
        assert_eq!(parse_progress(&fixture_json("progress_task.json")).expect("p"), 100);
    }

    #[test]
    fn allocations_given_fixture_yields_ratees() {
        let allocs = parse_allocations(&fixture_json("allocations_given.json"), "giver");
        assert_eq!(allocs.len(), 5);
        assert_eq!(
            allocs[0],
            (
                "c5b735de-7494-4347-8b16-4c68d4c30c7c".to_string(),
                "112f2114-677c-4950-bde9-6dc8d136b5ff".to_string()
            )
        );
        assert!(allocs.iter().all(|(_, ratee)| !ratee.is_empty()));
    }

    #[test]
    fn allocations_received_empty_until_published_is_normal() {
        let allocs = parse_allocations(&fixture_json("allocations_received.json"), "receiver");
        assert!(allocs.is_empty());
    }

    #[test]
    fn ratings_fixture_parses_criterion_and_option() {
        let ratings = parse_ratings(&fixture_json("ratings.json"));
        assert_eq!(ratings.len(), 4);
        assert_eq!(ratings[0].id, "6d206de6-e457-46ca-816d-5efce5bac75f");
        assert_eq!(ratings[0].criterion_id, "8d115789-ec88-4a47-b538-5a723b3bfe44");
        assert_eq!(ratings[0].option_id, "dab1e81a-63d3-400e-a2da-6fdab14836e5");
        // one rating per criterion, all on the same allocation
        let criteria: std::collections::HashSet<&str> =
            ratings.iter().map(|r| r.criterion_id.as_str()).collect();
        assert_eq!(criteria.len(), 4);
    }

    #[test]
    fn criteria_fixture_parses_options_and_weights() {
        let criteria = parse_criteria(&fixture_json("criteria.json"));
        assert_eq!(criteria.len(), 4);
        let managing = criteria
            .iter()
            .find(|c| c.title.starts_with("Managing interpersonal relationships"))
            .expect("first criterion");
        assert_eq!(managing.facet_weight, 30.0);
        assert_eq!(managing.option_ids.len(), 9);
        assert!(managing.option_ids.contains(&"dab1e81a-63d3-400e-a2da-6fdab14836e5".to_string()));
    }

    #[test]
    fn option_fixture_parses() {
        let o = parse_option(&fixture_json("option.json")).expect("option");
        assert_eq!(o.id, "e99105d4-219e-478c-bc1b-1447ee2fd77b");
        assert_eq!(o.title, "Never");
        assert_eq!(o.weight, 1);
        assert!(o.description.as_deref().is_some_and(|d| d.contains("Did not participate")));
    }

    #[test]
    fn user_fixture_name() {
        assert_eq!(
            parse_user_name(&fixture_json("user.json")).expect("name"),
            "LKCMedicine WONG YUET LENG, CHERYL"
        );
    }

    #[test]
    fn reviews_parse_from_synthetic_body() {
        // No captured reviews fixture exists yet (none were live at capture
        // time); the shape mirrors ratings.json's relationship structure.
        let v = json!({
            "data": [
                {
                    "id": "rev-1",
                    "type": "reviews",
                    "attributes": {"body": "Great teammate", "kind": "overall_comment"},
                    "relationships": {
                        "user": {"data": {"id": "a4fa313b-b6b6-476f-a496-8ed98934ae92", "type": "users"}},
                        "allocation": {"data": {"id": "alloc-1", "type": "assignments/allocation"}}
                    }
                }
            ]
        });
        let reviews = parse_reviews(&v);
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].body, "Great teammate");
        assert_eq!(reviews[0].kind.as_deref(), Some("overall_comment"));
        assert_eq!(reviews[0].user_id.as_deref(), Some("a4fa313b-b6b6-476f-a496-8ed98934ae92"));
    }

    #[test]
    fn release_state_from_tasks_fixture_is_all_unreleased() {
        let state = parse_release_state(&fixture_json("activity_tasks.json"));
        assert_eq!(state.get("gradesPublishedAt"), Some(&Value::Null));
        assert_eq!(state.get("ratingsPublishedAt"), Some(&Value::Null));
        assert_eq!(state.get("reviewsPublishedAt"), Some(&Value::Null));
        // and when the summative task says released, it surfaces verbatim
        let mut v = fixture_json("activity_tasks.json");
        for t in v.get_mut("included").unwrap().as_array_mut().unwrap() {
            if t["type"] == "summative-grade-tasks" {
                t["attributes"]["grades-published-at"] = json!("2026-12-01T00:00:00.000Z");
            }
        }
        let state = parse_release_state(&v);
        assert_eq!(
            state.get("gradesPublishedAt").and_then(|x| x.as_str()),
            Some("2026-12-01T00:00:00.000Z")
        );
    }

    // -- cookie jar ----------------------------------------------------------

    #[test]
    fn cookie_jar_domain_matching() {
        let mut jar = CookieJar::default();
        jar.ingest(
            "au-api.feedbackfruits.com",
            &[
                "access_token=abc; Domain=.feedbackfruits.com; Path=/; Secure".to_string(),
                "hostonly=xyz".to_string(),
            ],
        );
        jar.ingest("ntulearn.ntu.edu.sg", &["JSESSIONID=j1; Path=/".to_string()]);
        // Domain cookie matches subdomains of feedbackfruits.com
        let au = jar.pairs_for("api.au.feedbackfruits.com");
        assert!(au.iter().any(|p| p == "access_token=abc"), "{au:?}");
        assert!(!au.iter().any(|p| p == "hostonly=xyz"), "{au:?}");
        // host-only cookie matches only its exact host
        let api_host = jar.pairs_for("au-api.feedbackfruits.com");
        assert!(api_host.iter().any(|p| p == "access_token=abc"), "{api_host:?}");
        assert!(api_host.iter().any(|p| p == "hostonly=xyz"), "{api_host:?}");
        // NTULearn host sees only its own cookie
        let ntulearn = jar.pairs_for("ntulearn.ntu.edu.sg");
        assert!(ntulearn.iter().any(|p| p == "JSESSIONID=j1"), "{ntulearn:?}");
        assert!(!ntulearn.iter().any(|p| p.contains("access_token")), "{ntulearn:?}");
        // and by name anywhere
        assert_eq!(jar.get_anywhere("access_token").as_deref(), Some("abc"));
    }

    #[test]
    fn cookie_header_adds_bb_router_only_on_ntulearn_host() {
        let mut jar = CookieJar::default();
        jar.ingest(
            "au-api.feedbackfruits.com",
            &["access_token=abc; Domain=.feedbackfruits.com".to_string()],
        );
        let on_bb =
            cookie_header(&jar, "ntulearn.ntu.edu.sg", "ntulearn.ntu.edu.sg", "expires:1,id:x");
        assert_eq!(on_bb.as_deref(), Some("BbRouter=expires:1,id:x"));
        let on_fbf = cookie_header(&jar, "api.au.feedbackfruits.com", "ntulearn.ntu.edu.sg", "expires:1,id:x");
        assert_eq!(on_fbf.as_deref(), Some("access_token=abc"));
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
        let dir = std::env::temp_dir().join(format!("ntulearn-fbf-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(FBF_SESSION_FILE);

        // missing file -> empty default
        assert_eq!(load_cached(&path).activity_ids.len(), 0);

        let mut session = CachedSession {
            api_base: "https://api.au.feedbackfruits.com".to_string(),
            access_token: make_jwt(&format!(r#"{{"sub":"u1","exp":{}}}"#, now_secs() + 3600)),
            refresh_token: "rt".to_string(),
            api_session: "sess".to_string(),
            activity_ids: HashMap::new(),
        };
        session
            .activity_ids
            .insert("_6208502_1".to_string(), "01b7f224".to_string());
        save_cached(&path, &session);

        let loaded = load_cached(&path);
        assert_eq!(loaded.api_base, session.api_base);
        assert_eq!(loaded.access_token, session.access_token);
        assert_eq!(loaded.refresh_token, "rt");
        assert_eq!(loaded.api_session, "sess");
        assert_eq!(
            loaded.activity_ids.get("_6208502_1").map(String::as_str),
            Some("01b7f224")
        );
        // the cached token is a decodable JWT with the right claims
        assert_eq!(jwt_sub(&loaded.access_token).as_deref(), Some("u1"));

        // corrupt file -> empty default, not an error
        std::fs::write(&path, "{not json").expect("write");
        assert_eq!(load_cached(&path).activity_ids.len(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_cache_shape_is_stable() {
        // The on-disk keys are a persistence contract with older/newer
        // binaries of this crate — pin them.
        let session = CachedSession {
            api_base: "b".into(),
            access_token: "t".into(),
            refresh_token: "r".into(),
            api_session: "s".into(),
            activity_ids: HashMap::from([("c".to_string(), "a".to_string())]),
        };
        let text = serde_json::to_string(&session).expect("serialize");
        for key in [
            "api_base",
            "access_token",
            "refresh_token",
            "api_session",
            "activity_ids",
        ] {
            assert!(text.contains(&format!("\"{key}\"")), "missing {key} in {text}");
        }
    }
}
