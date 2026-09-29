# cswap TUI — behavioral spec notes (input for the cswitch ratatui port)

Sources read (2026-09-29):
- Python reference: `/Users/jacky/projects/dev/claude-swap/src/claude_swap/tui/{__init__,app,dashboard,autoview,widgets,modals,data,theme}.py`, `tui/cswap.tcss`, `appearance.py`
- Go design doc: `scratchpad/cswap-go-docs/09-tui-menubar.md` (older than the Python; see §13 for where the Python has moved on)
- README "Interactive dashboard (TUI)" (README.md:160-164) + screenshot `assets/tui-watch.png`
- Supporting constants pulled from `switcher.py` (SENTINEL_NOTES/ERROR_NOTES/last_seen_note), `json_output.py` (USAGE_* sentinels), `usage_store.py` (STALE_OK_S), `poll_policy.py` (SERVE_TTL_S, binding_pct), `settings.py` (SETTING_SPECS, ui.theme), `autoswitch.py` (event `human()` strings, pct_label), `pace.py`, `oauth.py` (reset_clock_string), `snapshot_source.py`, `cli.py`.

Conventions in this file: `[muted]`, `[accent]`, `[fg]`, `[ok]`, `[warn]`, `[crit]`, `[track]` = palette roles (§10). "bold" = bold text style. All strings quoted verbatim from source.

---

## 1. Entry points

| Invocation | Behavior (cli.py:951-955, 1342-1349; tui/__init__.py) |
|---|---|
| bare `cswap` | Only when **both stdout and stdin are TTYs**: argv is rewritten to `["--tui"]` → dashboard. Piped/non-tty bare invocation keeps the argparse usage error (exit 2). |
| `cswap tui` (alias of hidden `--tui`) | `tui.run(switcher)` → `CswapApp(start="dashboard")`. Opens **DashboardScreen**. |
| `cswap watch` (alias of hidden `--watch`) | `tui.run(switcher, start="watch")`. DashboardScreen is pushed first, then **WatchScreen stacked on top**, so Esc/q from watch lands on the dashboard, not on process exit. |
| `cswap --menubar` | macOS-only rumps menu bar app. Out of scope for the TUI port (exists; not documented here). |

`tui.run()` sequence (tui/__init__.py:17-43):
1. `detect_terminal_background()` (appearance.py) — OSC 11 + DA1 query while still in cooked mode, 1.0 s cap; returns `"light"`/`"dark"`/`None`. Skipped (None) on Windows, `TERM=dumb|linux`, inside tmux/screen (`TMUX`/`STY` set), or non-tty. Any exception → None.
2. Build app with `detected`.
3. `drain_stdin()` (tcflush TCIFLUSH) so a late OSC reply is not replayed as keystrokes.
4. `app.run()`; exit code = `app.return_code or 0` (always 0 in practice; TUI errors surface as toasts/modals, never exit codes).

Heavy imports (textual/rich) are deferred inside `run()` so `cswap list` etc. never pay for them.

---

## 2. Application shell (`tui/app.py`)

```
TITLE = "claude-swap"
ENABLE_COMMAND_PALETTE = False    # no global command palette; menus are the only action surface
BINDINGS = [ctrl+t → toggle_theme  ("Theme", shown in footer on every screen)]
POLL_INTERVAL_S = 3.0
SNAPSHOT_AGE_NOTE_S = 60.0
```

Reactive state: `snapshot: AccountsSnapshot | None`, `refresh_status: str`, `busy: bool`, plus `theme` (Textual built-in).

Constructor loads:
- `threshold_pct = load_settings(backup_dir).threshold` (fallback `None` on any error). This is the value drawn as the **tick mark on every usage bar** app-wide.
- `_theme_name = load_ui_settings(backup_dir).theme` (`"dark" | "light" | "auto"`, default `"auto"`; fallback `"auto"` on error).

`on_mount`: register both themes; resolve theme (§10.3); `printer.set_theme(resolved)` so captured CLI output uses the same palette; push DashboardScreen (+ WatchScreen if start=watch); `set_interval(3.0, _tick)`; `set_interval(1.0, _update_refresh_status)`; call `_tick()` once immediately.

### 2.1 Snapshot data model consumed by the TUI

`AccountsSnapshot { active_number: str|None, accounts: tuple[AccountSnapshot], taken_at: float }`

`AccountSnapshot { number: str, email, org_name, org_uuid, is_active: bool, kind: "oauth"|"api_key", switchable: bool, usage: UsageEntry, alias: str = "", disabled: bool = False }`; `display_tag` = `org_name or "personal"`.

`UsageEntry` fields used by the TUI: `sentinel: str|None` (live overlay replacing bars), `last_good: dict|None` (window dict, §3.3), `fetched_at: float|None`, `age_s: float|None`, `last_error: str|None`.

Sentinel strings (json_output.py) and their display wording (switcher.SENTINEL_NOTES) — must be byte-identical to `cswap list`:

| sentinel | label shown |
|---|---|
| `"token expired"` | `token expired — refresh deferred this pass; retries automatically` |
| `"foreign credential"` | `live credential belongs to another account — a switch repairs it` |
| `"api key"` | `API key (no quota)` |
| `"keychain unavailable"` | `keychain unavailable — locked or in use; try again` |
| `"re-login needed"` | `re-login needed — refresh token dead; log in with Claude Code, then run: cswap add` |
| any other | the raw sentinel string |

`ERROR_NOTES` (for `last_error` on the "usage unavailable" line): `"store-unmirrored"` → `CLAUDE_SECURESTORAGE_CONFIG_DIR set — unset it or run from a normal shell`; `"invalid_client"` → `cswap's OAuth client was rejected — systemic, not this account`; `"consume-busy"` → `another cswap surface holds the slot — retries next pass`; `"stash-unreadable"` → `this slot's stashed successor is unreadable — unlock the keychain or fix the file, then retry; \`cswap unclaimed\` inspects it`; unknown kinds print raw.

`last_seen_note(entry)` → `"last seen {100-headroom:.0f}% used · {age}"` where age comes from printer.format_age: `"just now"` (<60s), `"{m}m ago"`, `"{h}h ago"`, `"{d}d ago"`. None when `last_good` or `fetched_at` missing or no 5h/7d window.

### 2.2 Refresh / polling (two lanes, single-flight each)

