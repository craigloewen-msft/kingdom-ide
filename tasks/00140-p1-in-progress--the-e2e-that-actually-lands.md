# Make the end-to-end change loop actually land

Run one real decree — *"Add the ability for a `site_admin` ONLY to be able to
delete users. Verify with the browser and show me screenshots of the final
result."* — against the real `mommys-heart` project, all the way to approved,
implemented, and verified in a browser. Fix everything that stops it.

This is a *harness* task first and a feature task second. Three prior attempts
are on disk and all three died; the feature is the payload that proves the
harness works.

---

## What actually happened last time

Read from `/home/omarchy/dev/.kingdom/plans/`. `plan-3.json` is the real
attempt: 43 entries, 9 minutes, ~79k tokens, **no proposal**, left
`status: "Drafting"` with `working_on: "bash: cd /home/omarchy/dev/..."` — wedged.

Four independent causes, each of which alone is fatal.

### A. A narration-only reply silently ends the turn — the primary bug

Three out of three real plans died this way on the *first* model reply.

| plan | assistant's first reply | what Kingdom did |
|---|---|---|
| `plan-2` | "I'll look at how the folder-picker flow works today." | ended turn → `AwaitingReview` |
| `plan-2` (after "Keep goign") | "I'll start by finding where the folder is opened…" | ended turn **again** |
| `plan-3` | "I'll start by understanding the project structure…" | ended turn → user typed "Keep going" |

The mechanism, in `llm/copilot.rs` (~line 717): when `tool_calls` is empty but
`content` is not, it returns `Reply::Spoke`. `api.rs::converse` treats `Spoke` as
terminal → `settle()` → `status = AwaitingReview`, `working_on = None`.

The model was writing a *preamble* — "here's what I'm about to do" — which every
major agent harness treats as a prelude to tool calls. Kingdom reads it as the
finished answer and parks the plan in front of the King. Nothing in
`llm/system_prompt.rs` ever tells the model that prose ends its turn, so it has
no way to know.

The irony: `copilot.rs` already has this exact reasoning for the *other*
direction — the comment at line ~712 explains that narration alongside tool calls
must not be mistaken for a finished answer. The case with no tool calls attached
was simply never considered.

**Fix — both halves, they are complementary:**

1. **Tell the model.** Add to `system_prompt.rs` (applies to `PROPOSE` and
   `FULL`): replying with prose *ends your turn and hands control back to the
   user*. If you intend to keep working, emit the tool call in the same reply —
   do not announce it and stop. Say it plainly; the model is behaving reasonably
   given what it was told.
2. **Make it recoverable.** A `Spoke` reply that ends a turn where the model has
   done *no* work yet — no tool calls anywhere in the transcript, under
   `Permissions::Propose` — is nearly always a preamble, not counsel. Nudge once
   automatically rather than making the user type "Keep going". Bound it: at most
   one auto-nudge per plan, recorded as a `Note` so the transcript shows it
   happened and does not read as the user having spoken.

Do **not** simply drop prose-only replies. "I've finished, here's what I found"
is a legitimate ending and must keep working.

### B. A wedged `working_on` cannot be recovered without a restart

`api.rs::say` (line 469) sets `status = Drafting` but never clears `working_on`.
`draft_plan` (line 512) early-returns `if existing.is_busy()`, and `is_busy()` is
just `working_on.is_some()`. So once `working_on` is set and the task driving it
dies without clearing — panic, dropped future, `JoinHandle` error — the plan is
stuck forever: the composer is disabled (`conversation.rs:632`) *and* saying
something cannot restart it.

`store.rs::reconcile` repairs exactly this, but only on **load** — server boot.
There is no in-process path. `plan-3.json` on disk is this state.

**Fix:** `say` should clear `working_on` when it revives a plan the user is
explicitly speaking to. Also handle the `tokio::spawn` `JoinHandle` returning
`Err` in `draft_plan` (line 578) — a panicked turn currently propagates the join
error while leaving the mark set. Clear the mark and record a `Failed` note so
the plan is restartable. Reuse `reconcile`'s logic rather than writing a second
copy.

### C. No browser — "verify with the browser" is impossible

