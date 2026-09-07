#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ig;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};

/// Saved Instagram accounts. Cookies live in the app config dir, not the repo.
/// Instagram needs the whole jar (sessionid, csrftoken, ds_user_id, mid,
/// ig_did, ...) — sessionid alone gets a stripped page with no
/// full-resolution URLs.
#[derive(Serialize, Deserialize, Clone, Default)]
struct Account {
    username: String,
    cookies: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Accounts {
    active: String,
    accounts: Vec<Account>,
}

impl Accounts {
    fn active_cookies(&self) -> Option<String> {
        self.accounts
            .iter()
            .find(|a| a.username == self.active)
            .map(|a| a.cookies.clone())
    }

    /// Add or replace by username, and make it current.
    fn upsert(&mut self, username: String, cookies: String) {
        match self.accounts.iter_mut().find(|a| a.username == username) {
            Some(a) => a.cookies = cookies,
            None => self.accounts.push(Account {
                username: username.clone(),
                cookies,
            }),
        }
        self.active = username;
    }
}

struct Store {
    accounts: Mutex<Accounts>,
}

fn config_dir(app: &AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_config_dir()
        .unwrap_or_else(|_| PathBuf::from("."));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn accounts_path(app: &AppHandle) -> PathBuf {
    config_dir(app).join("accounts.json")
}

fn save_accounts(app: &AppHandle, acc: &Accounts) -> Result<(), String> {
    let json = serde_json::to_string_pretty(acc).map_err(|e| e.to_string())?;
    std::fs::write(accounts_path(app), json).map_err(|e| e.to_string())
}

/// Loads the account file, migrating a legacy single `cookies.txt` if present.
/// The migrated jar has no username until the first `auth_status` resolves one.
fn load_accounts(app: &AppHandle) -> Accounts {
    if let Ok(s) = std::fs::read_to_string(accounts_path(app)) {
        if let Ok(a) = serde_json::from_str::<Accounts>(&s) {
            return a;
        }
    }
    let legacy = config_dir(app).join("cookies.txt");
    if let Ok(c) = std::fs::read_to_string(&legacy) {
        let c = c.trim().to_string();
        if !c.is_empty() {
            return Accounts {
                active: "(unknown)".into(),
                accounts: vec![Account {
                    username: "(unknown)".into(),
                    cookies: c,
                }],
            };
        }
    }
    Accounts::default()
}

fn client(state: &State<Store>) -> Result<ig::Client, String> {
    let c = state
        .accounts
        .lock()
        .unwrap()
        .active_cookies()
        .unwrap_or_default();
    if c.is_empty() {
        return Err("Not signed in — add an account first.".into());
    }
    ig::Client::new(c).map_err(|e| e.to_string())
}

#[derive(Serialize, Clone)]
struct AuthState {
    signed_in: bool,
    username: String,
    detail: String,
    accounts: Vec<String>,
}

fn snapshot_of(a: &Accounts, signed_in: bool, detail: String) -> AuthState {
    AuthState {
        signed_in,
        username: a.active.clone(),
        detail,
        accounts: a.accounts.iter().map(|x| x.username.clone()).collect(),
    }
}

fn snapshot(state: &State<Store>, signed_in: bool, detail: String) -> AuthState {
    snapshot_of(&state.accounts.lock().unwrap(), signed_in, detail)
}

#[tauri::command]
async fn auth_status(app: AppHandle, state: State<'_, Store>) -> Result<AuthState, String> {
    eprintln!("[instagrab] auth_status called — frontend is live");
    let cookies = state.accounts.lock().unwrap().active_cookies();
    let Some(c) = cookies.filter(|c| !c.is_empty()) else {
        return Ok(snapshot(&state, false, "no account added yet".into()));
    };
    let cl = ig::Client::new(c).map_err(|e| e.to_string())?;
    match cl.whoami().await {
        Ok(name) => {
            // A migrated jar arrives unnamed; label it once we know who it is.
            let mut a = state.accounts.lock().unwrap();
            if a.active != name {
                let old = a.active.clone();
                if let Some(acct) = a.accounts.iter_mut().find(|x| x.username == old) {
                    acct.username = name.clone();
                }
                a.active = name.clone();
                let _ = save_accounts(&app, &a);
            }
            drop(a);
            Ok(snapshot(&state, true, format!("signed in as @{name}")))
        }
        Err(e) => Ok(snapshot(&state, false, format!("session invalid — {e}"))),
    }
}

#[tauri::command]
async fn list_accounts(state: State<'_, Store>) -> Result<AuthState, String> {
    Ok(snapshot(&state, true, String::new()))
}

#[tauri::command]
async fn switch_account(
    app: AppHandle,
    username: String,
    state: State<'_, Store>,
) -> Result<AuthState, String> {
    {
        let mut a = state.accounts.lock().unwrap();
        if !a.accounts.iter().any(|x| x.username == username) {
            return Err(format!("no stored account called @{username}"));
        }
        a.active = username;
        save_accounts(&app, &a)?;
    }
    auth_status(app, state).await
}

#[tauri::command]
async fn remove_account(
    app: AppHandle,
    username: String,
    state: State<'_, Store>,
) -> Result<AuthState, String> {
    {
        let mut a = state.accounts.lock().unwrap();
        a.accounts.retain(|x| x.username != username);
        if a.active == username {
            a.active = a
                .accounts
                .first()
                .map(|x| x.username.clone())
                .unwrap_or_default();
        }
        save_accounts(&app, &a)?;
    }
    auth_status(app, state).await
}

/// Opens a real Instagram login page in its own webview window. Browsing data is
/// cleared first — otherwise the window resumes the session that is already
/// signed in and you can never add a *different* account.
#[tauri::command]
async fn open_login(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("login") {
        let _ = w.set_focus();
        return Ok(());
    }
    if let Err(e) = app
        .webview_windows()
        .values()
        .next()
        .map_or(Ok(()), |w| w.clear_all_browsing_data())
    {
        eprintln!("[instagrab] could not clear webview data: {e}");
    }
    tauri::WebviewWindowBuilder::new(
        &app,
        "login",
        tauri::WebviewUrl::External("https://www.instagram.com/accounts/login/".parse().unwrap()),
    )
    .title("Sign in to Instagram")
    .inner_size(500.0, 760.0)
    .build()
    .map_err(|e| e.to_string())?;

    watch_for_login(app);
    Ok(())
}

/// Polls the login window until it holds a session that actually authenticates,
/// then stores the account and closes the window — so there's nothing to click.
///
/// A `sessionid` cookie alone isn't proof of a finished login: it can appear
/// mid-flow, before 2FA or a checkpoint completes. Requiring `whoami()` to
/// succeed is what makes this reliable, and it names the account at the same
/// time. Gives up after 5 minutes so the task can't leak.
fn watch_for_login(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        eprintln!("[instagrab] watching login window for a session…");
        for tick in 0..300 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;

            // The user closing the window is a cancellation.
            let Some(w) = app.get_webview_window("login") else {
                let _ = app.emit("login-cancelled", ());
                return;
            };
            let Ok(cookies) = w.cookies() else { continue };
            let jar: Vec<String> = cookies
                .iter()
                .filter(|c| c.domain().map(|d| d.contains("instagram")).unwrap_or(false))
                .map(|c| format!("{}={}", c.name(), c.value()))
                .collect();
            if tick % 10 == 0 {
                eprintln!(
                    "[instagrab] login poll {tick}s — {} instagram cookie(s)",
                    jar.len()
                );
            }
            if !jar.iter().any(|c| c.starts_with("sessionid=")) {
                continue;
            }
            let joined = jar.join("; ");
            let Ok(cl) = ig::Client::new(joined.clone()) else {
                continue;
            };
            let Ok(name) = cl.whoami().await else {
                eprintln!("[instagrab] sessionid present but not yet valid — still waiting");
                continue;
            };
            eprintln!("[instagrab] login detected: @{name}");

            let state = app.state::<Store>();
            let payload = {
                let mut a = state.accounts.lock().unwrap();
                a.upsert(name.clone(), joined);
                let _ = save_accounts(&app, &a);
                snapshot_of(&a, true, format!("signed in as @{name}"))
            };
            let _ = w.close();
            let _ = app.emit("account-added", payload);
            return;
        }
        let _ = app.emit("login-timeout", ());
    });
}

