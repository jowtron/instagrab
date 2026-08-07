# instagrab — working notes

Context for anyone (or any agent) picking this up. The README explains what the
app does and why; this covers how to work on it without relearning the traps.

## Layout

```
src/                  frontend — plain HTML/CSS/JS, no bundler, no npm
  index.html          all markup; ids are the contract with main.js
  main.js             ES module; talks to Rust via window.__TAURI__
  styles.css          light/dark via prefers-color-scheme
src-tauri/
  src/ig.rs           all Instagram knowledge — the part worth reading
  src/main.rs         Tauri commands, account store, download orchestration
  tauri.conf.json     window, CSP, bundle targets
  capabilities/       Tauri permissions for the main and login windows
```

`ig.rs` is deliberately free of Tauri types, so it can be tested (and lifted)
on its own. Keep Instagram logic there and app plumbing in `main.rs`.

## Build and check

```bash
cargo tauri dev                 # run it
cargo tauri build               # bundle
cd src-tauri && cargo test      # 10 offline unit tests
cd src-tauri && cargo clippy --all-targets -- -D warnings
cd src-tauri && cargo fmt --check
```

CI enforces clippy-with-`-D warnings` and `fmt --check` on macOS, Windows and
Linux. Run all three locally before pushing; a stray warning fails the build.

## Things that will bite you

**`withGlobalTauri` must stay true** (`tauri.conf.json`). Without it
`window.__TAURI__` doesn't exist, the first line of `main.js` throws, and *every*
listener dies — including tab switching, which needs no backend. The app then
looks frozen rather than broken. `main.js` now detects this and writes a real
message into the status bar; keep that guard.

**A window on screen proves nothing.** Tauri shows the window whether or not
the frontend booted. The app logs `auth_status called — frontend is live` when
JS successfully calls in, so to verify a build actually works:

```bash
./src-tauri/target/release/bundle/macos/instagrab.app/Contents/MacOS/instagrab
```

and watch stderr. This is the only cheap end-to-end check there is.

**An untagged CDN URL is the original.** Instagram tags resized copies
(`…_s1080x1080`) and leaves the original bare, so a missing size token must rank
*highest* in `size_score`. Scoring it zero — the natural reading — silently
picks a 1080 copy while the 1440 original sits beside it in the same ladder,
and the app still labels it 1440 because that number comes from
`original_width`. Metadata agreed with itself while the bytes disagreed, which
is why the live test decodes the JPEG's SOF header rather than trusting labels.
If you touch resolution selection, keep that test honest.

**The size token is a bounding box, not a dimension.** A 1080×1350 still is
tagged `s1080x1080`. Fine for ranking, wrong as a label.

**Adding an account requires clearing browsing data first.** Otherwise the login
window resumes the session already signed in and you can only ever re-capture
the same account.

**Login detection waits for `whoami()`, not for a `sessionid` cookie.** The
cookie can appear mid-flow, before 2FA or a checkpoint finishes; capturing then
stores a session that doesn't work.

**`accounts/edit/web_form_data/` is how you learn who you are.** The obvious
endpoints (`accounts/current_user/`, `users/{pk}/info/`) return 400 or non-JSON.

**Bundle targets must stay `"all"`.** `["app", "dmg"]` is macOS-only and
produces no Windows or Linux artifacts, which fails the release job with the
unhelpful "No artifacts were found".

**Ubuntu 24.04**: `libappindicator3-dev` conflicts with
`libayatana-appindicator3-dev`. Install only the ayatana one.

## Tests and privacy

The repo is public and must stay free of personal data. Unit tests use
synthetic values (`BAAAAAAAAAA` → 2^60 — hand-checkable, not circular). Live
tests are `#[ignore]`d *and* skip unless their inputs are supplied:

```bash
IG_COOKIES="…" IG_TEST_POST=… IG_TEST_USER=… IG_TEST_HIDDEN_POST=… \
  cargo test -- --ignored --nocapture
```

Never hard-code a real account, shortcode or cookie. Live tests also need a
residential IP — see below.

## IP reputation

Instagram blocks datacentre IPs at the edge, before any client fingerprinting,
so this applies to plain HTTP exactly as it did to headless Chrome. Tested from
a VPS with a valid session: HTTP **200**, a full-size page, and **zero** CDN
URLs — a logged-out page dressed as success. Any code here that gets a page
with no CDN URLs should treat it as failure, not as "no images".

## Releasing

```bash
git tag -a vX.Y.Z -m "…" && git push origin vX.Y.Z
```

Builds macOS (both architectures), Windows and Linux, and drafts a release.
Publish with `gh release edit vX.Y.Z --draft=false`. Bundles are unsigned;
signing needs paid certificates.

Only the macOS builds have been run in anger. Windows and Linux compile, bundle
and pass CI, but treat their runtime behaviour as unverified.

## Deliberate non-goals

- **No npm/bundler.** The frontend is small; keep it dependency-free.
- **No browser for scraping.** A webview is used *only* to log in, because the
  httpOnly session cookie is invisible to `document.cookie`.
- **No richer alt text.** Instagram's ML descriptions are added client-side and
  aren't in the server-rendered HTML. Not worth a headless browser.
