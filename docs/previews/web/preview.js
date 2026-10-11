"use strict";

// Fictional accounts only. This preview never reads or changes live credentials.
const accounts = [
  { slot: 1, provider: "codex", name: "Daily driver", email: "daily@example.com", plan: "Pro", five: 38, week: 24, fiveReset: "2h 14m", weekReset: "4d 8h", status: "ready" },
  { slot: 2, provider: "codex", name: "Team workspace", email: "team@example.com", plan: "Team", five: 12, week: 17, fiveReset: "4h 32m", weekReset: "5d 2h", status: "ready" },
  { slot: 3, provider: "codex", name: "Sandbox", email: "sandbox@example.com", plan: "Plus", five: null, week: null, status: "disabled" },
  { slot: 4, provider: "claude", name: "Work", email: "work@example.com", plan: "Max", five: 86, week: 41, fiveReset: "1h 08m", weekReset: "3d 6h", status: "ready" },
  { slot: 5, provider: "claude", name: "Personal", email: "personal@example.com", plan: "Max", five: 18, week: 32, fiveReset: "3h 46m", weekReset: "4d 1h", status: "ready" },
  { slot: 6, provider: "claude", name: "Backup", email: "backup@example.com", plan: "Pro", five: null, week: null, status: "relogin" },
];