Every 3 s `_tick()` starts **one** lane:
- `_store_only == True` (Auto screen open) → store lane only.
- else if the normal lane is idle → normal lane (`full` = pending `_full_next` flag, then cleared).
- else (normal lane busy) → store lane, so another process's store writes are still observed while a slow fetch runs.

Lanes (`SnapshotSource.take`, snapshot_source.py):
- **normal**: `accounts_snapshot(fetch=None)` — same on-demand pass as `cswap list`; the usage store's own poll plans / serve TTL / backoff decide which accounts actually hit the network. `full=True` (key `f`) is **not faster**: identical call; it only exists as an explicit "do a pass now" hook.
- **store**: `accounts_snapshot(fetch=set())` — pure read of persisted store, no network.
- `SnapshotSource` reconciles per account (same identity = (email, org_uuid, kind)): never lets `fetched_at` regress (keeps previous usage with recomputed age), keeps a `token expired` sentinel sticky until a newer fetch lands.

Apply (`_apply_snapshot`): generations are numbered; a result older than the latest applied one only **merges its usage rows** into the current snapshot (same identity), never restores stale metadata; `taken_at = max(...)`.

Errors: worker error in `refresh-normal` → toast `Refresh failed: {msg}` (warning, 6 s); `refresh-store` → `Store refresh failed: {msg}`; de-duplicated against the last error message so a persistently failing poll doesn't spam. `action` group error → `Action failed: {err}` (error). `engine` group error → `Auto-switch engine stopped: {err}` (error).

`refresh_status` (recomputed every 1 s, shown only in the Watch screen title, §6):
- `"snapshot {format_duration(age)} ago"` once `now - snapshot.taken_at >= 60 s` (hidden while polling is healthy).
- `"refreshing {format_duration(elapsed)}"` once the normal lane has been running `>= 3 s`.
- parts joined with `" · "`; empty string when healthy.

`request_refresh(full=False)` → optionally arm `_full_next`, then `_tick()`. `set_store_only(bool)` → set flag and `request_refresh()`. Every completed mutating action calls `request_refresh()`; an engine `switch` event also calls it.

### 2.3 Mutating actions (single-flight app-wide)

`_start_action(label, fn, show_output=False)`: if `busy` → toast `Another action is still running` (warning) and drop; else `busy=True`, run `fn` on a thread with stdout/stderr captured (ANSI color forced) and stdin replaced by an empty stream (`Error: interactive input is not available here.` if anything prompts). `ClaudeSwitchError` → captured `Error: {e}`, ok=False.

`_action_done(label, result, show_output)` — exact order:
1. `busy=False`; `request_refresh()`.
2. not ok → `OutputModal(f"{label} — failed", output)`.
3. payload has key `"switched"`: true → toast `Switched to {to.email or "account {to.number}"}` (title "Switch"); false → toast `{reason or "no switch performed"}` (title "No switch", warning). Return (never a modal for switches).
4. `show_output` and non-blank output → `OutputModal(label, output)`.
5. else if first non-empty output line → plain toast with that line.

Call sites:

| method | label | switcher call | show_output |
|---|---|---|---|
| `do_switch(n)` | `Switch to account {n}` | `switch_to(n, json_output=True)` | no |
| `action_switch_best()` | `Switch (best)` | `switch(strategy="best", json_output=True)` | no |
| `do_toggle_disabled(n)` | `Disable account {n}` / `Enable account {n}` (direction from live snapshot; silently no-op if n not found) | `set_account_disabled(n, target)` | no |
| `confirm_remove(n, email)` → confirm → | `Remove account {n}` | `remove_account(n, assume_yes=True)` | no |
| `action_add_current()` → confirm → | `Add current login` | `add_account()` | **yes** |
| `action_add_token()` → form → (occupied-slot confirm) → | `Add account from token` | `add_account_from_token(token=, email=, slot=, assume_yes=True)` | **yes** |

Confirm texts (§8.1). Occupied slot: `_slot_occupant(slot)` scans snapshot for `number == str(slot)`; if found → `ConfirmModal("Slot {slot} is occupied by {email}. Overwrite?", title="Overwrite slot", yes_label="Overwrite")`. `slot=None` or no snapshot yet → no check.

Navigation actions: `action_refresh_full()` → `request_refresh(full=True)` + toast `Refreshing usage…` (2 s). `action_open_auto()` / `action_open_watch()` push the screen unless already on it. `apply_theme(name)` / `action_toggle_theme()` → §10.3.

---

## 3. Shared rendering (`tui/widgets.py`, `tui/data.py`)

### 3.1 Usage bar glyphs and rules

```
_BAR_FILLED = "━"   _BAR_HALF = "╸"   _BAR_EMPTY = "─"   _BAR_TICK = "┃"
```

`bar_cells(pct, width, stale, threshold, palette)`:
- `pct is None` → `width × "─"` in `[track]`.
- `frac = clamp(pct, 0, 100) / 100`; `cells = frac*width`; `full = int(cells)`; `half = (cells - full) >= 0.5 and full < width`.
- `tick_at = clamp(round(threshold/100*width), 0, width-1)` when threshold is set (the auto-switch trigger line).
- Fill color = severity(pct) (§10.1); if `stale`, style is `"{color} dim"` (fill and pct number only, not track/tick).
- For i in 0..width: `i == tick_at` → `┃` in `[warn]` (drawn unconditionally, even inside the filled region); `i < full` → `━`; `i == full and half` → `╸`; else `─` in `[track]`.

`usage_bar(label, pct, suffix, width, ...)` = one row:
```
"{label} "[muted] + bar + ("  usage unknown"[muted] if pct None else " {pct:3.0f}%"[severity, dim if stale]) + ("  {suffix}"[muted] if suffix)
```
Example: `5h ━━━━╸────┃──  47%  resets 2h 13m · 20:39`

### 3.2 Time helpers (`data.py`)