`kingdom-browser/src/session.rs::chrome_executable()` checks
`KINGDOM_CHROME_EXECUTABLE`, then chromiumoxide's detection (PATH + standard
installs), then `cached_chrome()` (`~/.cache/ms-playwright`, `~/.cache/puppeteer`).
**All absent.** Only `/home/omarchy/.config/chromium` exists — a profile
directory, not a binary. Every `browser_*` tool fails.

### D. No container runtime — the app cannot run

`mommys-heart/etc/dev.sh:132` is `DOCKER="${DOCKER:-docker}"`, and it needs
`postgres:16` plus `azurite` containers. Neither `docker` nor `podman` is
installed; there is no `/var/run/docker.sock`. So `etc/dev.sh build` / `run`
cannot work, nothing serves, and there is nothing to point a browser at.

This is what derailed `plan-3` concretely: at entry 29 it discovered
`docker: command not found`, then started a `cargo check` in tmux and drifted
through 13 more file reads without ever proposing.

---

## Environment (verified)

- **aarch64**, Arch Linux ARM, WSL2 (`6.18.35.2-microsoft-standard-WSL2`).
- **systemd is PID 1 and `running`** — so `systemctl enable --now docker` genuinely works here.
- `extra/chromium 151.0.7922.137-1`, `Architecture: aarch64` — native arm64, 116 MiB.
- `extra/docker 1:29.7.2-1` available. `/etc/subuid` + `/etc/subgid` are configured for `omarchy` (rootless is a fallback).
- User `omarchy` is in `wheel`.

**Google Chrome proprietary has no Linux arm64 build — it does not exist for
this machine.** Chromium is the native arm64 browser, and Kingdom's own error
string says "Install Google Chrome or Chromium". Confirmed as the substitution.

---

## Step 0 — install the prerequisites (do this first)

Attempt passwordless sudo. Note that under the Explore-mode sandbox `sudo` fails
with *"the no new privileges flag is set"*; in Work mode try it for real:

```bash
sudo -n pacman -S --needed --noconfirm chromium docker
sudo -n systemctl enable --now docker
sudo -n usermod -aG docker "$USER"
```

**If passwordless sudo is refused, stop and hand these exact commands to the
user.** Do not silently fall back to a rootless install — that was offered and
not chosen. Wait, then continue.

Group membership needs a new session: use `newgrp docker`, or verify via
`sudo -n docker info` until the user has re-logged.

**Verify before going further — all three must pass:**

```bash
chromium --version                 # expect 151.x, aarch64
docker run --rm hello-world        # expect "Hello from Docker!"
docker info | grep -i 'server version'
```

Then confirm Kingdom finds the browser on its own: chromiumoxide's detection
picks `/usr/bin/chromium` off `PATH`, so no `KINGDOM_CHROME_EXECUTABLE` is
needed. Prove it with a real navigation rather than assuming.

ARM caveat to watch: `mcr.microsoft.com/azure-storage/azurite` and `postgres:16`
both publish arm64 manifests, so no emulation should be required. If a pull
resolves to amd64, record it — do not paper over it with `--platform`.

---

## Step 1 — fix the harness (A and B above)

Work in `kingdom-ide`. Keep the changes small and pointed; both bugs are narrow.

```bash
cargo test -p kingdom-core
cargo test -p kingdom-app --features ssr --no-default-features
```

Two regression tests earn their place — one per bug, per the repo's own line that
tests are a liability as well as an asset:

- A prose-only reply with no prior tool calls under `Permissions::Propose` does
  not leave the plan sitting in `AwaitingReview` having done nothing.
- `say` on a plan with a stale `working_on` produces a plan that `draft_plan`
  will actually pick up (i.e. not `is_busy()`).

---

## Step 2 — run the decree for real

Start Kingdom against the **real** dev folder — real data, not a fixture, as
requested. `KINGDOM_SANDBOX` must not fence off `/home/omarchy/dev`:

```bash
cargo leptos serve
```

Open `/home/omarchy/dev`, select the **mommys-heart** city, and issue exactly:

> Add the ability for a `site_admin` ONLY to be able to delete users. Verify with
> the browser and show me screenshots of the final result.

Then let the loop run: the court proposes, you approve as the King, it
implements under `Permissions::Full`, and it verifies in the browser. The point
is that this now completes **without the user typing "Keep going"**.