/// Reads the login window's cookie jar natively — `document.cookie` can't see
/// the httpOnly `sessionid`, so this has to come from the webview itself.
#[tauri::command]
async fn capture_login(app: AppHandle, state: State<'_, Store>) -> Result<AuthState, String> {
    let w = app
        .get_webview_window("login")
        .ok_or("The login window isn't open.")?;
    let cookies = w.cookies().map_err(|e| e.to_string())?;
    let jar: Vec<String> = cookies
        .iter()
        .filter(|c| c.domain().map(|d| d.contains("instagram")).unwrap_or(false))
        .map(|c| format!("{}={}", c.name(), c.value()))
        .collect();
    if !jar.iter().any(|c| c.starts_with("sessionid=")) {
        return Err("No session cookie yet — finish logging in, then press Done.".into());
    }
    let joined = jar.join("; ");

    // Name the account before storing it, so the switcher has something to show.
    let name = ig::Client::new(joined.clone())
        .map_err(|e| e.to_string())?
        .whoami()
        .await
        .map_err(|e| format!("captured cookies but couldn't identify the account: {e}"))?;
    {
        let mut a = state.accounts.lock().unwrap();
        a.upsert(name, joined);
        save_accounts(&app, &a)?;
    }
    let _ = w.close();
    auth_status(app, state).await
}

