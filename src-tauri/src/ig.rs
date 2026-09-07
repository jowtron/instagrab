//! Instagram media extraction.
//!
//! Two data sources, because neither alone is sufficient:
//!   * the private web API (`/api/v1/...`) gives clean structure — carousel
//!     layout, captions, video URLs — but caps still images at 720px wide;
//!   * the post's HTML document embeds the full-resolution CDN URLs
//!     (1440/1080/...), but carries no usable structure.
//!
//! So we take structure from the API and resolution from the HTML, matching the
//! two together on the numeric asset id present in every CDN URL.
//!
//! No browser is involved. The HTML route needs a complete cookie jar, not just
//! `sessionid` — with only the session cookie Instagram serves a stripped page
//! that omits the full-resolution URLs entirely.
//!
//! Whole profiles are a third route. The `/api/v1/` profile endpoints
//! (`users/web_profile_info/`, `feed/user/{id}/`) are action-blocked with
//! `feedback_required` once an account has enumerated a few profiles, and the
//! web client stopped using them: a profile page in a browser makes zero
//! `/api/v1/` calls and pages its grid through a persisted GraphQL query
//! instead. That query is what [`Client::profile_page`] speaks, and it keeps
//! answering for an account whose `/api/v1/` profile access is blocked. The
//! posts it lists are then fetched one at a time through the single-post route.

use anyhow::{anyhow, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

pub const APP_ID: &str = "936619743392459";

/// Persisted-query id of `PolarisProfilePostsTabContentQuery_connection`, the
/// GraphQL request the web client makes as you scroll a profile's grid.
///
/// If Instagram retires it, read the current one from the console of a
/// signed-in browser tab sitting on any profile page:
///
/// ```text
/// require("PolarisProfilePostsTabContentQuery_connection.graphql").params.id
/// ```
///
/// and check `.params.providedVariables` on the same object — every name
/// listed there must appear in `variables`, or the server answers only
/// "execution error" with no further detail.
pub const DOC_PROFILE_POSTS: &str = "39535953862670189";

/// Sent as `lsd` because the browser does; the server doesn't verify it.
const LSD: &str = "AVqbxe3tYU";
pub const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
                      AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Instagram's shortcode alphabet — base64url, used to derive a media id.
const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn re_asset() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"/(\d{8,})_").unwrap())
}

fn re_cdn() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"https://[^"\s\\]+?fbcdn\.net/v/t51[^"\s\\]+"#).unwrap())
}

/// Size hint baked into CDN URLs, e.g. `stp=dst-jpg_e35_s1440x1440`.
fn re_size() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"[sp](\d{3,4})x(\d{3,4})").unwrap())
}