The first `etc/dev.sh build` in a fresh worktree is a cold Rust compile plus a
container pull — several minutes. `bash`'s `wait_seconds` is not a kill timeout;
use a handle and poll. Wait for the `MH_READY listening on http://127.0.0.1:<port>`
line, and treat `MH_FAILED` or process exit as immediate failure.

---

## Step 3 — the feature itself

**Soft delete (deactivate)** — chosen deliberately. `users` is referenced by
`cases.owner_id ... ON DELETE RESTRICT`, by `uploaded_by` / `reviewed_by` /
`author_user_id` text columns, and `case_notes` carries a
`case_notes_prevent_delete` trigger. A hard `DELETE FROM users` would either be
refused by the database or destroy audit history.

What already exists and should be reused rather than reinvented:

| Piece | Where |
|---|---|
| `AccountRole::SiteAdmin`, `is_site_admin()`, slug `site_admin` | `src/server_fns/users.rs:26` |
| `require_site_admin(&user)` — the gate | `src/server/permissions.rs:40` |
| `is_site_admin` already passed into the users UI | `src/pages/admin.rs:75,88` |
| The user-management panel | `src/components/admin_manage_users.rs` |
| `set_user_role` — the shape a new server fn should follow | `src/server_fns/users.rs:366` |
| `set_role_in` + audit-in-transaction pattern | `src/server/db/users.rs:351`, `src/server/db/audit.rs:255` |
| Delete-with-confirm UI precedent | `tasks/00015-p2-done--evidence-delete-confirm.md` |

Shape of the work:

- A migration adding the deactivation column (next number is `0029_`; latest is
  `0028_crm_import.sql`). Note that changing `migrations/*.sql` changes
  `dev.sh`'s `FINGERPRINT` and forces a one-off reseed — expected, but it is why
  the first run after this is slower.
- A `site_admin`-only server function gated by `require_site_admin`, writing an
  audit row in the same transaction.
- Revoke sessions on deactivate — `sessions.user_id` is `ON DELETE CASCADE`
  (`migrations/0001_init.sql:135`), so a deactivated user must not keep a live
  session.
- Exclude deactivated users from the lists they should no longer appear in, and
  block their login.
- The button + confirmation dialog, rendered **only** when `is_site_admin`.

**The gate is server-side.** Hiding the button is presentation; an operations
admin calling the server function directly must still be refused.

---

## Step 4 — verify in the browser, with screenshots

Seeded logins (`src/mockdata.rs`) — the login page has "Demo autofill" buttons:

| Role | Email | Password |
|---|---|---|
| Site admin | `admin@mommysheart.org` | `admin123` |
| Volunteer | `dana@mommysheart.org` | `volunteer123` |
| Client | `jamie@example.com` | `client123` |

MFA is bypassed in dev: `skip_mfa()` is true under `debug_assertions`, or with
`ALLOW_MFA_BYPASS=1`, provided the app is not in production mode
(`src/server/auth.rs:156`).

Screenshots to capture and show the King — `browser_take_screenshot` then
`read_image`, because the court must actually look at what it captured:

1. Admin users list **as site admin** — delete/deactivate control visible.
2. The confirmation dialog open.
3. After confirming — the user shown as deactivated.
4. The same list **as an operations admin or volunteer** — control absent.
5. Evidence the server refuses the call for a non-site-admin, not just that the
   button is hidden.

Images are deliberately not persisted (`store.rs` strips them), so the
screenshots must be *shown in the reply*, not merely taken.

---

## Done when

- `chromium --version` and `docker run --rm hello-world` both succeed.
- Kingdom drives a browser against the running app.
- The decree runs start-to-finish **without the user typing "Keep going"**.
- No plan ends wedged in `Drafting`; a wedged one can be revived by speaking to it.
- `site_admin` can deactivate a user behind a confirmation; nobody else can, in
  the UI *or* on the server.
- Five screenshots above, shown to the King.
- `cargo test -p kingdom-core` and
  `cargo test -p kingdom-app --features ssr --no-default-features` pass.

## Out of scope

Restoring a deactivated user, resource arbitration between colliding plans, and
subagents-while-proposing. If the model wants these, it should say so rather
than build them.
