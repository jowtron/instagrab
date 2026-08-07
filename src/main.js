const $ = (id) => document.getElementById(id);

// A hard failure here used to kill every listener on the page — including tab
// switching, which needs no backend at all — leaving the UI looking "locked"
// with no clue why. Surface it instead.
function fatal(msg) {
  const el = $("status");
  if (el) {
    el.textContent = msg;
    el.className = "status err";
    el.classList.remove("hidden");
  }
  console.error(msg);
}

// Requires `app.withGlobalTauri: true` in tauri.conf.json; without it this
// object does not exist and the whole frontend is inert.
const api = window.__TAURI__;
if (!api) {
  fatal(
    "Tauri API unavailable — the app shell did not inject window.__TAURI__. " +
      "Check that withGlobalTauri is enabled, then rebuild."
  );
}
const invoke = api ? api.core.invoke : async () => { throw new Error("Tauri API unavailable"); };
const listen = api ? api.event.listen : async () => {};
const open = api ? api.dialog.open : async () => null;
const state = {
  mode: "post",
  dest: null,
  post: null,     // single-post view
  posts: [],      // profile view
  picked: new Set(),
};

/* ---------- auth ---------- */

function paintAuth(s) {
  $("auth-dot").className = "dot " + (s.signed_in ? "on" : "off");
  $("auth-text").textContent = s.signed_in ? "" : s.detail;
  $("btn-forget").classList.toggle("hidden", !s.accounts.length);

  const pick = $("account-picker");
  pick.innerHTML = "";
  s.accounts.forEach((name) => {
    const o = document.createElement("option");
    o.value = name;
    o.textContent = "@" + name;
    o.selected = name === s.username;
    pick.appendChild(o);
  });
  pick.classList.toggle("hidden", s.accounts.length === 0);
}

async function refreshAuth() {
  try {
    paintAuth(await invoke("auth_status"));
  } catch (e) {
    $("auth-text").textContent = String(e);
  }
}

// Switching accounts changes what is visible on Instagram, so any results
// fetched as the previous account are stale — clear them.
$("account-picker").addEventListener("change", async (e) => {
  try {
    paintAuth(await invoke("switch_account", { username: e.target.value }));
    $("results").innerHTML = "";
    $("actions").classList.add("hidden");
    say(`Switched to @${e.target.value}.`);
  } catch (err) {
    say(String(err), true);
  }
});

$("btn-forget").addEventListener("click", async () => {
  const who = $("account-picker").value;
  if (!who) return;
  try {
    paintAuth(await invoke("remove_account", { username: who }));
    say(`Removed @${who} from this app. (Instagram itself is unaffected.)`);
  } catch (err) {
    say(String(err), true);
  }
});

$("btn-signin").addEventListener("click", async () => {
  await invoke("open_login");
  // The backend polls and captures on its own; this stays as a manual fallback.
  $("btn-capture").classList.remove("hidden");
  say("Log in in the new window — it captures automatically when you're through.");
});

// Auto-capture: fires as soon as the login actually authenticates.
listen("account-added", (e) => {
  $("btn-capture").classList.add("hidden");
  paintAuth(e.payload);
  say(`Signed in as @${e.payload.username}.`);
});
listen("login-cancelled", () => {
  $("btn-capture").classList.add("hidden");
  say("Login window closed — no account added.");
});
listen("login-timeout", () => {
  $("btn-capture").classList.add("hidden");
  say("Login timed out after 5 minutes. Press Add account to try again.");
});

$("btn-capture").addEventListener("click", async () => {
  try {
    const s = await invoke("capture_login");
    $("btn-capture").classList.add("hidden");
    paintAuth(s);
    say(`Added @${s.username}.`);
  } catch (e) {
    say(String(e), true);
  }
});

$("btn-paste").addEventListener("click", () => $("paste-dlg").showModal());
$("paste-save").addEventListener("click", async (ev) => {
  ev.preventDefault();
  try {
    const s = await invoke("set_cookies", { raw: $("paste-box").value });
    $("paste-dlg").close();
    paintAuth(s);
    say(`Added @${s.username}.`);
  } catch (e) {
    say(String(e), true);
  }
});

/* ---------- chrome ---------- */

function say(msg, isErr = false) {
  const el = $("status");
  el.textContent = msg;
  el.className = "status" + (isErr ? " err" : "");
  el.classList.remove("hidden");
}
function clearSay() { $("status").classList.add("hidden"); }

document.querySelectorAll(".tab").forEach((t) =>
  t.addEventListener("click", () => {
    document.querySelectorAll(".tab").forEach((x) => x.classList.remove("active"));
    t.classList.add("active");
    state.mode = t.dataset.mode;
    $("pane-post").classList.toggle("hidden", state.mode !== "post");
    $("pane-profile").classList.toggle("hidden", state.mode !== "profile");
    $("results").innerHTML = "";
    $("actions").classList.add("hidden");
    clearSay();
  })
);

/* ---------- single post ---------- */