- `format_duration(s)`: `<60 → "45s"`, `<1h → "12m"`, `<1d → "2h 13m"` (or `"2h"` when m=0), else `"3d 4h"` (or `"3d"`). Examples: 42→`42s`, 180→`3m`, 7980→`2h 13m`, 93600→`1d 2h`.
- `format_age(age_s)`: `None` when age is None or `< SERVE_TTL_S (180 s)`; else `"· {format_duration(age)} ago"` (e.g. 400 s → `· 6m ago`). Shown on the card header.
- `reset_text(window, now)`: from `resets_at` (ISO; `Z` → `+00:00`) computed **live at render time**, never from the cached `countdown` string: remaining ≤ 0 → `"resets now"`, else `"resets {format_duration(remaining)}"`. None if no/unparseable `resets_at`.
- `reset_clock(window, now)`: absolute local time via `oauth.reset_clock_string`: same local day → `"20:39"` (`%H:%M`), otherwise `"Jul 5 08:59"` (`%b {day} %H:%M`, day not zero-padded). None once elapsed.
- `clock_stamp()` → `time.strftime("%H:%M:%S")` local, used by the auto event log.
- Staleness for dimming: `stale = age_s is not None and age_s > STALE_OK_S (300 s)`.

### 3.3 Window dict shape (`UsageEntry.last_good`)

```json
{"five_hour": {"pct": 47.0, "resets_at": "2026-07-17T20:39:00Z"},
 "seven_day": {"pct": 63.0, "resets_at": "..."},
 "spend":     {"used": 12.5, "limit": 50.0, "pct": 25.0, "resets_at": "..."},
 "scoped":    [{"name": "Fable", "pct": 62.0, "resets_at": "..."}]}
```
Any key absent → **no row at all** (an annual plan with no `seven_day` never shows a 7d row; never render "0%" or "—" placeholders).

### 3.4 `usage_rows(last_good, now, fetched_at)` → `[(label, pct, suffix, suffix_full)]`, in this order

1. `spend` → label `$$`; `amounts = "${used:,.2f} / ${limit:,.2f}"`; suffix = `"{reset}  {amounts}"` (two spaces) or just amounts.
2. `five_hour` → label `5h`; suffix = reset text (or "").
3. `seven_day` → label `7d`; suffix = reset text; plus `"(ahead of pace)"` appended (two-space separated) when `pace.compute_pace(window, fetched_at).ahead`.
4. each `scoped` window → label = its `name` (e.g. `Fable`, `Opus`); `pct >= 100` → append `"(!)"` (and **no** pace marker); else append `"(ahead of pace)"` when ahead.
`suffix_full` = same but the reset part is `"{reset} · {clock}"` (e.g. `resets 2h 13m · 20:39`) when a clock exists; equals `suffix` otherwise.

Pace ("ahead of pace", pace.py): weekly windows only (7d + scoped). Window start derived by rolling `resets_at` back by whole 7-day periods until ≤ `fetched_at`; `expected_pct = elapsed/period*100`; `ahead = actual - expected >= 15.0` pct points; suppressed for the first 24 h after a reset. Uses `fetched_at`, not wall clock.

### 3.5 Full account card `account_card_text(acc, width, threshold, now, palette)`

Header line:
```
"{number:>2}  "[bold fg]
+ alias[bold accent] + " ({email})"[fg]        (if alias)   |   email[fg]
+ "  [{display_tag}]"[muted]
+ "   ● active"[bold accent]                   (if is_active)
+ "   (disabled)"[muted]                       (if disabled)
+ "   · 6m ago"[muted]                         (format_age, only when ≥180 s old)
```

Then exactly one of:
- **Sentinel branch** (`usage.sentinel` set): `"\n    " + marker + " " + sentinel_label` where marker is `·`[muted] for `api key`, else `⚠`[warn]. If sentinel ≠ `api key` and `last_seen_note` exists: `"\n    └ last seen 53% used · 12m ago"`[muted]. No bars.
- **No-data branch** (no rows): `"\n    usage unavailable"`[muted] + `" · {ERROR_NOTES[last_error] or last_error}"`[muted] if `last_error`.
- **Bars**: `label_width = max(len(label))`; `bar_width = clamp(width - 42 - label_width, 12, 30)`; `row_overhead = 4 + label_width + 1 + bar_width + 5 + 2`. Per row: use `suffix_full` only if `row_overhead + len(suffix_full) <= width` (decided **per row**, so 5h/7d may keep their clocks while a longer spend row drops its clock); each row is `"\n    " + usage_bar(label.ljust(label_width), pct, suffix, bar_width, stale, threshold)`.

Sketch (width ≈ 100, threshold 90):
```
 2  john.doe@gmail.com  [Personal]   ● active   · 6m ago
    $$    ━━━━━━━━━━━━━━━━━━━━━━━━━━━┃──   25%  resets 3d 4h · Oct 2 09:00  $12.50 / $50.00
    5h    ━━━━━━━━━━━━━━━━━━━━━━╸────┃──   76%  resets 2h 47m · 20:39
    7d    ━━━━━━━━━━━━━━━━━━━━━━━━━━━┃──   40%  resets 4d 21h · Oct 4 09:00  (ahead of pace)
    Fable ━━━━━━━━━━━━━━━━━━━━━━━━━━━┃━━  100%  resets 5d 3h · Oct 4 12:00  (!)
```
Screenshot (assets/tui-watch.png, watch screen) confirms: title `watching all accounts` muted top-left; each card = bold number, email, `[Personal]`/`[Work]` tag muted, `● active` in terracotta; rows `5h`/`7d`/`Fable` with muted labels, colored bars (amber 76%, red 96%, green 59/40%), pct right of bar, then `resets 2h 47m` muted; cards separated by a blank line; **no 7d row** on the first account (only the windows it has).

### 3.6 Mini line `mini_account_text(acc, now, palette)` (inactive accounts on the dashboard)

Single line, `no_wrap`, overflow ellipsis:
```
"{number:>2}  "[bold muted] + (alias[bold accent] + " ({email})"[fg] | email[fg]) + "  [{tag}]"[muted] + ("  (disabled)"[muted]) + "   "
```
then:
- sentinel → just the sentinel label (`[muted]` for api key, else `[warn]`). Nothing else.
- else for `five_hour`, `seven_day` present (joined by `" · "`[track]): `"5h "`[muted] + `"{pct:.0f}%"`[severity, dim if stale]; if `pct >= 100` append `" ({reset_text})"`[muted]; else for 7d append `" (ahead)"`[warn] when ahead of pace. Then every scoped window with `pct >= 100` → `"{name} (!)"`[crit]. No parts at all → `"usage unknown"`[muted].
Example: ` 2  work@acme.dev  [personal]   5h 92% · 7d 63% (ahead) · Fable (!)`