const usageSamples = [
  [[38, 24], [12, 17], [86, 41], [18, 32]],
  [[43, 26], [15, 19], [88, 44], [23, 34]],
  [[48, 29], [18, 22], [89, 48], [28, 37]],
  [[34, 22], [10, 14], [83, 39], [16, 29]],
];
const state = { active: { codex: 1, claude: 4 }, filter: "all", query: "", sort: "slot", auto: true, events: [], target: null, switching: false, sampleIndex: 0 };
let motionReady = false;
const $ = (selector) => document.querySelector(selector);
const icon = (name, small = false) => `<svg class="icon${small ? " small" : ""}" aria-hidden="true"><use href="#i-${name}"/></svg>`;
const escapeText = (value) => String(value).replace(/[&<>"']/g, (char) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[char]);
const providerName = (provider) => provider === "codex" ? "Codex" : "Claude Code";
const accountBySlot = (slot) => accounts.find((account) => account.slot === slot);
const isActive = (account) => state.active[account.provider] === account.slot;
const mark = (provider) => `<span class="provider-mark ${provider}">${icon(provider)}</span>`;

function mountMotion(root = document) {
  if (motionReady) Bitmap.mount(root);
}

function metric(value, key, label, reset) {
  const previous = Bitmap.previous(key);
  const delta = previous === null ? null : value - previous;
  const change = delta === null ? "INITIAL READING" : delta === 0 ? "NO CHANGE" : `${delta > 0 ? "+" : "−"}${String(Math.abs(delta)).padStart(2, "0")} PTS / LAST READING`;
  return `<div><p class="usage-label">${label}</p><div class="metric-top"><p class="usage-number">${Bitmap.number(value, key)}<span class="percent">%</span></p><span class="metric-unit">USED</span></div><p class="metric-delta${delta ? " changed" : ""}">${change}</p>${Bitmap.chart(value, key)}<p class="reset-time">RESET IN ${reset}</p></div>`;
}

function applyUsageSample() {
  accounts.filter((account) => account.five !== null).forEach((account, index) => {
    [account.five, account.week] = usageSamples[state.sampleIndex][index];
  });
}

function seedEvents() {
  const now = Date.now();
  return [
    { title: "Usage snapshot updated", detail: "6 sample accounts · 2 providers", time: now, icon: "refresh" },
    { title: "Claude switched to Work", detail: "Personal → Work · manual switch", time: now - 12 * 60000, icon: "switch" },
    { title: "Auto-switch started", detail: "Claude Code · 90% usage threshold", time: now - 47 * 60000, icon: "bolt" },
  ];
}

function timeLabel(timestamp) {
  const minutes = Math.floor((Date.now() - timestamp) / 60000);
  if (minutes < 1) return "Just now";
  if (minutes < 60) return `${minutes}m ago`;
  return `${Math.floor(minutes / 60)}h ago`;
}

function renderActiveCards() {
  $("#active-cards").innerHTML = ["codex", "claude"].map((provider) => {
    const account = accountBySlot(state.active[provider]);
    const note = provider === "codex" ? "Switch may restart running sessions" : "Keychain · applies in about 30 seconds";
    return `<article class="active-card dither-card" aria-label="Active ${providerName(provider)} account">
      <div class="row between"><div class="row">${mark(provider)}<span class="provider-name">${providerName(provider)}</span></div><span class="active-tag"><span class="dot"></span>Active</span></div>
      <div class="active-identity"><h2>${account.name}</h2><span class="slot-tag">#${account.slot}</span></div>
      <p class="active-email">${account.email}<span style="padding:0 7px">/</span>${account.plan}</p>
      <div class="usage-grid">${metric(account.five, `active-${provider}-five`, "5-hour window", account.fiveReset)}${metric(account.week, `active-${provider}-week`, "7-day window", account.weekReset)}</div>
      <div class="card-footer"><span class="row card-note">${icon("clock", true)}${note}</span><button class="text-button" data-choose="${provider}" aria-label="Choose another ${providerName(provider)} account">Switch${icon("switch", true)}</button></div>
    </article>`;
  }).join("");
  mountMotion($("#active-cards"));
}

function bestCandidate(provider) {
  return accounts.filter((account) => account.provider === provider && account.status === "ready" && !isActive(account))
    .sort((left, right) => Math.max(left.five, left.week) - Math.max(right.five, right.week))[0];
}

function renderRecommendation() {
  const current = accountBySlot(state.active.claude);
  const candidate = bestCandidate("claude");
  const nearLimit = Math.max(current.five, current.week) >= 80;
  $("#recommendation").innerHTML = nearLimit && candidate
    ? `<div class="recommendation">${icon("alert")}<div class="recommendation-copy"><b>Claude ${current.name} is close to its limit.</b><span>${100 - current.five}% of the 5-hour window remains.</span></div><button class="text-button" data-switch="${candidate.slot}">Switch to ${candidate.name}${icon("arrow", true)}</button></div>`
    : `<div class="recommendation healthy">${icon("check")}<div class="recommendation-copy"><b>Room to keep going.</b><span>Your active accounts have quota available.</span></div><span class="mono" style="font-size:10px">ALL CLEAR</span></div>`;
}

function meter(value, reset, label, key) {
  if (value === null) return `<div class="mini-meter" data-label="${label}"><span class="muted mono" style="font-size:11px">—</span><span class="muted" style="font-size:9px;margin-left:6px">Unavailable</span></div>`;
  return `<div class="mini-meter" data-label="${label}"><div class="meter-value"><span>${Bitmap.number(value, key)}<span class="percent">%</span></span><small>${reset}</small></div>${Bitmap.chart(value, key, true)}</div>`;
}

function accountStatus(account) {
  if (isActive(account)) return `<span class="status active"><span class="dot"></span>Active</span>`;
  if (account.status === "disabled") return `<span class="status off"><span class="dot"></span>Disabled</span>`;
  if (account.status === "relogin") return `<span class="status blocked">${icon("alert", true)}Re-login</span>`;
  return `<span class="status ready"><span class="dot"></span>Ready</span>`;
}

function renderAccounts() {
  const query = state.query.toLocaleLowerCase().trim();
  const visible = accounts.filter((account) => (state.filter === "all" || account.provider === state.filter)
    && `${account.name} ${account.email} ${account.provider} ${account.slot} ${account.plan}`.toLocaleLowerCase().includes(query));
  if (state.sort === "headroom") visible.sort((a, b) => (a.five === null ? 101 : Math.max(a.five, a.week)) - (b.five === null ? 101 : Math.max(b.five, b.week)));
  if (state.sort === "name") visible.sort((a, b) => a.name.localeCompare(b.name));
  $("#account-rows").innerHTML = visible.length ? visible.map((account) => {
    const action = isActive(account) ? `<span class="current-label">${icon("check", true)}Current</span>`
      : account.status === "ready" ? `<button class="button switch-button" data-switch="${account.slot}" aria-label="Switch to ${account.name}">Switch${icon("arrow", true)}</button>`
      : `<button class="text-button" data-info="${account.slot}">${account.status === "disabled" ? "Details" : "How to fix"}${icon("arrow", true)}</button>`;
    return `<tr><td><div class="account-cell">${mark(account.provider)}<div class="account-info"><div class="account-name">${account.name}<span class="account-slot">#${account.slot}</span><span class="plan">· ${account.plan}</span></div><div class="account-email">${account.email}</div></div></div></td><td>${meter(account.five, account.fiveReset, "5-hour usage", `account-${account.slot}-five`)}</td><td>${meter(account.week, account.weekReset, "7-day usage", `account-${account.slot}-week`)}</td><td>${accountStatus(account)}</td><td>${action}</td></tr>`;
  }).join("") : `<tr class="empty"><td colspan="5">No accounts match “${escapeText(state.query)}”.</td></tr>`;
  $("#result-count").textContent = state.filter === "all" && !query ? "6 accounts across 2 providers" : `${visible.length} of 6 accounts shown`;
  document.querySelectorAll("[data-filter]").forEach((button) => button.setAttribute("aria-pressed", String(button.dataset.filter === state.filter)));
  mountMotion($("#account-rows"));
}

function renderActivity() {
  const latest = state.events[0];
  $("#latest-activity").innerHTML = `<span class="event-icon">${icon(latest.icon, true)}</span><div class="event-copy"><p>${escapeText(latest.title)}</p><small>${escapeText(latest.detail)}</small></div><span class="muted" style="font-size:10px;white-space:nowrap">${timeLabel(latest.time)}</span>`;
  $("#activity-list").innerHTML = state.events.map((event) => `<li><span class="event-icon">${icon(event.icon, true)}</span><div class="event-copy"><p>${escapeText(event.title)}</p><small>${escapeText(event.detail)}</small></div><time class="event-time" datetime="${new Date(event.time).toISOString()}">${timeLabel(event.time)}</time></li>`).join("");
}

function recordEvent(title, detail, eventIcon = "switch") {
  state.events.unshift({ title, detail, icon: eventIcon, time: Date.now() });
  state.events = state.events.slice(0, 50);
  renderActivity();
}

function renderAutomation() {
  $("#auto-state").textContent = state.auto ? "Watching" : "Paused";
  $("#auto-state").classList.toggle("paused", !state.auto);
  $("#auto-toggle").setAttribute("aria-checked", String(state.auto));
  $("#auto-description").textContent = state.auto ? "Switch to an available Claude account before the limit." : "Manual switching is still available.";
}

function render() {
  renderActiveCards();
  renderRecommendation();
  renderAccounts();
  renderActivity();
  renderAutomation();
  mountMotion();
}

let toastTimer;
function toast(message) {
  clearTimeout(toastTimer);
  $("#toast").innerHTML = `${icon("check", true)}<span>${escapeText(message)}</span>`;
  $("#toast").hidden = false;
  toastTimer = setTimeout(() => { $("#toast").hidden = true; }, 4500);
}

function showSwitch(slot) {
  const account = accountBySlot(slot);
  if (!account || account.status !== "ready" || isActive(account) || state.switching) return;
  const from = accountBySlot(state.active[account.provider]);
  const isCodex = account.provider === "codex";
  state.target = slot;
  $("#switch-content").innerHTML = `<div class="dialog-top">${mark(account.provider)}<button class="icon-button" data-close-dialog aria-label="Close dialog">${icon("close", true)}</button></div>
    <h2 id="switch-title">Switch ${providerName(account.provider)} account?</h2><p class="dialog-subtitle" id="switch-description">This changes the default login on j-studio.</p>
    <div class="switch-route"><div><small>From</small><strong>${from.name} <span class="muted">#${from.slot}</span></strong></div>${icon("arrow")}<div><small>To</small><strong>${account.name} <span class="muted">#${account.slot}</span></strong></div></div>
    <div class="switch-impact${isCodex ? " warning" : ""}">${isCodex ? "If the Codex app-server daemon is running, ccsw restarts it. A turn in progress will be interrupted. Restart any running codex exec or --no-daemon sessions yourself." : "With Keychain credentials, Claude Code picks up the account in about 30 seconds. Restart Claude Code to apply it immediately. With file credentials, it applies on the next message."}</div>
    ${isCodex ? '<label class="impact-check"><input type="checkbox" id="acknowledge-impact">I understand that a running Codex turn may be interrupted.</label>' : ""}
    <p class="dialog-demo">Preview only. This switch changes sample data in your browser.</p>`;
  $("#confirm-switch").hidden = false;
  $("#confirm-switch").disabled = isCodex;
  $("#confirm-switch").innerHTML = `Switch account${icon("arrow", true)}`;
  $("#cancel-switch").textContent = "Cancel";
  $("#cancel-switch").disabled = false;
  $("#switch-dialog").showModal();
  $("#cancel-switch").focus();
}

function showAccountInfo(slot) {
  const account = accountBySlot(slot);
  state.target = null;
  const disabled = account.status === "disabled";
  $("#switch-content").innerHTML = `<div class="dialog-top">${mark(account.provider)}<button class="icon-button" data-close-dialog aria-label="Close dialog">${icon("close", true)}</button></div><h2 id="switch-title">${disabled ? "Account disabled" : "Sign in again on j-studio"}</h2><p class="dialog-subtitle" id="switch-description">${account.name} · #${account.slot}</p><p class="connection-note">${disabled ? "This account is excluded from switching. Enable it on the host to make it available again." : "This account needs a new login. On the host, open Claude Code, use /login, then save the updated credentials."}</p><pre>${disabled ? `ccsw enable ${account.slot}` : "ccsw add claude"}</pre><p class="dialog-demo">These are instructions for the proposed console. This preview uses fictional accounts.</p>`;
  $("#confirm-switch").hidden = true;
  $("#cancel-switch").textContent = "Got it";
  $("#cancel-switch").disabled = false;
  $("#switch-dialog").showModal();
}

function closeDialog() {
  if (state.switching) return;
  $("#switch-dialog").close();
  state.target = null;
}

async function confirmSwitch() {
  if (state.target === null || state.switching || $("#confirm-switch").disabled) return;
  const target = accountBySlot(state.target);
  const from = accountBySlot(state.active[target.provider]);
  state.switching = true;
  $("#confirm-switch").disabled = true;
  $("#confirm-switch").textContent = "Switching…";
  $("#cancel-switch").disabled = true;
  await new Promise((resolve) => setTimeout(resolve, 650));
  state.active[target.provider] = target.slot;
  recordEvent(`${providerName(target.provider)} switched to ${target.name}`, `${from.name} → ${target.name} · preview switch`);
  state.switching = false;
  closeDialog();
  render();
  document.querySelector(`[data-choose="${target.provider}"]`).focus();
  toast(`Preview: ${providerName(target.provider)} now uses ${target.name}.`);
}

function route() {
  const requested = window.location.hash.slice(1);
  const page = ["accounts", "activity", "connection"].includes(requested) ? requested : "accounts";
  for (const name of ["accounts", "activity", "connection"]) $("#page-" + name).hidden = name !== page;
  document.querySelectorAll("[data-page]").forEach((link) => {
    if (link.dataset.page === page) link.setAttribute("aria-current", "page");
    else link.removeAttribute("aria-current");
  });
  $("#breadcrumb").textContent = page[0].toUpperCase() + page.slice(1);
  window.scrollTo(0, 0);
}

document.addEventListener("click", (event) => {
  const button = event.target.closest("button");
  if (!button) return;
  if (button.dataset.switch) showSwitch(Number(button.dataset.switch));
  if (button.dataset.info) showAccountInfo(Number(button.dataset.info));
  if (button.dataset.choose) {
    const candidate = bestCandidate(button.dataset.choose);
    if (candidate) showSwitch(candidate.slot);
  }
  if (button.hasAttribute("data-close-dialog")) closeDialog();
  if (button.dataset.filter) {
    state.filter = button.dataset.filter;
    renderAccounts();
  }
});

$("#search").addEventListener("input", (event) => { state.query = event.target.value; renderAccounts(); });
$("#sort").addEventListener("change", (event) => { state.sort = event.target.value; renderAccounts(); });
$("#cancel-switch").addEventListener("click", closeDialog);
$("#confirm-switch").addEventListener("click", confirmSwitch);
$("#switch-dialog").addEventListener("cancel", (event) => { if (state.switching) event.preventDefault(); });
$("#switch-dialog").addEventListener("change", (event) => {
  if (event.target.id === "acknowledge-impact") $("#confirm-switch").disabled = !event.target.checked;
});
$("#switch-dialog").addEventListener("click", (event) => {
  const bounds = event.currentTarget.getBoundingClientRect();
  if (event.target === event.currentTarget && (event.clientX < bounds.left || event.clientX > bounds.right || event.clientY < bounds.top || event.clientY > bounds.bottom)) closeDialog();
});
$("#auto-toggle").addEventListener("click", () => {
  state.auto = !state.auto;
  renderAutomation();
  recordEvent(`Auto-switch ${state.auto ? "resumed" : "paused"}`, "Claude Code · preview control", "bolt");
  toast(`Preview: auto-switch ${state.auto ? "resumed" : "paused"}.`);
});

let refreshTimer;
$("#refresh").addEventListener("click", () => {
  $("#refresh").classList.add("spin");
  $("#refresh").disabled = true;
  refreshTimer = setTimeout(() => {
    $("#refresh").classList.remove("spin");
    $("#refresh").disabled = false;
    $("#updated").textContent = "Sample data · just now";
    state.sampleIndex = (state.sampleIndex + 1) % usageSamples.length;
    applyUsageSample();
    renderActiveCards();
    renderAccounts();
    renderRecommendation();
    recordEvent("Usage snapshot updated", "6 sample accounts · new demo reading", "refresh");
    toast("Sample usage updated. Changes appear beside each reading.");
  }, 550);
});

$("#reset-demo").addEventListener("click", () => {
  clearTimeout(refreshTimer);
  $("#refresh").classList.remove("spin");
  $("#refresh").disabled = false;
  Object.assign(state, { active: { codex: 1, claude: 4 }, filter: "all", query: "", sort: "slot", auto: true, target: null, events: seedEvents(), sampleIndex: 0 });
  applyUsageSample();
  $("#search").value = "";
  $("#sort").value = "slot";
  render();
  toast("Demo reset to the original accounts.");
});

$("#replay-motion").addEventListener("click", () => Bitmap.replay());

$("#copy-address").addEventListener("click", async () => {
  const address = $("#preview-address").textContent;
  try {
    if (navigator.clipboard && window.isSecureContext) await navigator.clipboard.writeText(address);
    else {
      // HTTP tailnet addresses do not have the Clipboard API in all browsers.
      const field = document.createElement("textarea");
      field.value = address;
      field.style.cssText = "position:fixed;left:-10000px;top:0";
      document.body.appendChild(field);
      field.select();
      const copied = document.execCommand("copy");
      field.remove();
      $("#copy-address").focus();
      if (!copied) throw new Error("Clipboard unavailable");
    }
    toast("Preview address copied.");
  } catch {
    const range = document.createRange();
    range.selectNodeContents($("#preview-address"));
    const selection = window.getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
    toast("Address selected. Copy it with your browser.");
  }
});

document.addEventListener("keydown", (event) => {
  const editing = /INPUT|TEXTAREA|SELECT/.test(event.target.tagName) || event.target.isContentEditable;
  if (event.key === "/" && !editing && !event.metaKey && !event.ctrlKey && !event.altKey && !$("#switch-dialog").open) {
    event.preventDefault();
    if (location.hash !== "#accounts") location.hash = "accounts";
    route();
    $("#search").focus();
    $("#search").scrollIntoView({ block: "center" });
  }
});
window.addEventListener("hashchange", route);
$("#preview-address").textContent = location.protocol === "file:" ? "http://100.64.0.70:8771/" : `${location.origin}${location.pathname}`;
state.events = seedEvents();
render();
route();
Promise.all([
  document.fonts.load('48px "Geist Pixel Circle"'),
  document.fonts.load('44px "Geist Pixel Square"'),
]).then(() => { motionReady = true; mountMotion(); });