$("btn-get").addEventListener("click", getPost);
$("post-url").addEventListener("keydown", (e) => { if (e.key === "Enter") getPost(); });

async function getPost() {
  const url = $("post-url").value.trim();
  if (!url) return;
  say("Fetching…");
  $("btn-get").disabled = true;
  try {
    const post = await invoke("fetch_post", { url });
    state.post = post;
    state.posts = [];
    state.picked = new Set(post.items.map((i) => i.index));
    renderPost(post);
    clearSay();
  } catch (e) {
    say(String(e), true);
  } finally {
    $("btn-get").disabled = false;
  }
}

function renderPost(post) {
  const r = $("results");
  const vids = post.items.filter((i) => i.kind === "video").length;
  r.innerHTML = "";
  const head = document.createElement("div");
  head.className = "post-head";
  head.innerHTML =
    `<h2>@${esc(post.owner)}</h2><span class="muted">${post.items.length} item(s) · ` +
    `${post.items.length - vids} photo, ${vids} video · ${esc(post.shortcode)}</span>`;
  r.appendChild(head);

  post.items.forEach((it) => {
    const c = document.createElement("div");
    c.className = "card";
    c.innerHTML =
      `<input type="checkbox" class="pick" data-idx="${it.index}" checked />` +
      `<span class="badge">${it.kind === "video" ? "video" : it.width + "×" + it.height}</span>` +
      `<img loading="lazy" src="${esc(it.thumb_url)}" alt="" />` +
      `<div class="meta"><span>#${it.index}</span><span>${it.kind}</span></div>`;
    r.appendChild(c);
  });

  r.querySelectorAll(".pick").forEach((cb) =>
    cb.addEventListener("change", () => {
      const i = Number(cb.dataset.idx);
      cb.checked ? state.picked.add(i) : state.picked.delete(i);
      cb.closest(".card").classList.toggle("off", !cb.checked);
      updateCount();
    })
  );
  $("actions").classList.remove("hidden");
  updateCount();
}

/* ---------- profile ---------- */

async function runSearch() {
  const q = $("profile-search").value.trim();
  if (!q) return;
  const list = $("search-results");
  $("btn-search").disabled = true;
  try {
    const hits = await invoke("search_users", { query: q });
    list.innerHTML = "";
    if (!hits.length) {
      say(`No accounts found for “${q}”.`);
      list.classList.add("hidden");
      return;
    }
    clearSay();
    for (const h of hits) {
      const li = document.createElement("li");
      const tags = [h.is_private ? "private" : null, h.is_verified ? "verified" : null]
        .filter(Boolean)
        .join(" · ");
      li.innerHTML =
        `<img loading="lazy" src="${esc(h.profile_pic)}" alt="" />` +
        `<span><span class="who">@${esc(h.username)}</span>` +
        (h.full_name ? `<br><span class="sub">${esc(h.full_name)}</span>` : "") +
        `</span>` +
        (tags ? `<span class="tag">${esc(tags)}</span>` : "");
      // Picking a result fills the username field; scanning stays a
      // deliberate second action, since a private account will fail.
      li.addEventListener("click", () => {
        $("profile-user").value = h.username;
        list.classList.add("hidden");
        say(
          h.is_private
            ? `@${h.username} is private — you'll only see it if the signed-in account follows them.`
            : `Ready to scan @${h.username}.`
        );
      });
      list.appendChild(li);
    }
    list.classList.remove("hidden");
  } catch (e) {
    say(String(e), true);
  } finally {
    $("btn-search").disabled = false;
  }
}

$("btn-search").addEventListener("click", runSearch);
$("profile-search").addEventListener("keydown", (e) => {
  if (e.key === "Enter") runSearch();
});

$("btn-scan").addEventListener("click", async () => {
  const username = $("profile-user").value.trim();
  if (!username) return;
  const limit = Number($("profile-limit").value);
  $("btn-scan").disabled = true;
  say("Scanning…");
  try {
    const posts = await invoke("fetch_profile", { username, limit });
    state.posts = posts;
    state.post = null;
    state.picked = new Set(posts.map((_, i) => i));
    renderProfile(posts);
    say(`Found ${posts.length} post(s). Downloading fetches each at full resolution.`);
  } catch (e) {
    say(String(e), true);
  } finally {
    $("btn-scan").disabled = false;
  }
});

listen("scan-progress", (e) => {
  const { fetched, total, done } = e.payload;
  if (!done) say(`Scanning… ${fetched}${total ? " of " + total : ""} posts`);
});