### 3.7 `AccountsPanel` (dashboard monitor; auto screen with `show_minis=False`)

- `snapshot is None` → `loading…`[muted].
- no accounts → two lines [muted]: `No managed accounts yet.` / `Use the menu below: Add account — from your current Claude Code login, or from a setup-token / API key.`
- else `width = panel width - 2`; accounts in slot order: active → full card (threshold = `app.threshold_pct`); others → mini (or skipped when minis off). No blocks → `no active managed login`[muted].
- Join rule: blank line (`\n\n`) before/after any multi-line block (the active card), single `\n` between consecutive minis.
- Repaints on every snapshot and theme change.

`AccountCard`/`AccountItem` (list rows on Switch/Watch): each account rendered as a full card via `account_card_text(acc, row width, threshold=None)` — **note: list cards pass no threshold, so the ┃ tick only appears on the dashboard/auto panel card**, not on Switch/Watch rows. `AccountItem` remembers `number`/`email`.

---

## 4. Dashboard screen (`tui/dashboard.py: DashboardScreen`)

Layout (top → bottom):
```
┌────────────────────────────────────────────────────────────────────────┐
│ (padding 1 3)                                                          │
│  2  john.doe@gmail.com  [Personal]   ● active                          │  AccountsPanel
│     5h    ━━━━━━━━━━━━━━━╸───┃──   76%  resets 2h 47m · 20:39          │  (active = full card,
│     Fable ━━━━━━━━━━━━━━━━━━━┃──   59%  resets 5d 3h · Oct 4 12:00     │   others = one-line minis)
│                                                                        │
│  1  alice@corp.io  [Acme]   5h 12% · 7d 40%                            │
│  3  john.doe@company.com  [Work]  (disabled)   5h 96% · 7d 40%         │
├──────────────────────────────────────────────────────────── border ────┤  border-bottom solid $panel
│                                                                        │
│   menu › add account                                                   │  #menu-title breadcrumb [muted]
│                                                                        │
│  ▌ From current Claude Code login                                      │  ListView #menu; highlighted row =
│    From a setup-token / API key…                                       │  thick accent left border + $surface bg
│    ← back                                                  [muted]     │
│                                                                        │
│ s Switch accounts  w Watch  q Quit  ^t Theme                           │  Footer (visible bindings)
└────────────────────────────────────────────────────────────────────────┘
```
The arrow keys drive the **menu**, not the accounts. The menu is a stack of `(title, entries)`; breadcrumb = titles joined with `" › "`; root title is literally `"menu"`. After every push/pop the list is rebuilt and the cursor resets to index 0. The `← back` row is rendered muted.

Root menu (exact labels/ids, in order):
```
Switch account…              switch
Watch accounts               watch
Auto-switch view             auto
Add account…                 add-menu
Disable / enable account…    disable-menu
Remove account…              remove-menu
Theme…                       theme-menu
Quit                         quit
```
No "Refresh" entry by design (every view auto-refreshes; `f` is the hidden escape hatch). There is **no rotate / next-available / alias item in the TUI** (those exist only in the CLI and the macOS menu bar).

Submenus:
- `add account`: `From current Claude Code login` (add-login) · `From a setup-token / API key…` (add-token) · `← back`.
- `remove account`: one row per account `"{number}  {alias (email) | email}  [{tag}]"` → id `remove:{n}` · `← back`. Selecting opens the Remove confirm (§8.1); menu stays on the submenu.
- `disable / enable`: one row per account `"{number}  {alias (email) | email}{'  (disabled)' if disabled}   {'→ enable' if disabled else '→ disable'}"` → id `disable:{n}` · `← back`. Selecting toggles **immediately with no confirmation**, then pops back to root.
- `theme`: rows `"● dark" / "  light" / "  auto"` (● marks the current setting) → id `theme:{name}` · `← back`. Selecting applies + persists the theme, toasts `Theme: {name}`, pops to root.
- `quit` → `app.exit()`.

Key bindings (DashboardScreen; ListView also handles ↑/↓/Home/End/PgUp/PgDn/Enter natively; mouse click selects):

| key | action | footer |
|---|---|---|
| `↑`/`↓`, `k`/`j` | menu cursor | hidden |
| `Enter` (or click) | activate menu row | (list default) |
| `Esc`, `←` | pop one menu level (no-op at root) | hidden ("Back") |
| `s` | open SwitchScreen | **Switch accounts** |
| `w` | open WatchScreen | **Watch** |
| `g` | open AutoScreen | hidden ("Auto view") |
| `f` | full refresh + toast `Refreshing usage…` | hidden ("Refresh usage") |
| `q` | quit | **Quit** |
| `ctrl+t` | cycle theme dark→light→auto, toast `Theme: {name}` | **Theme** (app-level, every screen) |

---

## 5. Switch screen (`SwitchScreen`, opened by `s` / menu "Switch account…")

```
   switch to which account?                                   #list-title [muted]

  ▌ 1  alice@corp.io  [Acme]                                   AccountItem (full card, no ┃ tick)
  ▌    5h ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━   12%  resets 4h 2m · 23:10
  ▌    7d ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━   40%  resets 4d 21h · Oct 4 09:00
                                                               (margin-bottom 1 between rows)
    2  john.doe@gmail.com  [Personal]   ● active
       5h ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━   76%  resets 2h 47m · 20:39

 enter Switch  b Best pick  esc Back  ^t Theme                 Footer
```
- Every account as a full live card (same list machinery as Watch). Cursor starts on the **active** account on first build.
- Live updates: if the set/order of account numbers is unchanged, rows are updated in place and the **cursor is untouched**; if changed, the list is rebuilt and the cursor clamps to `min(previous, len-1)`.
- Flash: a row whose `usage.fetched_at` advanced gets class `flash` (`$panel` background) for `FLASH_S = 1.5 s`.

| key | action | footer |
|---|---|---|
| `↑`/`↓`, `k`/`j` | move cursor | hidden |
| `Enter` / click | `do_switch(number)` then **pop screen immediately** (result toast/modal appears on the screen underneath) | **Switch** |
| `b` | `switch(strategy="best")` | **Best pick** |
| `Esc`, `q`, `s` | pop back | **Back** |