/// Fallback for when the in-app login is blocked: paste a cookie header copied
/// from a signed-in browser's devtools.
#[tauri::command]
async fn set_cookies(
    app: AppHandle,
    raw: String,
    state: State<'_, Store>,
) -> Result<AuthState, String> {
    let cleaned = raw.trim().trim_start_matches("Cookie:").trim().to_string();
    if !cleaned.contains("sessionid=") {
        return Err("That doesn't contain a sessionid= cookie.".into());
    }
    let name = ig::Client::new(cleaned.clone())
        .map_err(|e| e.to_string())?
        .whoami()
        .await
        .map_err(|e| format!("those cookies didn't authenticate: {e}"))?;
    {
        let mut a = state.accounts.lock().unwrap();
        a.upsert(name, cleaned);
        save_accounts(&app, &a)?;
    }
    auth_status(app, state).await
}

/// Where downloads go when the user hasn't picked a folder.
#[tauri::command]
fn default_dest(app: AppHandle) -> String {
    let d = app.path();
    eprintln!(
        "[instagrab] default_dest -> {:?}",
        d.download_dir().map(|p| p.to_string_lossy().to_string())
    );
    app.path()
        .download_dir()
        .or_else(|_| app.path().home_dir().map(|h| h.join("Downloads")))
        .unwrap_or_else(|_| PathBuf::from("."))
        .to_string_lossy()
        .to_string()
}

#[tauri::command]
async fn search_users(query: String, state: State<'_, Store>) -> Result<Vec<ig::UserHit>, String> {
    let cl = client(&state)?;
    cl.search_users(&query).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn fetch_post(url: String, state: State<'_, Store>) -> Result<ig::Post, String> {
    let cl = client(&state)?;
    let code = ig::shortcode_from_url(&url).map_err(|e| e.to_string())?;
    cl.post(&code).await.map_err(|e| e.to_string())
}

#[derive(Serialize, Clone)]
struct ScanProgress {
    fetched: usize,
    total: u64,
    done: bool,
}

/// Walks a profile's grid through the GraphQL connection the web client uses,
/// which is not subject to the `/api/v1/` profile action block. `limit` of 0
/// means everything. Paced deliberately — Instagram throttles fast pagination
/// hard. Only shortcodes come from here; the download step fetches each post
/// individually, exactly as the Single post tab does.
#[tauri::command]
async fn fetch_profile(
    app: AppHandle,
    username: String,
    limit: usize,
    state: State<'_, Store>,
) -> Result<Vec<ig::PostSummary>, String> {
    let cl = client(&state)?;
    let uname = ig::username_from_input(&username).map_err(|e| e.to_string())?;
    // Cosmetic ("n of N"); a miss just leaves the total blank.
    let total = cl.post_count(&uname).await.unwrap_or(0);

    let mut all: Vec<ig::PostSummary> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let (page, next) = cl
            .profile_page(&uname, cursor.as_deref())
            .await
            .map_err(|e| format!("{e:#}"))?;
        if page.is_empty() {
            break;
        }
        all.extend(page);
        let _ = app.emit(
            "scan-progress",
            ScanProgress {
                fetched: all.len(),
                total,
                done: false,
            },
        );
        if limit > 0 && all.len() >= limit {
            all.truncate(limit);
            break;
        }
        match next {
            Some(n) => cursor = Some(n),
            None => break,
        }
        tokio::time::sleep(std::time::Duration::from_millis(900)).await;
    }
    let _ = app.emit(
        "scan-progress",
        ScanProgress {
            fetched: all.len(),
            total,
            done: true,
        },
    );
    Ok(all)
}