fn re_shortcode() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"/(?:p|reel|tv)/([A-Za-z0-9_-]+)").unwrap())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaItem {
    pub index: usize,
    pub kind: String, // "photo" | "video"
    pub width: u32,
    pub height: u32,
    /// Best still: the post original for photos, the cover frame for videos.
    pub image_url: String,
    pub video_url: Option<String>,
    /// Small preview used by the picker UI.
    pub thumb_url: String,
    pub asset: String,
    /// Approximate bytes, from the CDN's Content-Length.
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Post {
    pub shortcode: String,
    pub owner: String,
    pub caption: String,
    pub taken_at: i64,
    pub items: Vec<MediaItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostSummary {
    pub shortcode: String,
    pub thumb_url: String,
    pub kind: String,
    pub item_count: usize,
    pub taken_at: i64,
    pub caption: String,
}

/// Accepts a full post/reel/tv URL, or a bare code pasted on its own.
///
/// The bare-code path is deliberately permissive: this field only ever means
/// "a post", so anything that *could* be a code is treated as one and a wrong
/// guess surfaces as a clear "post not found" from Instagram rather than a
/// parse error here. A `.` or `/` rules it out (usernames commonly contain
/// dots; paths contain slashes).
pub fn shortcode_from_url(input: &str) -> Result<String> {
    let s = input.trim();
    if let Some(c) = re_shortcode().captures(s) {
        return Ok(c[1].to_string());
    }
    let bare = s.trim_matches('/');
    let plausible = !bare.is_empty()
        && bare.len() >= 5
        && bare.len() <= 64
        && bare
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if plausible {
        return Ok(bare.to_string());
    }
    Err(anyhow!(
        "couldn't read a post from that — paste an Instagram post URL, or just its code"
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserHit {
    pub username: String,
    pub full_name: String,
    pub is_private: bool,
    pub is_verified: bool,
    pub profile_pic: String,
}

/// Accepts `name`, `@name`, or any profile URL, and returns the bare username.
///
/// A profile URL has the username as its first path segment, so anything with a
/// deeper path (`/p/…`, `/reel/…`) is a post, not a profile, and is rejected —
/// pasting a post link into the profile field is an easy mistake to make.
pub fn username_from_input(input: &str) -> Result<String> {
    let s = input.trim();
    if s.is_empty() {
        return Err(anyhow!("enter a username"));
    }
    if s.contains("instagram.com") {
        let after = s
            .split("instagram.com")
            .nth(1)
            .unwrap_or("")
            .split(['?', '#'])
            .next()
            .unwrap_or("");
        let segments: Vec<&str> = after.split('/').filter(|p| !p.is_empty()).collect();
        let first = segments
            .first()
            .ok_or_else(|| anyhow!("that URL has no username in it"))?;
        if matches!(
            *first,
            "p" | "reel" | "reels" | "tv" | "stories" | "explore"
        ) {
            return Err(anyhow!(
                "that's a post link, not a profile — use the Single post tab"
            ));
        }
        return Ok(first.trim_start_matches('@').to_string());
    }
    if s.contains('/') || s.contains(' ') {
        return Err(anyhow!("that doesn't look like a username or profile URL"));
    }
    Ok(s.trim_start_matches('@').to_string())
}

/// The sidecar text saved next to the media. Leads with the post URL, so a
/// folder of JPEGs stays traceable back to where it came from.
pub fn post_info(post: &Post) -> String {
    let vids = post.items.iter().filter(|i| i.kind == "video").count();
    let mut s = format!(
        "https://www.instagram.com/p/{}/\n\naccount: @{}\nposted:  {}\nitems:   {} ({} photo, {} video)\n",
        post.shortcode,
        post.owner,
        iso_utc(post.taken_at),
        post.items.len(),
        post.items.len() - vids,
        vids
    );
    if !post.caption.is_empty() {
        s.push_str("\n---\n\n");
        s.push_str(&post.caption);
        s.push('\n');
    }
    s
}

/// `taken_at` (unix seconds) as `YYYY-MM-DD HH:MM UTC`, without pulling in a
/// date crate. Civil-from-days per Howard Hinnant's algorithm.
pub fn iso_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} UTC",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60
    )
}

/// Shortcode -> media id (base64 over Instagram's alphabet). 11 chars * 6 bits
/// overflows u64, so this accumulates in u128.
pub fn media_id(shortcode: &str) -> Result<String> {
    // Classic shortcodes are 11 chars. Longer values are share links that encode
    // something else entirely — the caller resolves those from the page instead.
    if shortcode.len() > 11 {
        return Err(anyhow!(
            "{shortcode} is a long share link, not a classic shortcode — \
             resolve its media id from the post page"
        ));
    }
    let mut id: u128 = 0;
    for b in shortcode.bytes() {
        let v = ALPHABET
            .iter()
            .position(|&a| a == b)
            .ok_or_else(|| anyhow!("bad character {:?} in shortcode", b as char))?;
        id = id
            .checked_mul(64)
            .and_then(|x| x.checked_add(v as u128))
            .ok_or_else(|| anyhow!("shortcode too long"))?;
    }
    Ok(id.to_string())
}

/// Pull the numeric media id out of a post page. Instagram now issues long
/// share codes (30-40 chars) that can't be decoded arithmetically, so for those
/// the page itself is the only source. The `instagram://media?id=` app-link meta
/// tag is the most stable anchor; the JSON keys are fallbacks.
pub fn media_id_from_doc(unescaped: &str) -> Option<String> {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    let pats = R.get_or_init(|| {
        vec![
            Regex::new(r"instagram://media\?id=(\d{10,})").unwrap(),
            Regex::new(r#""media_id"\s*:\s*"?(\d{10,})"#).unwrap(),
            Regex::new(r#""pk"\s*:\s*"?(\d{15,})"#).unwrap(),
        ]
    });
    pats.iter()
        .find_map(|r| r.captures(unescaped).map(|c| c[1].to_string()))
}

/// Minimal percent-encoding for a search term (no url crate needed here).
fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn asset_of(url: &str) -> Option<String> {
    re_asset().captures(url).map(|c| c[1].to_string())
}

/// Rank a CDN URL within one asset's ladder.
///
/// Instagram tags *resized* copies with a bounding-box token (`…_s1080x1080`)
/// and leaves the **original untagged**. So an absent token means "original",
/// which must rank highest — ranking it lowest (the obvious reading) silently
/// selects a downscaled copy over the full-size image sitting right next to it.
fn size_score(url: &str) -> u64 {
    match re_size().captures(url) {
        Some(c) => {
            let w: u64 = c[1].parse().unwrap_or(0);
            let h: u64 = c[2].parse().unwrap_or(0);
            w * h
        }
        None => u64::MAX,
    }
}

/// One item of a timeline, whether it came from `/api/v1/feed/user/` or from
/// the GraphQL connection — the media object has the same shape in both.
fn summary_of(it: &serde_json::Value) -> Option<PostSummary> {
    let code = it["code"].as_str().unwrap_or_default().to_string();
    if code.is_empty() {
        return None;
    }
    let children = it["carousel_media"].as_array().cloned();
    let count = children.as_ref().map(|c| c.len()).unwrap_or(1);
    let first = children
        .as_ref()
        .and_then(|c| c.first().cloned())
        .unwrap_or_else(|| it.clone());
    let thumb = first
        .pointer("/image_versions2/candidates")
        .and_then(|c| c.as_array())
        .and_then(|c| c.last())
        .and_then(|c| c["url"].as_str())
        .unwrap_or_default()
        .to_string();
    let kind = match it["media_type"].as_i64().unwrap_or(1) {
        2 => "video",
        8 => "carousel",
        _ => "photo",
    };
    Some(PostSummary {
        shortcode: code,
        thumb_url: thumb,
        kind: kind.to_string(),
        item_count: count,
        taken_at: it["taken_at"].as_i64().unwrap_or(0),
        caption: it
            .pointer("/caption/text")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .chars()
            .take(300)
            .collect(),
    })
}

/// Reads one page out of a GraphQL timeline response: the summaries and the
/// cursor for the next page, if there is one.
pub fn timeline_from_gql(j: &serde_json::Value) -> Result<(Vec<PostSummary>, Option<String>)> {
    let conn = j
        .pointer("/data/xdt_api__v1__feed__user_timeline_graphql_connection")
        .filter(|c| !c.is_null())
        .ok_or_else(|| anyhow!("Instagram's reply had no timeline in it"))?;
    let out = conn["edges"]
        .as_array()
        .map(|edges| {
            edges
                .iter()
                .filter_map(|e| summary_of(&e["node"]))
                .collect()
        })
        .unwrap_or_default();
    let next = if conn.pointer("/page_info/has_next_page") == Some(&serde_json::Value::Bool(true)) {
        conn.pointer("/page_info/end_cursor")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        None
    };
    Ok((out, next))
}

/// Turns a GraphQL reply into either its JSON or a sentence about why it
/// failed. Failures arrive in three shapes: a `for (;;);`-prefixed envelope
/// from the outer platform (bad session), a GraphQL `errors` array (bad
/// query, unknown user), or an HTML page (rate limit, expired session). Left
/// undecoded, all three surface as reqwest's "error decoding response body",
/// which sends you hunting for a parsing bug that doesn't exist.
pub fn decode_graphql(status: u16, body: &str) -> Result<serde_json::Value> {
    let trimmed = body.trim_start_matches("for (;;);");
    let j: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(j) => j,
        Err(_) => return Err(describe_non_json(status, body)),
    };
    if let Some(summary) = j["errorSummary"].as_str() {
        let code = j["error"].as_i64().unwrap_or(0);
        return Err(if code == 1357001 {
            anyhow!(
                "Instagram doesn't accept this session for profile browsing \
                 (\"Log in to continue\"). Remove the account and sign in again."
            )
        } else {
            anyhow!("Instagram rejected the request: {summary} (error {code})")
        });
    }
    let usable = j["data"]
        .as_object()
        .map(|o| o.values().any(|v| !v.is_null()))
        .unwrap_or(false);
    if usable {
        return Ok(j);
    }
    let detail = j["errors"].as_array().and_then(|a| a.first()).map(|e| {
        e["description"]
            .as_str()
            .or(e["message"].as_str())
            .unwrap_or("unknown error")
            .to_string()
    });
    Err(match detail.as_deref() {
        Some(d) if d.contains("User lookup returned null") => {
            anyhow!("no such account — check the spelling")
        }
        Some(d) if d.contains("feedback_required") => blocked_message(),
        Some(d) => anyhow!("Instagram answered: {d}"),
        None => anyhow!("Instagram sent an empty reply (HTTP {status})"),
    })
}

/// What an `/api/v1/` or GraphQL call means when the body isn't JSON.
fn describe_non_json(status: u16, body: &str) -> anyhow::Error {
    let lower = body.to_ascii_lowercase();
    if lower.contains("feedback_required") || status == 429 {
        blocked_message()
    } else if lower.contains("/accounts/login/") || status == 401 || status == 403 {
        anyhow!("the session was rejected (HTTP {status}) — remove the account and sign in again")
    } else {
        anyhow!("Instagram answered HTTP {status} with something that isn't JSON")
    }
}

fn blocked_message() -> anyhow::Error {
    anyhow!(
        "Instagram has temporarily blocked this action for the signed-in account \
         (\"feedback_required\"). Single posts still work. The block usually clears \
         within a day on its own; retrying makes it last longer."
    )
}

/// The `/api/v1/` JSON envelope for an error is `{"message": "…", "status":
/// "fail"}`, and a `feedback_required` message is the action block. Anything
/// non-JSON goes through [`describe_non_json`].
pub fn decode_api(status: u16, body: &str) -> Result<serde_json::Value> {
    let j: serde_json::Value = match serde_json::from_str(body) {
        Ok(j) => j,
        Err(_) => return Err(describe_non_json(status, body)),
    };
    if j["status"].as_str() == Some("fail") || !(200..300).contains(&status) {
        let msg = j["message"].as_str().unwrap_or("");
        return Err(if msg == "feedback_required" {
            blocked_message()
        } else if msg == "login_required" {
            anyhow!("the session has expired — remove the account and sign in again")
        } else if msg.is_empty() {
            anyhow!("Instagram answered HTTP {status}")
        } else {
            anyhow!("Instagram answered: {msg} (HTTP {status})")
        });
    }
    Ok(j)
}

/// Post count from a profile page's `og:description` ("… 8,579 Posts - …").
/// Instagram rounds big numbers ("1.2K"), so a suffixed value is approximate.
pub fn count_from_og(desc: &str) -> Option<u64> {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"([\d][\d,\.]*)\s*([KkMm])?\s+[Pp]osts?\b").unwrap());
    let c = re.captures(desc)?;
    let digits = c[1].replace(',', "");
    let n: f64 = digits.parse().ok()?;
    let mult = match c.get(2).map(|m| m.as_str().to_ascii_uppercase()) {
        Some(s) if s == "K" => 1_000.0,
        Some(s) if s == "M" => 1_000_000.0,
        _ => 1.0,
    };
    Some((n * mult).round() as u64)
}

fn cookie_value<'a>(jar: &'a str, name: &str) -> Option<&'a str> {
    jar.split(';')
        .map(str::trim)
        .find_map(|kv| kv.strip_prefix(name)?.strip_prefix('='))
}