---

## 6. Watch screen (`WatchScreen`, `w` / menu "Watch accounts" / `cswap watch`)

Same layout as Switch (title + full cards list + footer). Read-only monitor by default: **no cursor at all** (`index=None`), nothing focused.

Title text:
- monitor mode: `watching all accounts`, plus `" · " + refresh_status` when non-empty → e.g. `watching all accounts · snapshot 1m ago · refreshing 5s`.
- select mode: `switch to which account? · enter confirm · esc cancel`.

`s` arms selection: cursor jumps to the active account, list gets focus, title changes, `Enter` binding becomes visible. Selecting an account calls `do_switch(number)` and **disarms but stays on the Watch screen** (you keep watching on the new account). Esc/q while armed only disarms; a second Esc/q leaves.

| key | monitor mode | select mode | footer |
|---|---|---|---|
| `s` | arm selection | disarm | **Switch** |
| `Enter` / click | ignored | switch highlighted, disarm, stay | **Confirm** (hidden while not armed) |
| `↓`/`j`, `↑`/`k` | scroll viewport (no animation) | move cursor | hidden |
| `f` | full refresh | same | hidden ("Refresh") |
| `Esc`, `q` | pop screen (to dashboard) | disarm only | **Back** |

---

## 7. Auto-switch view (`tui/autoview.py: AutoScreen`, `g` / menu "Auto-switch view")

Hosts the real `AutoSwitchEngine` in a thread; **always opens in dry-run**; going live is an explicit confirmed action. Neither the dry-run/live state nor the session threshold is ever persisted.

```
┌────────────────────────────────────────────────────────────────────────┐
│  2  john.doe@gmail.com  [Personal]   ● active                          │  AccountsPanel(show_minis=False)
│     5h ━━━━━━━━━━━━━━━━━━━━━━╸────┃──   76%  resets 2h 47m · 20:39     │  active card only, WITH ┃ tick
│     7d ━━━━━━━━━━━━━━━━━━━━━━━━━━━┃──   40%  resets 4d 21h            │
│                                                                        │
│  DRY-RUN   auto-switch · threshold 90% (session) · poll every 60s   ← → adjust · enter done
│                                                                        │  #auto-title-row: badge + summary
│  Next best                                                             │  #candidates [muted header]
│     3  john.doe@company.com   12% used                                 │  ranked, best headroom first
│     1  alice@corp.io   API key (no quota)                              │
├──────────────────────────────────────────────────────── border-top ────┤
│ 20:39:01  — engine started: DRY-RUN (watching only) —                  │  RichLog #event-log (wrap on)
│ 20:39:01  Account-2 (john.doe@gmail.com): 76% used (switch at 90%) | others: #3: 5h 12% · 7d 40%
│ 20:40:01  no switch: below-threshold                                   │
│ 20:41:03  [dry-run] would switch Account-2 -> Account-3 (john.doe@company.com) (proactive)
│                                                                        │
│ l Go live / dry-run  t Threshold  esc Back  ^t Theme                   │  Footer (← → +1%/-1%, enter Done appear only while adjusting)
└────────────────────────────────────────────────────────────────────────┘
```

Mode badge (`#mode-badge`, bold): `" DRY-RUN "` → bg `$panel`, fg `$warning`; `" LIVE "` → bg `$primary` (accent), fg `$background` (solid accent block = loudest signal that switching is armed).

Summary line (`#auto-summary`, muted): `"auto-switch · "` + `"threshold {pct_label(t)}%"` (accent-colored while adjusting) + `" (session)"`[muted] if `t != file value` + `" · poll every {interval_seconds:.0f}s"` + `"   ← → adjust · enter done"`[muted] while adjusting. `pct_label(v) = f"{v:.10g}"` (so 99.9 never shows as 100).

Lifecycle:
- mount: `app.set_store_only(True)` (the engine is the only fetcher while open); `settings = load_settings()` fresh; `configured_threshold = settings.threshold`; `app.threshold_pct = settings.threshold`; start engine dry-run.
- unmount: `engine.stop()`; `switcher.clear_poll_policy_inputs()`; `app.threshold_pct = configured_threshold`; `app.set_store_only(False)`.
- engine events arrive on the worker thread and are marshalled to the UI; dropped if the screen is gone. A `switch` event also triggers `request_refresh()`.

Event log line = `"{HH:MM:SS}  "`[muted] + `event.human()` colored by kind:

| kind | color | `human()` text |
|---|---|---|
| `switch` | accent | `Switched Account-2 -> Account-3 (email) (proactive)` / `[dry-run] would switch …` (trigger ∈ proactive, at-limit, failover, consume-first) |
| `error` | warn | `error: {message}` + ` (will retry)` if transient |
| `account-quarantined` | warn | `Account-{n} ({email}) quarantined: {reason}. Log in with it and run 'cswap --add-account --slot {n}' to recover.` |
| `all-exhausted` | crit | `all accounts exhausted; earliest reset {iso}` / `…; no reset time known` |
| `poll`, `no-switch`, `sleep`, `account-unquarantined` | muted | `Account-{n} ({email}): 76% used (switch at 90%) \| others: #3: 5h 12% · 7d 40%` (or `usage unknown (http-429)`, `poll: no active account`); `no switch: {reason} ({detail})`; `sleeping 5m (until …)`; `Account-{n} ({email}) back in rotation (credentials-replaced)` |
| anything else (`config-warning`) | fg | `warning: {message}` |

System lines written by the screen itself (muted): `— engine started: DRY-RUN (watching only) —` / `— engine started: LIVE (will switch accounts) —` / `— threshold set to {pct_label}% for this session —`.

Candidates (`#candidates`): header `Next best`[muted]; for every account that is not active and `switchable`: `"\n  {number:>2}  {email}"` + one of `"  {sentinel_label}"`[muted] (sort key 998), `"  usage unknown"`[muted] (999), `"  {pct:3.0f}% used"`[severity] (key = pct). Sorted ascending (best headroom first, ties by number). pct = `binding_pct(last_good, parse_model_names(settings.model))` — the **same window axis the engine uses** (max of 5h/7d plus any configured per-model scoped windows). None → `"\n  no other switchable accounts"`[muted].