function renderProfile(posts) {
  const r = $("results");
  r.innerHTML = "";
  posts.forEach((p, i) => {
    const c = document.createElement("div");
    c.className = "card";
    const when = p.taken_at ? new Date(p.taken_at * 1000).toISOString().slice(0, 10) : "";
    c.innerHTML =
      `<input type="checkbox" class="pick" data-idx="${i}" checked />` +
      `<span class="badge">${p.kind === "carousel" ? p.item_count + " items" : p.kind}</span>` +
      `<img loading="lazy" src="${esc(p.thumb_url)}" alt="" />` +
      `<div class="meta"><span>${esc(when)}</span><span>${esc(p.shortcode)}</span></div>`;
    r.appendChild(c);
  });
  r.querySelectorAll(".pick").forEach((cb) =>
    cb.addEventListener("change", () => {
      const i = Number(cb.dataset.idx);
      cb.checked ? state.picked.add(i) : state.picked.delete(i);
      cb.closest(".card").classList.toggle("off", !cb.checked);
      updateCount();
    })
  );
  $("actions").classList.remove("hidden");
  updateCount();
}

/* ---------- selection + download ---------- */

$("check-all").addEventListener("change", () => {
  const on = $("check-all").checked;
  state.picked = new Set();
  document.querySelectorAll(".pick").forEach((cb) => {
    cb.checked = on;
    cb.closest(".card").classList.toggle("off", !on);
    if (on) state.picked.add(Number(cb.dataset.idx));
  });
  updateCount();
});

function updateCount() {
  const n = state.picked.size;
  const noun = state.post ? "item" : "post";
  $("sel-count").textContent = `${n} ${noun}${n === 1 ? "" : "s"} selected`;
  $("btn-download").disabled = n === 0 || !state.dest;
}

$("btn-dest").addEventListener("click", async () => {
  const dir = await open({ directory: true, multiple: false });
  if (dir) {
    state.dest = dir;
    state.destIsDefault = false;
    $("dest-label").textContent = dir;
    updateCount();
  }
});

// Fall back to ~/Downloads so downloading works without picking anything.
async function initDest() {
  try {
    const d = await invoke("default_dest");
    if (d && !state.dest) {
      state.dest = d;
      state.destIsDefault = true;
      $("dest-label").textContent = d + "  (default)";
      updateCount();
    }
  } catch (e) {
    /* leave the user to choose a folder */
  }
}

// Download progress is reported at two levels: files within the current post
// (from the backend) and posts within the batch (tracked here). A single bar
// would be misleading in profile mode, where one "step" is a whole post.
const dl = { files: 0, bytes: 0, failed: 0, post: 0, posts: 0 };

function human(n) {
  if (n > 1024 * 1024) return (n / 1024 / 1024).toFixed(1) + " MB";
  if (n > 1024) return Math.round(n / 1024) + " KB";
  return n + " B";
}

function paintProgress(current) {
  const pct = dl.posts ? (dl.post / dl.posts) * 100 : 0;
  $("pfill").style.width = pct + "%";
  const scope = dl.posts > 1 ? `post ${dl.post}/${dl.posts} · ` : "";
  const failed = dl.failed ? ` · ${dl.failed} failed` : "";
  $("ptext").textContent =
    `${scope}${dl.files} file${dl.files === 1 ? "" : "s"} · ${human(dl.bytes)}${failed}` +
    (current ? ` · ${current}` : "");
}

listen("dl-progress", (e) => {
  const { file, done, total, bytes, error } = e.payload;
  $("progress").classList.remove("hidden");
  if (error) {
    dl.failed++;
  } else {
    dl.files++;
    dl.bytes += bytes || 0;
  }
  // Within a single post, advance the bar by files instead.
  if (dl.posts <= 1 && total) $("pfill").style.width = (done / total) * 100 + "%";
  paintProgress(error ? `${file} failed` : file);
});

$("btn-download").addEventListener("click", async () => {
  if (!state.dest) return;
  $("btn-download").disabled = true;
  $("progress").classList.remove("hidden");
  dl.files = 0;
  dl.bytes = 0;
  dl.failed = 0;
  dl.post = 0;
  dl.posts = state.post ? 1 : state.picked.size;
  paintProgress("starting…");
  let files = 0;
  try {
    if (state.post) {
      files = await invoke("download_post", {
        req: {
          shortcode: state.post.shortcode,
          dest: state.dest,
          only: [...state.picked],
          // Always its own folder — a carousel can be a dozen-plus files, and
          // the default destination is the (already busy) Downloads folder.
          subfolder: true,
        },
      });
    } else {
      const chosen = [...state.picked].sort((a, b) => a - b).map((i) => state.posts[i]);
      let n = 0;
      for (const p of chosen) {
        n++;
        dl.post = n;
        paintProgress(p.shortcode);
        files += await invoke("download_post", {
          req: { shortcode: p.shortcode, dest: state.dest, only: [], subfolder: true },
        });
        // Same pacing as the scan — full-resolution needs one page fetch per post.
        await new Promise((r) => setTimeout(r, 700));
      }
    }
    const where = state.post ? `${state.dest}/${state.post.shortcode}` : state.dest;
    say(`Done — ${files} file(s) saved to ${where}`);
  } catch (e) {
    say(String(e), true);
  } finally {
    $("btn-download").disabled = false;
  }
});

function esc(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])
  );
}

refreshAuth();
initDest();