pub struct Client {
    http: reqwest::Client,
    cookies: String,
}

impl Client {
    pub fn new(cookies: String) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent(UA)
                .timeout(std::time::Duration::from_secs(60))
                .build()?,
            cookies,
        })
    }

    fn api_req(&self, url: &str) -> reqwest::RequestBuilder {
        self.http
            .get(url)
            .header("Cookie", &self.cookies)
            .header("X-IG-App-ID", APP_ID)
            .header("Accept-Language", "en-US,en;q=0.9")
    }

    /// A document request has to look like a real navigation or Instagram
    /// serves the stripped page without full-resolution URLs.
    fn doc_req(&self, url: &str) -> reqwest::RequestBuilder {
        self.http
            .get(url)
            .header("Cookie", &self.cookies)
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("Sec-Fetch-Dest", "document")
            .header("Sec-Fetch-Mode", "navigate")
            .header("Sec-Fetch-Site", "none")
            .header("Upgrade-Insecure-Requests", "1")
    }

    /// Which account this cookie jar belongs to. The obvious endpoints
    /// (`accounts/current_user/`, `users/{pk}/info/`) return 400 or non-JSON on
    /// the web API; the settings form is what reliably names the viewer.
    pub async fn whoami(&self) -> Result<String> {
        let r = self
            .api_req("https://www.instagram.com/api/v1/accounts/edit/web_form_data/")
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Referer", "https://www.instagram.com/accounts/edit/")
            .send()
            .await?;
        if !r.status().is_success() {
            return Err(anyhow!("not signed in (HTTP {})", r.status()));
        }
        let j: serde_json::Value = r.json().await.context("settings form was not JSON")?;
        j.pointer("/form_data/username")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("could not read the signed-in username"))
    }

    /// Typeahead over Instagram's own search.
    pub async fn search_users(&self, query: &str) -> Result<Vec<UserHit>> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(vec![]);
        }
        let url = format!(
            "https://www.instagram.com/api/v1/web/search/topsearch/?context=blended&query={}&count=8",
            urlencode(q)
        );
        let j = self.api_json(self.api_req(&url)).await?;
        let mut out = Vec::new();
        for entry in j["users"].as_array().cloned().unwrap_or_default() {
            // Results are wrapped in a `user` object.
            let u = entry.get("user").unwrap_or(&entry);
            let username = u["username"].as_str().unwrap_or_default().to_string();
            if username.is_empty() {
                continue;
            }
            out.push(UserHit {
                username,
                full_name: u["full_name"].as_str().unwrap_or_default().to_string(),
                is_private: u["is_private"].as_bool().unwrap_or(false),
                is_verified: u["is_verified"].as_bool().unwrap_or(false),
                profile_pic: u["profile_pic_url"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            });
        }
        Ok(out)
    }

    /// A `/api/v1/` GET, decoded with a status check so that an HTML wall or a
    /// `feedback_required` envelope reads as what it is.
    async fn api_json(&self, req: reqwest::RequestBuilder) -> Result<serde_json::Value> {
        let r = req.send().await?;
        let status = r.status().as_u16();
        let body = r.text().await?;
        decode_api(status, &body)
    }

    /// One persisted GraphQL query, as the web client sends it. `X-CSRFToken`
    /// is mandatory — without it the answer is a login page — and it has to
    /// match the `csrftoken` cookie, which is why the jar is required to hold
    /// one. Nothing else in the form is checked, but the browser's shape is
    /// kept anyway.
    async fn graphql(
        &self,
        doc_id: &str,
        name: &str,
        variables: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let csrf = cookie_value(&self.cookies, "csrftoken")
            .ok_or_else(|| anyhow!("the stored cookies have no csrftoken — sign in again"))?;
        let form = [
            ("av", "0"),
            ("__d", "www"),
            ("__user", "0"),
            ("__a", "1"),
            ("__req", "1"),
            ("dpr", "2"),
            ("__ccg", "GOOD"),
            ("lsd", LSD),
            ("fb_api_caller_class", "RelayModern"),
            ("fb_api_req_friendly_name", name),
            ("server_timestamps", "true"),
            ("doc_id", doc_id),
            ("variables", &variables.to_string()),
        ];
        let r = self
            .http
            .post("https://www.instagram.com/graphql/query")
            .header("Cookie", &self.cookies)
            .header("X-CSRFToken", csrf)
            .header("X-IG-App-ID", APP_ID)
            .header("X-FB-LSD", LSD)
            .header("X-FB-Friendly-Name", name)
            .header("Origin", "https://www.instagram.com")
            .header("Referer", "https://www.instagram.com/")
            .header("Accept", "*/*")
            .header("Accept-Language", "en-US,en;q=0.9")
            .form(&form)
            .send()
            .await?;
        let status = r.status().as_u16();
        let body = r.text().await?;
        decode_graphql(status, &body)
    }

    /// How many posts a profile has, read off its page's `og:description`.
    /// Best-effort — it only feeds the "n of N" while scanning, so a miss is
    /// `None`, not an error. (The JSON that used to carry this number is the
    /// action-blocked `web_profile_info`; the document still answers.)
    pub async fn post_count(&self, username: &str) -> Option<u64> {
        static R: OnceLock<Regex> = OnceLock::new();
        let re = R.get_or_init(|| {
            Regex::new(r#"<meta\s+property="og:description"\s+content="([^"]*)""#).unwrap()
        });
        let url = format!("https://www.instagram.com/{username}/");
        let r = self.doc_req(&url).send().await.ok()?;
        if !r.status().is_success() {
            return None;
        }
        let doc = r.text().await.ok()?;
        re.captures(&doc).and_then(|c| count_from_og(&c[1]))
    }

    /// One page of a profile's grid, newest first, through the GraphQL
    /// connection the web client scrolls with. `after` is the cursor returned
    /// with the previous page. Twelve per page, like the browser.
    pub async fn profile_page(
        &self,
        username: &str,
        after: Option<&str>,
    ) -> Result<(Vec<PostSummary>, Option<String>)> {
        let vars = serde_json::json!({
            "after": after,
            "before": null,
            "data": {
                "count": 12,
                "include_reel_media_seen_timestamp": true,
                "include_relationship_info": true,
                "latest_besties_reel_media": true,
                "latest_reel_media": true,
            },
            "first": 12,
            "last": null,
            "username": username,
            "include_multi_captions": false,
            "__relay_internal__pv__PolarisMultiCaptionCarouselEnabledrelayprovider": false,
            "__relay_internal__pv__PolarisShortDramaEnabledrelayprovider": false,
            "__relay_internal__pv__PolarisReelsRecoDebugOverlayEnabledrelayprovider": false,
        });
        let j = self
            .graphql(
                DOC_PROFILE_POSTS,
                "PolarisProfilePostsTabContentQuery_connection",
                vars,
            )
            .await
            .with_context(|| format!("listing posts for @{username}"))?;
        timeline_from_gql(&j)
    }

    /// Full-resolution CDN URLs from a post's HTML, keyed by asset id.
    /// Only the best rung of each asset's ladder is kept.
    /// Fetch a post's HTML. Instagram answers with a 302 to the author's profile
    /// when the viewer isn't permitted to see the post, so a final URL that is no
    /// longer a post URL means "no access" rather than "no such post".
    async fn post_doc(&self, shortcode: &str) -> Result<String> {
        let url = format!("https://www.instagram.com/p/{shortcode}/");
        let r = self.doc_req(&url).send().await?;
        let landed = r.url().clone();
        if !landed.path().contains("/p/") {
            let who = landed.path().trim_matches('/');
            return Err(if who.is_empty() {
                anyhow!("Instagram redirected away from the post — the session may have expired.")
            } else {
                anyhow!(
                    "This post isn't visible to the signed-in account. Instagram \
                     redirected to @{who}, which normally means the account is \
                     private and isn't followed by the account you're signed in as \
                     (it can also mean the post was deleted)."
                )
            });
        }
        Ok(r.text().await?)
    }

    /// The embedded JSON is backslash-escaped inside `<script>` blocks.
    fn unescape(body: &str) -> String {
        body.replace("\\u0025", "%")
            .replace("\\/", "/")
            .replace("\\\"", "\"")
            .replace("&amp;", "&")
    }

    /// Best CDN URL per asset id, taken from an already-unescaped document.
    fn assets_from_doc(un: &str) -> std::collections::HashMap<String, String> {
        let mut best: std::collections::HashMap<String, (u64, String)> =
            std::collections::HashMap::new();
        for m in re_cdn().find_iter(un) {
            let u = m.as_str().to_string();
            let Some(a) = asset_of(&u) else { continue };
            let score = size_score(&u);
            let e = best.entry(a).or_insert((0, String::new()));
            if score >= e.0 {
                *e = (score, u);
            }
        }
        best.into_iter().map(|(k, (_, v))| (k, v)).collect()
    }

    /// Everything in one post, at the best resolution obtainable.
    pub async fn post(&self, shortcode: &str) -> Result<Post> {
        // One document fetch does double duty: it carries the full-resolution
        // URLs, and the media id for share codes too long to decode.
        let doc = self.post_doc(shortcode).await?;
        let un = Self::unescape(&doc);

        let mid = match media_id(shortcode) {
            Ok(m) => m,
            Err(_) => media_id_from_doc(&un)
                .ok_or_else(|| anyhow!("couldn't find a media id on the page for {shortcode}"))?,
        };

        let url = format!("https://www.instagram.com/api/v1/media/{mid}/info/");
        let j = self
            .api_json(self.api_req(&url))
            .await
            .context("reading the post's media info")?;
        let post = j["items"]
            .as_array()
            .and_then(|a| a.first())
            .cloned()
            .ok_or_else(|| anyhow!("post not found (private, deleted, or session expired)"))?;

        let hi = Self::assets_from_doc(&un);

        let children = post["carousel_media"]
            .as_array()
            .cloned()
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| vec![post.clone()]);

        let mut items = Vec::new();
        for (i, k) in children.iter().enumerate() {
            let cands = k
                .pointer("/image_versions2/candidates")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default();
            let api_best = cands.first().and_then(|c| c["url"].as_str()).unwrap_or("");
            let thumb = cands
                .last()
                .and_then(|c| c["url"].as_str())
                .unwrap_or(api_best)
                .to_string();
            let Some(asset) = asset_of(api_best) else {
                continue;
            };

            // Prefer the HTML URL — the API tops out at 720 wide.
            let image_url = hi
                .get(&asset)
                .cloned()
                .unwrap_or_else(|| api_best.to_string());

            let videos = k["video_versions"].as_array().cloned().unwrap_or_default();
            let video_url = videos
                .iter()
                .max_by_key(|v| {
                    v["width"].as_u64().unwrap_or(0) * v["height"].as_u64().unwrap_or(0)
                })
                .and_then(|v| v["url"].as_str())
                .map(|s| s.to_string());

            // `original_width/height` are the true pixel dimensions. The size
            // token in the URL (`s1080x1080`) is only a bounding box the image
            // was fitted into, so a 1080x1350 still carries an `s1080x1080`
            // tag — good enough to rank candidates by, wrong as a label.
            let (ow, oh) = (
                k["original_width"].as_u64().unwrap_or(0) as u32,
                k["original_height"].as_u64().unwrap_or(0) as u32,
            );
            let (w, h) = if ow > 0 && oh > 0 {
                (ow, oh)
            } else if let Some(c) = re_size().captures(&image_url) {
                (c[1].parse().unwrap_or(0), c[2].parse().unwrap_or(0))
            } else {
                (0, 0)
            };

            items.push(MediaItem {
                index: i + 1,
                kind: if video_url.is_some() {
                    "video"
                } else {
                    "photo"
                }
                .to_string(),
                width: w,
                height: h,
                image_url,
                video_url,
                thumb_url: thumb,
                asset,
                bytes: 0,
            });
        }

        Ok(Post {
            shortcode: shortcode.to_string(),
            owner: post
                .pointer("/user/username")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
            caption: post
                .pointer("/caption/text")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            taken_at: post["taken_at"].as_i64().unwrap_or(0),
            items,
        })
    }

    /// Fetch bytes for one media URL.
    pub async fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let r = self
            .http
            .get(url)
            .header("Cookie", &self.cookies)
            .send()
            .await?;
        if !r.status().is_success() {
            return Err(anyhow!("CDN returned {}", r.status()));
        }
        Ok(r.bytes().await?.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcode_to_media_id() {
        // "B" is index 1, followed by ten zeros, so this is 1 * 64^10 = 2^60.
        assert_eq!(media_id("BAAAAAAAAAA").unwrap(), "1152921504606846976");
        assert_eq!(media_id("AAAAAAAAAAA").unwrap(), "0");
        assert!(media_id("not a shortcode!").is_err());
    }

    #[test]
    fn parses_post_urls() {
        for u in [
            "https://www.instagram.com/p/ABCDEFGHIJK/?utm_source=ig_web_copy_link",
            "https://instagram.com/reel/ABCDEFGHIJK/",
            "https://www.instagram.com/tv/ABCDEFGHIJK",
        ] {
            assert_eq!(shortcode_from_url(u).unwrap(), "ABCDEFGHIJK");
        }
    }

    #[test]
    fn accepts_a_bare_code() {
        for s in ["ABCDEFGHIJK", "  ABCDEFGHIJK  "] {
            assert_eq!(shortcode_from_url(s).unwrap(), s.trim());
        }
        for s in ["", "   ", "https://example.com/some/path", "a b c"] {
            assert!(shortcode_from_url(s).is_err(), "should reject {s:?}");
        }
    }

    #[test]
    fn reads_usernames_from_urls_and_bare_input() {
        for s in [
            "https://www.instagram.com/someuser/",
            "https://instagram.com/someuser",
            "https://www.instagram.com/someuser/?hl=en",
            "someuser",
            "@someuser",
            "  someuser  ",
        ] {
            assert_eq!(username_from_input(s).unwrap(), "someuser", "input {s:?}");
        }
        // A post link in the profile field is a common slip; say so clearly.
        let err = username_from_input("https://www.instagram.com/p/ABCDEFGHIJK/")
            .unwrap_err()
            .to_string();
        assert!(err.contains("post link"), "unhelpful: {err}");
        assert!(username_from_input("").is_err());
        assert!(username_from_input("two words").is_err());
    }

    #[test]
    fn long_share_codes_take_the_document_route() {
        let long = "A".repeat(39);
        assert_eq!(
            shortcode_from_url(&format!("https://www.instagram.com/p/{long}/?img_index=2"))
                .unwrap(),
            long
        );
        let err = media_id(&long).unwrap_err().to_string();
        assert!(err.contains("long share link"), "unhelpful error: {err}");
    }

    #[test]
    fn reads_media_id_from_a_page() {
        let meta = r#"<meta content="instagram://media?id=1152921504606846976" />"#;
        assert_eq!(media_id_from_doc(meta).unwrap(), "1152921504606846976");
        let json = r#"{"media_id":"1152921504606846976","x":1}"#;
        assert_eq!(media_id_from_doc(json).unwrap(), "1152921504606846976");
        assert_eq!(media_id_from_doc("<html>nothing here</html>"), None);
        assert_eq!(
            media_id("BAAAAAAAAAA").unwrap(),
            media_id_from_doc(meta).unwrap()
        );
    }

    #[test]
    fn formats_utc_dates() {
        assert_eq!(iso_utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(iso_utc(1_700_000_000), "2023-11-14 22:13 UTC");
        assert_eq!(iso_utc(1_000_000_000), "2001-09-09 01:46 UTC");
    }

    #[test]
    fn ranks_by_size_hint() {
        let small = "https://x.fbcdn.net/v/t51/1_n.jpg?stp=dst-jpg_e35_s720x720";
        let big = "https://x.fbcdn.net/v/t51/1_n.jpg?stp=dst-jpg_e35_s1440x1440";
        assert!(size_score(big) > size_score(small));
        // An untagged URL is the untouched original and must outrank every
        // resized copy — getting this backwards silently downloads a smaller
        // image while the full-size one sits beside it in the same ladder.
        let original = "https://x.fbcdn.net/v/t51/1_n.jpg";
        assert!(size_score(original) > size_score(big));
    }

    #[test]
    fn extracts_asset_ids() {
        assert_eq!(
            asset_of("https://i.fbcdn.net/v/t51.82787-15/123456789_18614027074005908_1.jpg"),
            Some("123456789".to_string())
        );
        assert_eq!(
            asset_of("https://i.fbcdn.net/v/t51/no-asset-here.jpg"),
            None
        );
    }

    #[test]
    fn encodes_search_terms() {
        assert_eq!(urlencode("plain"), "plain");
        assert_eq!(urlencode("two words"), "two%20words");
        assert_eq!(urlencode("a/b?c"), "a%2Fb%3Fc");
    }

    /// A GraphQL timeline page has the `/api/v1/` media shape under
    /// `edges[].node`; this is the exact nesting the server sends, with
    /// synthetic values.
    fn timeline_fixture(has_next: bool) -> serde_json::Value {
        serde_json::json!({
            "data": {
                "xdt_api__v1__feed__user_timeline_graphql_connection": {
                    "edges": [
                        {"node": {
                            "code": "AAAAAAAAAAB",
                            "media_type": 8,
                            "taken_at": 1_700_000_000,
                            "caption": {"text": "first"},
                            "carousel_media": [
                                {"image_versions2": {"candidates": [
                                    {"url": "https://cdn/big", "width": 1080},
                                    {"url": "https://cdn/small", "width": 150}
                                ]}},
                                {"image_versions2": {"candidates": []}}
                            ]
                        }},
                        {"node": {
                            "code": "AAAAAAAAAAC",
                            "media_type": 2,
                            "taken_at": 1_600_000_000,
                            "image_versions2": {"candidates": [{"url": "https://cdn/v"}]}
                        }},
                        {"node": {"media_type": 1}}
                    ],
                    "page_info": {
                        "has_next_page": has_next,
                        "end_cursor": "1234567890_42"
                    }
                },
                "xdt_viewer": {"user": {}}
            },
            "status": "ok"
        })
    }

    #[test]
    fn reads_a_graphql_timeline_page() {
        let (posts, next) = timeline_from_gql(&timeline_fixture(true)).unwrap();
        // The node without a code is dropped rather than producing a blank card.
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0].shortcode, "AAAAAAAAAAB");
        assert_eq!(posts[0].kind, "carousel");
        assert_eq!(posts[0].item_count, 2);
        assert_eq!(posts[0].thumb_url, "https://cdn/small");
        assert_eq!(posts[0].caption, "first");
        assert_eq!(posts[1].kind, "video");
        assert_eq!(posts[1].item_count, 1);
        assert_eq!(next.as_deref(), Some("1234567890_42"));

        let (_, next) = timeline_from_gql(&timeline_fixture(false)).unwrap();
        assert!(next.is_none(), "a last page must not hand back a cursor");

        let err = timeline_from_gql(&serde_json::json!({"data": {}}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no timeline"), "unhelpful: {err}");
    }

    #[test]
    fn explains_graphql_failures() {
        // Outer-platform envelope: a session GraphQL won't accept.
        let e = decode_graphql(
            200,
            r#"for (;;);{"__ar":1,"error":1357001,"errorSummary":"Log in to continue"}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("sign in again"), "{e}");

        // GraphQL `errors` with a null data: unknown user.
        let e = decode_graphql(
            200,
            r#"{"errors":[{"message":"execution error","description":"User lookup returned null"}],"data":null}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("no such account"), "{e}");

        // Every root field null counts as a failure even with no `errors`.
        let e = decode_graphql(200, r#"{"data":{"xig_user_by_username":null}}"#)
            .unwrap_err()
            .to_string();
        assert!(e.contains("empty reply"), "{e}");

        // An HTML wall is the action block, not a parsing bug.
        let e = decode_graphql(429, "<!DOCTYPE html><html>Please wait a few minutes</html>")
            .unwrap_err()
            .to_string();
        assert!(e.contains("temporarily blocked"), "{e}");

        assert!(decode_graphql(200, &timeline_fixture(true).to_string()).is_ok());
    }

    #[test]
    fn explains_api_failures() {
        let e = decode_api(
            400,
            r#"{"message":"feedback_required","status":"fail","feedback_title":"Try Again Later"}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("temporarily blocked"), "{e}");
        let e = decode_api(429, "<html>Page Not Found</html>")
            .unwrap_err()
            .to_string();
        assert!(e.contains("temporarily blocked"), "{e}");
        let e = decode_api(200, r#"{"message":"login_required","status":"fail"}"#)
            .unwrap_err()
            .to_string();
        assert!(e.contains("expired"), "{e}");
        assert!(decode_api(200, r#"{"items":[],"status":"ok"}"#).is_ok());
    }

    #[test]
    fn counts_posts_from_og_description() {
        assert_eq!(
            count_from_og("686M Followers, 286 Following, 8,579 Posts - See Instagram photos"),
            Some(8579)
        );
        assert_eq!(
            count_from_og("12 Followers, 3 Following, 1 Post - x"),
            Some(1)
        );
        assert_eq!(count_from_og("1.2K Posts"), Some(1200));
        assert_eq!(count_from_og("0 Posts"), Some(0));
        assert_eq!(count_from_og("nothing to see here"), None);
    }

    #[test]
    fn reads_cookie_values() {
        let jar = "mid=abc; csrftoken=tok-1; sessionid=s%3Ax";
        assert_eq!(cookie_value(jar, "csrftoken"), Some("tok-1"));
        assert_eq!(cookie_value(jar, "sessionid"), Some("s%3Ax"));
        assert_eq!(cookie_value(jar, "token"), None);
    }
}

#[cfg(test)]
mod live {
    //! Network tests against the real Instagram. Ignored by default, and each
    //! is skipped unless the account/post it needs is supplied — nothing here
    //! is hard-coded, so the suite carries no personal data.
    //!
    //!   IG_COOKIES="…" \
    //!   IG_TEST_POST=<shortcode of a public post you can see> \
    //!   IG_TEST_USER=<a public username> \
    //!   IG_TEST_HIDDEN_POST=<a post you are NOT allowed to view> \
    //!     cargo test -- --ignored --nocapture
    use super::*;

    fn client() -> Option<Client> {
        let c = std::env::var("IG_COOKIES").ok()?;
        Client::new(c).ok()
    }

    fn var(name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }

    /// Read real dimensions out of JPEG bytes, so the test trusts pixels rather
    /// than any label the app or Instagram attached.
    fn jpeg_dims(b: &[u8]) -> Option<(u32, u32)> {
        let mut i = 2; // skip SOI
        while i + 9 < b.len() {
            if b[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = b[i + 1];
            let len = ((b[i + 2] as usize) << 8) | b[i + 3] as usize;
            if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC
            {
                let h = ((b[i + 5] as u32) << 8) | b[i + 6] as u32;
                let w = ((b[i + 7] as u32) << 8) | b[i + 8] as u32;
                return Some((w, h));
            }
            i += 2 + len;
        }
        None
    }

    #[tokio::test]
    #[ignore]
    async fn session_authenticates() {
        let Some(c) = client() else {
            return eprintln!("skipped: IG_COOKIES unset");
        };
        // whoami() both proves the session works and names the account.
        println!("signed in as @{}", c.whoami().await.unwrap());
    }

    #[tokio::test]
    #[ignore]
    async fn fetches_a_post_at_full_resolution() {
        let Some(c) = client() else {
            return eprintln!("skipped: IG_COOKIES unset");
        };
        let Some(code) = var("IG_TEST_POST") else {
            return eprintln!("skipped: IG_TEST_POST unset");
        };
        let p = c.post(&code).await.unwrap();
        println!("@{} — {} item(s)", p.owner, p.items.len());
        assert!(!p.items.is_empty());

        let first = p.items.first().unwrap();
        let bytes = c.get_bytes(&first.image_url).await.unwrap();
        // The API alone caps stills at 720 wide; the HTML route must beat that.
        // Decode the bytes rather than trusting the label.
        if let Some((w, h)) = jpeg_dims(&bytes) {
            println!(
                "labelled {}x{}, downloaded {w}x{h}",
                first.width, first.height
            );
            assert_eq!(
                (w, h),
                (first.width, first.height),
                "label disagrees with pixels"
            );
            assert!(w > 720, "only got {w}px — a downscaled copy was selected");
        }
        assert!(post_info(&p).starts_with("https://www.instagram.com/p/"));
    }

    #[tokio::test]
    #[ignore]
    async fn paginates_a_profile() {
        let Some(c) = client() else {
            return eprintln!("skipped: IG_COOKIES unset");
        };
        let Some(user) = var("IG_TEST_USER") else {
            return eprintln!("skipped: IG_TEST_USER unset");
        };
        let total = c.post_count(&user).await;
        println!("@{user} total={total:?}");
        let (page1, cur) = c.profile_page(&user, None).await.unwrap();
        assert!(!page1.is_empty());
        assert!(page1.iter().all(|p| !p.shortcode.is_empty()));
        let Some(cur) = cur else {
            return println!("only one page; nothing to paginate");
        };
        let (page2, _) = c.profile_page(&user, Some(&cur)).await.unwrap();
        let overlap = page1
            .iter()
            .filter(|a| page2.iter().any(|b| b.shortcode == a.shortcode))
            .count();
        println!(
            "page1={} page2={} overlap={}",
            page1.len(),
            page2.len(),
            overlap
        );
        assert_eq!(overlap, 0, "pagination repeated posts");
    }

    #[tokio::test]
    #[ignore]
    async fn hidden_post_explains_itself() {
        let Some(c) = client() else {
            return eprintln!("skipped: IG_COOKIES unset");
        };
        let Some(code) = var("IG_TEST_HIDDEN_POST") else {
            return eprintln!("skipped: IG_TEST_HIDDEN_POST unset");
        };
        let err = c.post(&code).await.unwrap_err().to_string();
        println!("error surfaced: {err}");
        assert!(err.contains("private"), "unhelpful error: {err}");
        assert!(
            !err.contains("too long"),
            "leaked the shortcode-length error: {err}"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn searches_for_accounts() {
        let Some(c) = client() else {
            return eprintln!("skipped: IG_COOKIES unset");
        };
        let Some(user) = var("IG_TEST_USER") else {
            return eprintln!("skipped: IG_TEST_USER unset");
        };
        let hits = c.search_users(&user).await.unwrap();
        println!(
            "{} hit(s); first: {:?}",
            hits.len(),
            hits.first().map(|h| &h.username)
        );
        assert!(hits.iter().any(|h| h.username.eq_ignore_ascii_case(&user)));
    }
}