Threshold adjust mode (session-only): `t` toggles; `←`/`→` step ±1 within `SETTING_SPECS["autoswitch.threshold"]` = **[50.0, 99.9]**; each step updates `settings` (in memory), `engine.apply_threshold(v)` (re-pins poll policy), `app.threshold_pct` (bar ticks move live), repaints panel + summary. Leaving adjust mode (`Enter`, `t`, or `Esc`) with a **net change** → `engine.wake()` + the "threshold set" log line; no net change → silent.

Go live: `l` while dry-run → `ConfirmModal("Go live? claude-swap will switch your active account automatically when the threshold is reached.\n\n(Same behavior as running `cswap auto` in a terminal.)", title="Go live", yes_label="Go live")`; on confirm the engine is stopped and a **new** engine constructed from the in-memory settings (session threshold carried over) with `dry_run=False`. `l` while live → back to dry-run with **no confirmation**.

| key | action | footer |
|---|---|---|
| `l` | toggle live/dry-run (confirm when going live) | **Go live / dry-run** |
| `t` | enter/leave threshold adjust | **Threshold** |
| `←` / `→` | threshold −1% / +1% (only while adjusting) | **-1%** / **+1%** (hidden otherwise) |
| `Enter` | finish adjusting (only while adjusting) | **Done** (hidden otherwise) |
| `Esc`, `q` | leave adjust mode if adjusting, else pop screen | **Back** |

Settings the screen reads (`settings.json`, camelCase, section `autoswitch`): `threshold` 90.0 [50,99.9], `intervalSeconds` 60 [15,3600], `cooldownSeconds` 300, `hysteresisPct` 10, `strategy` best|consume-first, `includeApiKeyAccounts` false, `unhealthyTicks` 3, `model` (comma list of scoped window names, or null). The TUI never writes any of these.

---

## 8. Modals (`tui/modals.py`, `cswap.tcss`)

Common chrome: centered box, scrim = `$background` at 60 %, box width 64 (max 90 %), max-height 80 %, bg `$surface`, rounded border `$panel`, padding 1 2. Title bold accent; body fg; buttons right-aligned flat chips (bg `$panel`, min-width 12, padding 0 2, margin-left 2) — the **focused** button becomes a solid accent block (bg `$primary`, fg `$background`, bold) so the accent shows where Enter lands; hint line muted. Mouse clicks work.

### 8.1 ConfirmModal(message, title="Confirm", yes_label="Yes") → bool
```
╭──────────────────────────────────────────────────────────────╮
│ Remove account                                    [bold accent]
│                                                              │
│ Remove account 3 (john.doe@company.com)?                     │
│                                                              │
│ Its stored credentials and config backup are deleted.        │
│                                                              │
│                              [ Remove ]    [ Cancel ]        │
│ ← → · enter  ·  y remove  ·  n / esc cancel          [muted] │
╰──────────────────────────────────────────────────────────────╯
```
Keys: `y` → True; `n`, `Esc` → False; `←`/`→` move focus between buttons; `Enter` presses the focused button; click. Only an explicit True runs the follow-up. Hint = `"← → · enter  ·  y {yes_label.lower()}  ·  n / esc cancel"`.

Instances:
| title | yes | message |
|---|---|---|
| `Remove account` | `Remove` | `Remove account {n} ({email})?\n\nIts stored credentials and config backup are deleted.` |
| `Add account` | `Add` | `Back up the current Claude Code login as a managed account?\n\nIf this account is already managed, its stored credentials are refreshed in place.` |
| `Overwrite slot` | `Overwrite` | `Slot {slot} is occupied by {email}. Overwrite?` |
| `Go live` | `Go live` | `Go live? claude-swap will switch your active account automatically when the threshold is reached.\n\n(Same behavior as running \`cswap auto\` in a terminal.)` |

### 8.2 AddTokenModal() → TokenForm{token, email|None, slot|None} | None
```
╭──────────────────────────────────────────────────────────────╮
│ Add account from token                                       │
│ OAuth setup-token (sk-ant-oat…) or managed API key           │
│ (sk-ant-api…); the type is auto-detected.                    │
│ [••••••••••••••••••••••••••••••••••••••••]  token (required) │  Input password=True
│ [                                        ]  email label (optional)
│ [                                        ]  slot number (optional)   Input type=integer
│ Token is required.                                    [$error, 1 line, empty until a validation fails]
│                                 [ Add ]    [ Cancel ]        │
│ enter add  ·  tab next field  ·  esc cancel                  │
╰──────────────────────────────────────────────────────────────╯
```
Keys: `Tab`/`Shift+Tab` move between fields/buttons; `Enter` in any input submits; `Esc` cancels (None); `←`/`→` move between buttons only when a button is focused (inputs consume them for cursor movement). Validation order: empty token → `Token is required.`; slot non-integer → `Slot must be a number.`; slot < 1 → `Slot must be >= 1.`; email trimmed, empty → None.

### 8.3 OutputModal(title, output) → None
Wide box (width 90). Title (e.g. `Add current login` or `Remove account 3 — failed`), scrollable region (max-height 20, bg `$background`, padding 1) showing the captured ANSI-colored CLI output rendered with its colors (`(no output)` if blank), one `Close` button, hint `esc close`. Keys: `Esc`, `q`, `Enter`, click → close.

---

## 9. Key binding master table

