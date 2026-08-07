# instagrab

A small Tauri desktop app for pulling images and videos out of Instagram posts —
single posts, or a whole profile — at the highest resolution Instagram will
actually give up.

Built after discovering that the obvious routes silently hand you downscaled
copies.

## Why it works the way it does

Instagram exposes the same media through several channels of differing quality,
and the convenient ones are lossy:

| Route | Gives you | Problem |
|---|---|---|
| `/api/v1/media/{id}/info/` | clean structure, captions, video URLs | stills capped at **720px** wide |
| `/api/v1/feed/user/{id}/` | paginated post list | same 720px cap |
| post HTML document | full-res CDN ladder (1440 / 1080 / 720 / 640) | no usable structure |

So the app takes **structure from the API** and **resolution from the HTML**,
matching the two on the numeric asset id that appears in every CDN URL.

Two things that are easy to get wrong:

- **`sessionid` alone is not enough — but a `csrftoken` beside it is.** Sending
  only the session cookie gets a stripped ~626KB page with none of the
  full-resolution URLs; the complete jar returns ~1.58MB including them. The
  minimum, though, is smaller than the full jar: one *unauthenticated* GET to
  `instagram.com` sets a `csrftoken`, and that plus `sessionid` is sufficient —
  verified, 948KB with 21 original-rung URLs. So a headless client can bootstrap
  itself from a single stored session cookie. The document request must also
  carry navigation-shaped headers (`Sec-Fetch-Dest: document`, a real
  User-Agent).
- **The size token in a CDN URL is a bounding box, not a dimension.** A
  1080×1350 still is tagged `s1080x1080`. It ranks candidates correctly but
  makes a terrible label — actual dimensions come from `original_width/height`.
- **An untagged URL is the original, so absence of a token ranks _highest_.**
  Instagram tags resized copies (`…_s1080x1080`) and leaves the original bare.
  Scoring a missing token as zero — the obvious reading — quietly selects a
  1080 copy while the 1440 original sits beside it in the same ladder, and the
  app still *labels* it 1440 because that comes from `original_width`. The live
  test decodes the downloaded JPEG's SOF header and asserts real pixels for
  exactly this reason; metadata agreed with itself while the bytes disagreed.

No browser or headless Chrome is involved in scraping. A webview is used *only*
for logging in, to capture the httpOnly session cookie that `document.cookie`
can't see.

## Running it

```bash
cargo tauri dev      # development
cargo tauri build    # -> src-tauri/target/release/bundle/
```

Builds on macOS, Windows and Linux. Prebuilt (unsigned) bundles for all three
are attached to each [release](../../releases).

**Single post** takes a full URL (`/p/`, `/reel/` or `/tv/`, query string and
all) **or just the bare code** — `ABCDEFGHIJK` on its own is enough.

**Whole profile** takes a username, `@username`, or a profile URL. If you don't
know the exact handle, search by name and pick from the results — each shows
whether the account is private, which decides whether you can see it at all.
Picking a result fills the field but doesn't start a scan; that stays a
deliberate second action.

Scanning lists the posts so you can choose; downloading then fetches each at
full resolution. Progress reports at two levels — files within the current
post, and posts within the batch — with a running byte count, because in
profile mode a single bar would be measuring whole posts and reading as stalled.

Downloads default to `~/Downloads` when no folder is chosen; **Choose folder…**
overrides it. Every post saves into its own `<shortcode>/` subfolder, since a
carousel can be a dozen-plus files. Alongside the media goes a
`<shortcode>_caption.txt` — written whether or not there's a caption, because
the post URL is the thing that makes a folder of JPEGs traceable later:

```
https://www.instagram.com/p/ABCDEFGHIJK/

account: @someaccount
posted:  2026-01-14 09:32 UTC
items:   13 (7 photo, 6 video)

---

<caption, if any>
```

### Accounts

The app holds several Instagram accounts and switches between them, because
what you can see — private accounts, follower-only posts — depends entirely on
who you are signed in as.

- **Add account** opens a real Instagram login window and captures the session
  automatically once you're through — nothing to click. Browsing data is cleared
  first, otherwise the window silently resumes the session already signed in and
  you can never add a *different* account.
  - Detection polls once a second for up to 5 minutes. It waits for `whoami()`
    to succeed rather than for a `sessionid` cookie to appear: the cookie can be
    set mid-flow, before 2FA or a checkpoint completes, so treating it as "done"
    would capture a session that doesn't work yet.
  - **Capture now** remains as a manual fallback if detection doesn't fire.
- The account is named by asking Instagram who the cookies belong to, via
  `accounts/edit/web_form_data/`. The obvious endpoints for this
  (`accounts/current_user/`, `users/{pk}/info/`) return 400 or non-JSON.
- **Paste cookies** takes a `Cookie:` header from a signed-in browser. This
  accepts a `Cookie:` header copied verbatim from devtools, or from any tool
  that extracts one from an installed browser — useful for an account already
  signed in inside Chrome, with no re-login required.
- **Forget** drops an account from this app only; Instagram is unaffected.

Accounts live in the OS config directory — on macOS
`~/Library/Application Support/space.emus.instagrab/`, on Linux
`~/.config/space.emus.instagrab/`, on Windows
`%APPDATA%\\space.emus.instagrab\\`. A legacy single `cookies.txt` is migrated
automatically on first run and named once the session resolves.

> The file holds live session cookies in plain text — anyone with read access to
> it can act as those accounts. It is outside the repo and gitignored.

## Tests

```bash
cargo test                                    # offline unit tests
# Live tests are skipped unless you supply what they need — nothing is
# hard-coded, so the suite carries no personal data.
IG_COOKIES="<a cookie header>" \
IG_TEST_POST=<shortcode of a public post> \
IG_TEST_USER=<a public username> \
  cargo test -- --ignored --nocapture
```

The live tests assert the things that actually regress: that a post comes back
above 720px wide (the cap the API alone would impose), that the labelled size
matches the pixels actually downloaded, and that profile pagination advances
without repeating posts.

## Known limits

- **Videos are served at whatever Instagram transcoded them to.** Sometimes
  that's the original; sometimes it isn't — one sampled post had 1080×1920
  originals with only 720×1280 offered. There is no higher-resolution route for
  video on the web; the app takes the largest `video_versions` entry.
- **Private accounts are genuinely out of reach.** If the signed-in account
  doesn't follow the author, Instagram 302s the post URL to the author's profile
  and there is no media to fetch. The app says so explicitly rather than failing
  obscurely. The only fix is to follow the account.
- **Two kinds of post code exist.** Classic 11-character shortcodes decode to a
  media id arithmetically; the newer 30-40 character share links do not, and
  their media id is read from the page's `instagram://media?id=` meta tag
  instead. Both are handled.
- Instagram changes this HTML without notice. Extraction deliberately matches
  loosely (CDN URL shape + asset id) rather than walking a fixed JSON path, so
  it degrades rather than shatters — but expect occasional maintenance.
- Profile scanning is paced at ~1 page/second and downloads at ~1 post/700ms.
  Full-resolution requires one page fetch per post, so a 165-post profile means
  165 requests. Going faster gets you rate-limited.
- Sessions last months, not forever. When the dot goes red, sign in again.
- Releases are **unsigned**: macOS needs right-click → Open the first time, and
  Windows shows a SmartScreen warning. Signing requires paid certificates.
- Windows and Linux builds compile, bundle and pass CI, but have had far less
  real-world use than the macOS ones.