#[derive(Deserialize)]
struct DownloadReq {
    shortcode: String,
    dest: String,
    /// 1-based carousel indices to keep; empty means all.
    only: Vec<usize>,
    /// Sub-folder per post — sensible when saving a whole profile.
    subfolder: bool,
}

#[derive(Serialize, Clone)]
struct DlProgress {
    file: String,
    done: usize,
    total: usize,
    bytes: u64,
    error: Option<String>,
}

fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect()
}

#[tauri::command]
async fn download_post(
    app: AppHandle,
    req: DownloadReq,
    state: State<'_, Store>,
) -> Result<usize, String> {
    let cl = client(&state)?;
    let post = cl.post(&req.shortcode).await.map_err(|e| e.to_string())?;

    let mut dir = PathBuf::from(&req.dest);
    if req.subfolder {
        dir = dir.join(sanitize(&post.shortcode));
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let wanted: Vec<&ig::MediaItem> = post
        .items
        .iter()
        .filter(|i| req.only.is_empty() || req.only.contains(&i.index))
        .collect();

    // Each carousel slot can yield two files (video + its cover frame).
    let total = wanted
        .iter()
        .map(|i| if i.video_url.is_some() { 2 } else { 1 })
        .sum();
    let mut done = 0usize;
    let mut written = 0usize;

    for it in wanted {
        let n = format!("{:02}", it.index);
        if let Some(v) = &it.video_url {
            let name = format!("{}_{}_video.mp4", post.shortcode, n);
            match cl.get_bytes(v).await {
                Ok(b) => {
                    std::fs::write(dir.join(&name), &b).map_err(|e| e.to_string())?;
                    written += 1;
                    done += 1;
                    let _ = app.emit(
                        "dl-progress",
                        DlProgress {
                            file: name,
                            done,
                            total,
                            bytes: b.len() as u64,
                            error: None,
                        },
                    );
                }
                Err(e) => {
                    done += 1;
                    let _ = app.emit(
                        "dl-progress",
                        DlProgress {
                            file: name,
                            done,
                            total,
                            bytes: 0,
                            error: Some(e.to_string()),
                        },
                    );
                }
            }
        }
        let tag = if it.video_url.is_some() {
            "cover"
        } else {
            "photo"
        };
        let name = format!(
            "{}_{}_{}_{}x{}.jpg",
            post.shortcode, n, tag, it.width, it.height
        );
        match cl.get_bytes(&it.image_url).await {
            Ok(b) => {
                std::fs::write(dir.join(&name), &b).map_err(|e| e.to_string())?;
                written += 1;
                done += 1;
                let _ = app.emit(
                    "dl-progress",
                    DlProgress {
                        file: name,
                        done,
                        total,
                        bytes: b.len() as u64,
                        error: None,
                    },
                );
            }
            Err(e) => {
                done += 1;
                let _ = app.emit(
                    "dl-progress",
                    DlProgress {
                        file: name,
                        done,
                        total,
                        bytes: 0,
                        error: Some(e.to_string()),
                    },
                );
            }
        }
    }

    // Written even with no caption — the URL alone is worth keeping, since a
    // folder of JPEGs is otherwise untraceable back to its post.
    let _ = std::fs::write(
        dir.join(format!("{}_caption.txt", post.shortcode)),
        ig::post_info(&post),
    );
    Ok(written)
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let accounts = load_accounts(app.handle());
            app.manage(Store {
                accounts: Mutex::new(accounts),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            auth_status,
            default_dest,
            search_users,
            list_accounts,
            switch_account,
            remove_account,
            open_login,
            capture_login,
            set_cookies,
            fetch_post,
            fetch_profile,
            download_post
        ])
        .run(tauri::generate_context!())
        .expect("error while running instagrab");
}