| screen | key | effect | confirm? |
|---|---|---|---|
| all | `ctrl+t` | cycle theme dark → light → auto; toast `Theme: {name}`; persisted to `ui.theme` | no |
| Dashboard | `↑/↓ j/k` | move menu cursor | |
| | `Enter`/click | activate menu row | |
| | `Esc`/`←` | menu back | |
| | `s` | Switch screen | |
| | `w` | Watch screen | |
| | `g` | Auto screen | |
| | `f` | full refresh (toast `Refreshing usage…`) | |
| | `q` / menu Quit | exit app | no |
| | menu Switch account… | Switch screen | |
| | menu Add account… › From current Claude Code login | `add_account()` | **yes** (Add account) |
| | menu Add account… › From a setup-token / API key… | AddTokenModal → `add_account_from_token` | form; **Overwrite slot** confirm if occupied |
| | menu Disable / enable account… › row | toggle `disabled`, pop to root | **no** |
| | menu Remove account… › row | `remove_account` | **yes** (Remove account) |
| | menu Theme… › dark/light/auto | apply + persist theme | no |
| Switch | `↑/↓ j/k` | move cursor | |
| | `Enter`/click | switch to highlighted, pop | no |
| | `b` | switch best | no |
| | `Esc`/`q`/`s` | back | |
| Watch | `s` | arm/disarm selection | |
| | `Enter`/click (armed) | switch, disarm, stay | no |
| | `↓/j ↑/k` | scroll (monitor) / cursor (armed) | |
| | `f` | full refresh | |
| | `Esc`/`q` | disarm, else back | |
| Auto | `l` | live ⇄ dry-run | **yes when going live** (Go live) |
| | `t` | threshold adjust on/off | |
| | `←`/`→` (adjusting) | −1 / +1 % | |
| | `Enter` (adjusting) | done | |
| | `Esc`/`q` | leave adjust, else back | |
| ConfirmModal | `y` / `n` / `Esc` / `←→` / `Enter` / click | see §8.1 | |
| AddTokenModal | `Tab` / `Enter` / `Esc` / `←→` on buttons | see §8.2 | |
| OutputModal | `Esc` / `q` / `Enter` / click | close | |

Not available in the TUI (CLI-only in cswap): rotate-to-next, next-available strategy, alias set/unset, move slot, login/refresh credentials, auto start/stop as a daemon (the TUI hosts its own engine instance instead; the macOS menu bar has a persisted auto on/off toggle, the TUI does not).

---

## 10. Theme, colors, footer, notifications

### 10.1 Palette roles (theme.py)

| role | dark (`cswap-dark`) | light (`cswap-light`) | used for |
|---|---|---|---|
| accent / primary | `#d7875f` terracotta (xterm 173, = CLI accent) | `#954c2a` | alias names, `● active`, footer key chips, highlighted-row left border, modal titles, focused buttons, LIVE badge bg, `switch` log lines, threshold text while adjusting, `●` current theme mark |
| foreground | `#e8e4de` | `#2b2723` | emails, numbers, menu labels, generic log lines |
| muted / secondary | `#8a8a8a` | `#635d55` | labels (`5h`), `[tag]`, `(disabled)`, ages, reset suffixes, breadcrumb, list titles, hints, `← back`, sentinel labels for api-key, quiet log kinds, timestamps |
| background | `#141414` | `#faf7f2` | screen |
| surface | `#1e1e1e` | `#efeae1` | highlighted list row bg, modal box bg |
| panel | `#262626` | `#e2dbcf` | panel border, flash row bg, DRY-RUN badge bg, button rest bg |
| sev_ok / success | `#87af87` | `#3d6b3d` | pct < 70 |
| sev_warn / warning | `#d7af5f` | `#795911` | 70 ≤ pct < 90; ┃ tick; ⚠ sentinel; `(ahead)`; error/quarantine log; DRY-RUN badge fg |
| sev_crit / error | `#d75f5f` | `#ad3128` | pct ≥ 90; `Fable (!)` in minis; all-exhausted log; form error |
| track | `#3a3a3a` | `#cec7ba` | unfilled bar `─`, `" · "` separators in minis |

`severity(pct)`: None → muted; `>= 90` → crit; `>= 70` → warn; else ok. (`CRIT_PCT` deliberately equals the default auto-switch threshold.)

Textual theme variables: `footer-key-foreground = accent`, `block-cursor-background = panel`, `block-cursor-foreground = foreground`, `block-cursor-text-style = none`.

### 10.2 Layout chrome (cswap.tcss)
- `#accounts-panel`: padding `1 3`, border-bottom solid `$panel`. `#auto-active-panel`: padding `1 3 0 3`, no border.
- `#menu-title` / `#list-title`: height 1, margin-top 1, padding `0 3`, muted.
- `#menu`, `#accounts`: padding `1 2 0 1`, 1-cell scrollbar. Items: padding `0 1`, `border-left: thick $background` at rest; highlighted → fg, bg `$surface`, `border-left: thick $primary`. `#accounts ListItem` margin-bottom 1; `.flash` bg `$panel`.
- `#auto-top` padding `1 2`; `#auto-title-row` height 1, margin-bottom 1; `#auto-summary` padding-left 2 muted; `#event-log` border-top solid `$panel`, padding `0 1`.

### 10.3 Theme selection
- Setting `ui.theme` ∈ {dark, light, auto} (default auto). `auto` resolves to the pre-driver terminal detection, else `dark`; never re-probed mid-session.
- `apply_theme(name)`: set Textual theme `cswap-{resolved}`, `printer.set_theme(resolved)` (captured CLI output matches), persist via `set_setting(backup_dir, "ui.theme", name)`; persistence failure → toast `Could not save theme: {exc}` (warning).
- `ctrl+t` cycles `dark → light → auto → dark`.

### 10.4 Footer
Textual's standard Footer on every screen: one chip per **visible** binding, ` key ` in accent + description, in declaration order; hidden bindings (`show=False` or `check_action` False) omitted. Per screen: Dashboard `s Switch accounts · w Watch · q Quit · ^t Theme`; Switch `enter Switch · b Best pick · esc Back · ^t Theme`; Watch `s Switch · [enter Confirm when armed] · esc Back · ^t Theme`; Auto `l Go live / dry-run · t Threshold · [← -1% · → +1% · enter Done when adjusting] · esc Back · ^t Theme`. Modals have no footer (hint line instead).

### 10.5 Notifications (Textual toasts, bottom-right, default ~5 s)

| text | title | severity | timeout |
|---|---|---|---|
| `Switched to {email}` / `Switched to account {n}` | Switch | info | default |
| `{reason}` / `no switch performed` | No switch | warning | default |
| `Another action is still running` | — | warning | default |
| `Action failed: {err}` | — | error | default |
| `Refresh failed: {msg}` / `Store refresh failed: {msg}` | — | warning | 6 s |
| `Auto-switch engine stopped: {err}` | — | error | default |
| `Refreshing usage…` | — | info | 2 s |
| `Theme: {name}` | — | info | default |
| `Could not save theme: {exc}` | — | warning | default |
| first line of captured output (e.g. disable/enable/remove result) | — | info | default |

Numeric constants: POLL_INTERVAL_S 3.0 · SNAPSHOT_AGE_NOTE_S 60 · FLASH_S 1.5 · SERVE_TTL_S 180 (age note) · STALE_OK_S 300 (dim) · WARN 70 / CRIT 90 · bar width [12, 30] · threshold clamp [50.0, 99.9] · pace AHEAD 15 pt, suppress 24 h.

---

## 11. Claude-specific items needing a Codex mapping

| cswap (Claude) | where | cswitch (Codex) equivalent / decision needed |
|---|---|---|
| Windows `five_hour` (5h) / `seven_day` (7d) with `pct`, `resets_at` ISO | rows, minis, candidates | `UsageInfo.primary` (5h) / `secondary` (weekly) with `used_percent`, `resets_at: i64` epoch, `window_minutes` (src/usage/mod.rs:37-40, 83-84). Labels could stay `5h`/`7d`, or derive from `window_minutes`. |
| `spend` row (`$$`, `$used / $limit`) | card row 1 | Codex has `credits_balance` / `unlimited_credits` and `SpendControlLimit` (mod.rs:44-50); map to a credits row or drop. |
| `scoped` per-model weekly windows (`Fable`, `Opus`, `(!)` at 100 %) | rows, minis, candidates, `autoswitch.model` | Codex `AdditionalRateLimit` (mod.rs:56-62) is the nearest analogue; otherwise no per-model rows. |
| `(ahead of pace)` / `(ahead)` (weekly pace vs. elapsed week) | 7d + scoped rows | Applies to the weekly window only; needs `fetched_at` + `resets_at`. |
| `display_tag` = org name or `personal` | headers, submenus | Codex: `plan_type` (plus/pro/team…) or workspace/org name. |
| `kind: oauth \| api_key`; sentinel `api key` → `API key (no quota)` | sentinel branch, candidates | Codex API-key / custom provider profiles (src/provider.rs) have no rate-limit windows → same "no quota" treatment. |
| Sentinels `token expired` (Claude Code refreshes it), `re-login needed` (…run: cswap add), `keychain unavailable`, `foreign credential` | sentinel branch | Map to cswitch `TerminalAuthError` / refresh-token failure states; wording must reference `cswitch login`, not `cswap add`; no macOS Keychain state (Codex stores `~/.codex/auth.json`). |
| Empty-state text "…from your current Claude Code login, or from a setup-token / API key." | AccountsPanel | Reword to Codex login / import flow. |
| Add menu: `From current Claude Code login`, `From a setup-token / API key…`; token modal body `sk-ant-oat… / sk-ant-api…` | dashboard, modal | Codex: "From current Codex login (~/.codex/auth.json)", "Import auth.json…" or device login; no sk-ant prefixes. |
| Confirm text "Back up the current Claude Code login…" and "Go live? claude-swap will switch…(Same behavior as running `cswap auto`…)" | modals | Rename to cswitch / `cswitch auto` equivalent. |
| Slot **numbers** as primary identity (`{number:>2}`, `remove:{n}`, `Slot 3 is occupied`) | everywhere | cswitch profiles are keyed by **alias** (`Use { alias }`, `Rename`, `Delete`); decide whether to show an index column or alias-first rows. |
| Engine text `Account-{n} ({email})`, `cswap --add-account --slot {n}` recovery hint, triggers proactive/at-limit/failover/consume-first | event log | Depends on cswitch's auto-switch engine naming and its "reset card" concept (`ResetCard`, `consume_reset_credit`). |
| Switch strategies: `best` only in TUI (`b`) | Switch screen | cswitch "unified scoring" auto-select (`Use` with no alias) is the analogue of best pick. |
| `disabled` flag ("held out of auto-rotation") | minis, submenu | Needs a cswitch profile field if not present. |
| macOS ~30 s Keychain latency rationale, `~/.claude.json` active detection | comments only | Not applicable; Codex switch = rewrite auth.json + restart app-server daemon (see recent commit 7e57076). |
| `printer.force_color()` + ANSI capture of CLI output into OutputModal | actions | Rust: return structured results/log lines from commands; render in the modal without ANSI round-tripping (deliberate departure). |
| OSC 11 terminal-background probe before driver start, `ui.theme` auto | appearance.py | ratatui: probe before entering raw mode (same constraints: skip in tmux/screen, TERM=dumb, non-tty; 1 s cap; drain stdin after). |

---

## 12. Behavioral invariants worth test-asserting in the port

1. Opening the Auto view never switches by itself (dry-run first, confirm to go live; back to dry-run unconfirmed).
2. Session threshold and dry/live state are never written to settings; unmount restores the file threshold.
3. Remove always confirms; disable/enable never confirms and returns to the root menu.
4. Switch screen pops immediately on Enter; Watch screen stays and disarms.
5. Snapshot updates with an unchanged account set never move the list cursor; changed set → rebuild, cursor clamped; first build → cursor on the active account.
6. Rows only for windows the account has; sentinel replaces bars entirely; api-key never gets a "last seen" line.
7. Reset countdowns/clocks are recomputed from `resets_at` at render time; clock variant shown per row only where it fits.
8. Bar dimming at age > 300 s; header age note at age ≥ 180 s; watch title staleness note at snapshot age ≥ 60 s.
9. ┃ tick only on the dashboard/auto panel card (list cards pass no threshold).
10. Only one mutating action at a time; every action end triggers a refresh; switch results are toasts, add results are output modals.

---

## 13. Where the Python has moved past the Go doc (trust the Python)

- Two refresh lanes (normal + store-only) with generation-ordered merge; the Go doc describes a single `_refreshing` lane.
- `refresh_status` / `SNAPSHOT_AGE_NOTE_S` and the 1 s status timer (watch-title `snapshot 1m ago · refreshing 5s`) are new.
- Light theme (`cswap-light`), `ui.theme` setting, `Theme…` menu entry, `ctrl+t`, `Palette.from_theme`, terminal background detection (appearance.py) are new; the Go doc says "only one theme".
- Store-refresh error toast wording (`Store refresh failed:`) is new.
- SENTINEL_NOTES wording for `token expired` changed to `token expired — refresh deferred this pass; retries automatically`; `foreign credential` sentinel added.
- `usage_rows` takes `fetched_at` and adds `(ahead of pace)` / `(ahead)` pace markers (issue #125).
- `SnapshotSource` now reconciles per-account regressions (no `fetched_at` going backwards, sticky token-expired).
