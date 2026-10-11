"use strict";

(() => {
  function takePairingLink() {
    if (!location.hash.startsWith("#pair=")) return null;
    const code = new URLSearchParams(location.hash.slice(1)).get("pair") || "";
    // Fragments stay out of HTTP requests. Remove the code before starting any requests.
    history.replaceState(history.state, "", location.pathname + location.search);
    return code;
  }
  let pairingLink = takePairingLink();
  const $ = (selector) => document.querySelector(selector);
  const all = (selector) => Array.from(document.querySelectorAll(selector));
  const escape = (value) => String(value ?? "").replace(/[&<>"']/g, (char) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;"
  })[char]);
  const icon = (name, small = false) => '<svg class="icon' + (small ? " small" : "") +
    '" aria-hidden="true"><use class="icon-bitmap" href="#i-' + name +
    '"/><use class="icon-paper" href="#p-' + name + '"/></svg>';
  const providerName = (provider) => provider === "codex" ? "Codex" : "Claude";
  const nameOf = (account) => account.alias || account.email || "Account #" + account.slot;
  const dateText = (value) => {
    const date = new Date(typeof value === "number" ? value * 1000 : value);
    return Number.isNaN(date.getTime()) ? "Unknown" : date.toLocaleString();
  };
  const statusNames = {
    ok: "Ready", unavailable: "Usage unavailable", api_key: "API key",
    no_credentials: "No credentials", token_expired: "Token expired",
    relogin_required: "Sign in required", keychain_unavailable: "Unlock Keychain",
    fetch_failed: "Refresh failed", stale: "Stale usage"
  };
  const state = {
    session: null, payload: null, source: null, connected: false, lastEvent: 0,
    filter: "all", query: "", sort: "slot", selected: null, details: null,
    pending: null, resolving: false, submitting: false, checking: false, pairing: false
  };
  const regions = new Map();
  let toastTimer;
  let operationTimer;

  // Store only the pending request ID here. Keep credentials out of Web Storage.
  try { state.pending = sessionStorage.getItem("ccsw-operation"); } catch {}
  function setPending(id) {
    state.pending = id;
    try {
      if (id) sessionStorage.setItem("ccsw-operation", id);
      else sessionStorage.removeItem("ccsw-operation");
    } catch {}
  }

  class RequestError extends Error {
    constructor(message, status = 0, code = "offline") {
      super(message);
      this.status = status;
      this.code = code;
    }
  }

  async function request(path, method = "GET", body) {
    const headers = { Accept: "application/json" };
    if (body !== undefined) headers["Content-Type"] = "application/json";
    if (method !== "GET" && state.session) headers["X-CCSW-CSRF"] = state.session.csrfToken;
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 10000);
    try {
      const response = await fetch("/api/v1/" + path, {
        method, headers, credentials: "same-origin", cache: "no-store",
        body: body === undefined ? undefined : JSON.stringify(body), signal: controller.signal
      });
      const payload = response.status === 204 ? null : await response.json();
      if (!response.ok) {
        if (response.status === 401 && path !== "session") showPair("Your session ended. Pair this browser again.");
        throw new RequestError(payload?.error?.message || "The request did not complete.",
          response.status, payload?.error?.code);
      }
      return payload;
    } catch (error) {
      if (error instanceof RequestError) throw error;
      throw new RequestError("The host did not respond. Waiting for the connection to recover.");
    } finally {
      clearTimeout(timer);
    }
  }

  function toast(message) {
    $("#toast").textContent = message;
    $("#toast").hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { $("#toast").hidden = true; }, 5500);
  }

  function replaceRegion(selector, html) {
    if (regions.get(selector) === html) return;
    const root = $(selector);
    const focused = root.contains(document.activeElement) ? document.activeElement : null;
    const key = focused?.dataset.switch ? ["switch", focused.dataset.switch]
      : focused?.dataset.details ? ["details", focused.dataset.details] : null;
    root.innerHTML = html;
    regions.set(selector, html);
    Bitmap.mount(root);
    if (key) root.querySelector("[data-" + key[0] + '="' + key[1] + '"]')?.focus({ preventScroll: true });
  }

  function showPair(message = "") {
    state.source?.close();
    state.source = null;
    state.session = null;
    state.payload = null;
    state.connected = false;
    state.selected = null;
    state.details = null;
    clearTimeout(operationTimer);
    $("#switch-dialog").close();
    $("#details-dialog").close();
    $("#console").hidden = true;
    $("#pair-screen").hidden = false;
    $("#pair-error").textContent = message;
    $("#pair-error").hidden = !message;
    $("#pair-submit").disabled = false;
    $("#pair-submit").textContent = "Pair this browser";
    $("#pair-code").value = "";
    $("#toast").hidden = true;
    for (const selector of ["#active-cards", "#account-rows", "#activity-list", "#latest-activity"]) {
      $(selector).replaceChildren();
    }
    regions.clear();
  }

  function startSession(session) {
    state.session = session;
    state.connected = false;
    state.payload = null;
    $("#pair-screen").hidden = true;
    $("#console").hidden = false;
    const scope = session.readOnly ? "Read-only" : "View + switch accounts";
    all("[data-host]").forEach((element) => { element.textContent = session.host; });
    all("[data-scope]").forEach((element) => { element.textContent = scope; });
    all("[data-address]").forEach((element) => { element.textContent = new URL(session.origin).host; });
    $("#console-address").textContent = session.origin + "/";
    $("#session-expiry").textContent = dateText(session.expiresAt);
    $("#session-expiry").dateTime = new Date(session.expiresAt * 1000).toISOString();
    $("#transport").textContent = location.protocol === "https:" ? "HTTPS" : "HTTP · local / tailnet";
    route();
    render();
    openStream();
  }

  async function checkSession() {
    if (state.checking || state.pairing) return;
    state.checking = true;
    if (!state.session) $("#pair-submit").disabled = true;
    const code = pairingLink;
    pairingLink = null;
    try {
      const session = await request("session");
      if (!state.session) startSession(session);
    } catch (error) {
      if (error.status === 401) {
        showPair(state.session ? "Your session ended. Pair this browser again." : "");
        if (code !== null) await pairBrowser(code);
      }
      else if (!state.session) showPair(error.message);
    } finally {
      state.checking = false;
      if (!state.session) $("#pair-submit").disabled = false;
      if (pairingLink !== null) void checkSession();
    }
  }

  async function pairBrowser(code) {
    if (state.pairing) return;
    state.pairing = true;
    $("#pair-code").value = "";
    $("#pair-submit").disabled = true;
    $("#pair-submit").textContent = "Pairing…";
    $("#pair-error").hidden = true;
    try {
      if (!/^[a-f0-9]{64}$/.test(code)) throw new Error("Invalid pairing code. Generate a new link or enter the code from the host.");
      startSession(await request("session", "POST", { code }));
    } catch (error) {
      $("#pair-error").textContent = error.message;
      $("#pair-error").hidden = false;
    } finally {
      state.pairing = false;
      $("#pair-submit").disabled = false;
      $("#pair-submit").textContent = "Pair this browser";
      if (pairingLink !== null) void checkSession();
    }
  }

  function openStream() {
    state.source?.close();
    const source = new EventSource("/api/v1/events");
    state.source = source;
    source.addEventListener("snapshot", (event) => {
      if (state.source !== source || !state.session) return;
      try {
        const payload = JSON.parse(event.data);
        state.lastEvent = Date.now();
        state.connected = !payload.snapshot.stale;
        acceptPayload(payload);
      } catch {
        state.connected = false;
        renderConnection();
      }
    });
    source.addEventListener("unavailable", () => {
      if (state.source !== source) return;
      state.connected = false;
      state.lastEvent = Date.now();
      renderConnection();
    });
    source.onerror = () => {
      if (state.source !== source) return;
      state.connected = false;
      renderConnection();
      void checkSession();
    };
  }

  function acceptPayload(payload) {
    state.payload = payload;
    if (state.pending) {
      const operation = payload.operations.find((operation) => operation.id === state.pending);
      if (operation) acceptOperation(operation);
      else void resolveOperation();
    }
    render();
  }

  function writable() {
    return !!state.session && !state.session.readOnly && state.connected &&
      !!state.payload && !state.payload.snapshot.stale && !state.pending;
  }

  function renderConnection() {
    if (!state.session) return;
    const stale = !state.connected || !state.payload || state.payload.snapshot.stale;
    $("#connection-status").textContent = stale ? "RECONNECTING" : state.session.readOnly ? "LIVE / READ-ONLY" : "LIVE / PAIRED";
    $("#connection-banner").hidden = !stale;
    $("#connection-banner").textContent = state.payload
      ? "Updates paused. The last snapshot may be stale. Account switches are disabled until the host state is verified."
      : "Waiting for the host account state. Check the host credential store if this continues.";
    $("#refresh").disabled = !writable() || !!state.payload?.refreshing;
    $("#refresh").classList.toggle("refreshing", !!state.payload?.refreshing);
    $("#refresh span").textContent = state.payload?.refreshing ? "Refreshing…" : "Refresh usage";
    all("[data-switch]").forEach((button) => {
      const account = state.payload?.snapshot.accounts.find((row) => row.slot === Number(button.dataset.switch));
      button.disabled = !writable() || !account?.switchable || account.active;
    });
    validateDialog();
  }

  function relativeAge(account) {
    if (!account.fetchedAt) return "No usage reading";
    const seconds = Math.max(0, (Date.now() - new Date(account.fetchedAt).getTime()) / 1000);
    if (seconds < 60) return "Updated just now";
    if (seconds < 3600) return "Updated " + Math.floor(seconds / 60) + "m ago";
    if (seconds < 86400) return "Updated " + Math.floor(seconds / 3600) + "h ago";
    return "Updated " + Math.floor(seconds / 86400) + "d ago";
  }

  // Match the terminal's critical threshold on the unrounded reading.
  const usageClass = pct => Number.isFinite(pct) && pct >= 90 ? "usage-critical" : "";

  function metric(window, key, label, compact = false) {
    const known = window && Number.isFinite(window.pct);
    const value = known ? Math.max(0, Math.min(100, Math.round(window.pct))) : null;
    const number = known ? Bitmap.number(value, key) + '<span class="percent">%</span>' : "—";
    const reset = window?.countdown ? "Resets " + window.countdown : "Reset unavailable";
    const chart = known ? Bitmap.chart(value, key + "-chart", compact) : '<span class="unknown-chart">No current reading</span>';
    if (compact) {
      return '<div class="mini-meter" data-label="' + label + '"><div class="meter-value"><span class="' + usageClass(window?.pct) + '">' +
        number + '</span><small>' + escape(window?.countdown || "—") + "</small></div>" + chart + "</div>";
    }
    const prior = Bitmap.previous(key);
    const delta = known && prior !== null && value !== prior ? value - prior : null;
    return '<div><div class="usage-label">' + label + '</div><div class="metric-top"><div class="usage-number ' + usageClass(window?.pct) + '">' +
      number + '</div><span class="metric-unit">used</span></div><div class="metric-delta' +
      (delta !== null ? " changed" : "") + '">' + (delta === null ? " " : (delta > 0 ? "+" : "") + delta + " pp") +
      "</div>" + chart + '<div class="reset-time">' + escape(reset) + "</div></div>";
  }

  function activeCard(provider, snapshot) {
    const account = snapshot.accounts.find((row) => row.provider === provider && row.active);
    const unmanaged = snapshot.unmanaged.includes(provider);
    const title = '<div class="row between"><div class="row"><span class="provider-mark">' + icon(provider) +
      '</span><span class="provider-name">' + providerName(provider) + '</span></div><span class="active-tag">' +
      (account ? '<span class="dot"></span>Active' : unmanaged ? "Unmanaged" : "No login") + "</span></div>";
    if (!account) {
      return '<article class="active-card dither-card" data-provider="' + provider + '">' + title +
        '<div class="active-identity"><h2>' + (unmanaged ? "Save this login first" : "No active account") +
        '</h2></div><p class="card-empty">' + (unmanaged
          ? "Run ccsw add on the host to save the current login before switching remotely."
          : "Choose a saved account below, or add a login from the host.") + "</p></article>";
    }
    const key = "active-" + provider + "-" + account.slot;
    return '<article class="active-card dither-card" data-provider="' + provider + '">' + title +
      '<div class="active-identity"><h2>' + escape(nameOf(account)) + '</h2><span class="slot-tag">#' +
      account.slot + '</span></div><p class="active-email">' + escape(account.email) + " · " + escape(account.plan) +
      '</p><div class="usage-grid">' + metric(account.usage?.fiveHour, key + "-5h", "5-hour usage") +
      metric(account.usage?.sevenDay, key + "-7d", "7-day usage") +
      '</div><div class="card-footer"><span class="card-note">' + escape(relativeAge(account)) +
      '</span><button class="text-button" data-details="' + account.slot + '">Details' + icon("arrow", true) + "</button></div></article>";
  }

  function row(account) {
    const status = account.disabled ? "Disabled" : account.active ? "Active" : statusNames[account.usageStatus] || "Usage unavailable";
    const key = "row-" + account.provider + "-" + account.slot;
    const blocked = account.disabled || !account.switchable;
    return '<tr data-slot="' + account.slot + '"><td><div class="account-cell"><span class="provider-mark">' +
      icon(account.provider) + '</span><div class="account-info"><button class="account-name" data-details="' +
      account.slot + '">' + escape(nameOf(account)) + '</button><span class="account-slot">#' + account.slot +
      '</span><span class="plan">' + escape(account.plan) + '</span><p class="account-email" title="' +
      escape(account.email) + '">' + escape(account.email) + '</p></div></div></td><td>' +
      metric(account.usage?.fiveHour, key + "-5h", "5-hour usage", true) + "</td><td>" +
      metric(account.usage?.sevenDay, key + "-7d", "7-day usage", true) +
      '</td><td><span class="status account-status ' + (account.active ? "active" : blocked ? "off" : "ready") +
      '"><span class="dot"></span>' + escape(status) + "</span></td><td>" +
      (account.active ? '<span class="current-label">' + icon("check", true) + "Current</span>"
        : '<button class="button switch-button" data-switch="' + account.slot + '"' +
          (!writable() || blocked ? " disabled" : "") + ' aria-label="Switch to ' + escape(nameOf(account)) +
          '">' + icon("switch", true) + "Switch</button>") + "</td></tr>";
  }

  function renderAccounts() {
    const snapshot = state.payload?.snapshot;
    if (!snapshot) return;
    all("[data-account-count]").forEach((element) => {
      element.textContent = String(snapshot.accounts.length).padStart(2, "0");
    });
    replaceRegion("#active-cards", ["codex", "claude"].map((provider) => activeCard(provider, snapshot)).join(""));
    const query = state.query.trim().toLowerCase();
    const accounts = snapshot.accounts.filter((account) =>
      (state.filter === "all" || state.filter === account.provider) &&
      [account.alias, account.email, account.provider, "#" + account.slot, String(account.slot)]
        .some((value) => value?.toLowerCase().includes(query)));
    const usageRank = (account) => {
      const windows = [account.usage?.fiveHour?.pct, account.usage?.sevenDay?.pct].filter(Number.isFinite);
      return windows.length ? Math.max(...windows) : Infinity;
    };
    accounts.sort((a, b) => state.sort === "name" ? nameOf(a).localeCompare(nameOf(b)) :
      state.sort === "headroom" ? usageRank(a) - usageRank(b) || a.slot - b.slot : a.slot - b.slot);
    replaceRegion("#account-rows", accounts.length ? accounts.map(row).join("") :
      '<tr><td class="empty empty-row" colspan="5">' + (snapshot.accounts.length
        ? "No accounts match this filter." : "No saved accounts. Run ccsw add on the host to get started.") + "</td></tr>");
    $("#result-count").textContent = accounts.length + " of " + snapshot.accounts.length + " accounts · quota used";
    const note = snapshot.unmanaged.length
      ? "An unmanaged login is active. Save it with ccsw add on the host before switching that provider."
      : state.session.readOnly ? "Read-only console. Account changes are disabled by the host."
      : "Select an account to review the switch and its effect on this host.";
    replaceRegion("#account-notice", '<div class="recommendation healthy">' + icon("lock", true) +
      '<p class="recommendation-copy">' + escape(note) + "</p></div>");
    $("#updated").textContent = "Host snapshot · " + new Date(snapshot.takenAt * 1000).toLocaleTimeString();
    $("#snapshot-time").textContent = "Snapshot " + dateText(snapshot.takenAt);
  }

  function renderActivity() {
    const activity = state.payload?.activity || [];
    replaceRegion("#activity-list", activity.map((event) =>
      '<li><span class="event-icon">' + icon("activity", true) + '</span><div class="event-copy"><p>' +
      escape(event.title) + "</p><small>" + escape(event.detail) + '</small></div><time class="event-time">' +
      escape(dateText(event.time)) + "</time></li>").join("") ||
      '<li class="empty">No events observed in this server session.</li>');
    const latest = activity[0];
    replaceRegion("#latest-activity", latest ? '<span class="event-icon">' + icon("activity", true) +
      '</span><div class="event-copy"><p>' + escape(latest.title) + "</p><small>" +
      escape(latest.detail) + "</small></div>" : '<p class="subtext">New activity will appear here.</p>');
  }

  function render() {
    renderAccounts();
    renderActivity();
    renderConnection();
    if (state.details && $("#details-dialog").open) renderDetails(state.details);
    Bitmap.mount(document);
  }

  function route() {
    const page = ["accounts", "activity", "connection"].includes(location.hash.slice(1)) ? location.hash.slice(1) : "accounts";
    for (const name of ["accounts", "activity", "connection"]) $("#page-" + name).hidden = name !== page;
    all("[data-page]").forEach((link) => {
      if (link.dataset.page === page) link.setAttribute("aria-current", "page");
      else link.removeAttribute("aria-current");
    });
    $("#breadcrumb").textContent = page[0].toUpperCase() + page.slice(1);
  }

  function openSwitch(slot) {
    const account = state.payload?.snapshot.accounts.find((row) => row.slot === slot);
    if (!writable() || !account?.switchable || account.active) return;
    state.selected = { ...account, revision: state.payload.snapshot.revision };
    const outgoing = state.payload.snapshot.accounts.find((row) => row.provider === account.provider && row.active);
    const codex = account.provider === "codex";
    $("#switch-content").innerHTML = '<div class="dialog-top"><span class="provider-mark">' + icon(account.provider) +
      '</span><span class="eyebrow">' + escape(providerName(account.provider)) + '</span></div><h2 id="switch-title">Switch account?</h2>' +
      '<p class="dialog-subtitle" id="switch-description">Change the default ' + providerName(account.provider) +
      " login on " + escape(state.session.host) + '.</p><div class="switch-route"><div><small>From</small><strong>' +
      escape(outgoing ? nameOf(outgoing) : "No login") + '</strong></div>' + icon("arrow", true) +
      '<div><small>To · #' + slot + "</small><strong>" + escape(nameOf(account)) + "</strong></div></div>" +
      '<p class="switch-impact' + (codex ? " warning" : "") + '">' +
      (codex ? "This can restart the Codex daemon and interrupt an active turn. Restart existing codex exec and --no-daemon sessions yourself."
        : "File-backed Claude sessions pick up this account on the next message. Keychain sessions can take about 30 seconds. Isolated session profiles keep their own account.") +
      "</p>" + (codex ? '<label class="impact-check"><input type="checkbox" id="acknowledge"><span>I understand that an active Codex turn can be interrupted.</span></label>' : "") +
      '<p id="switch-error" class="dialog-error" role="status" hidden></p>';
    $("#switch-dialog").showModal();
    validateDialog();
  }

  function validateDialog() {
    if (!state.selected || !$("#switch-dialog").open) return;
    const current = state.payload?.snapshot.accounts.find((row) => row.slot === state.selected.slot);
    const changed = state.selected.revision !== state.payload?.snapshot.revision;
    const message = changed ? "The accounts changed on the host. Close this dialog and review the latest state."
      : !state.connected ? "Waiting for the host connection. Switching is disabled."
      : !current?.switchable ? "This account is no longer available. Refresh usage or sign in on the host." : "";
    $("#switch-error").textContent = message;
    $("#switch-error").hidden = !message;
    $("#confirm-switch").disabled = !writable() || !!message || current?.active ||
      (state.selected.provider === "codex" && !$("#acknowledge")?.checked);
  }

  function operationNotice(title, detail, terminal = false) {
    $("#operation-panel").hidden = false;
    $("#operation-title").textContent = title;
    $("#operation-detail").textContent = detail;
    $("#dismiss-operation").hidden = !terminal;
  }

  function acceptOperation(operation) {
    if (operation.id !== state.pending) return;
    const subject = providerName(operation.provider) + " · Account #" + operation.slot;
    if (operation.state === "pending") {
      operationNotice("Switch pending", subject + " · Waiting for the host to finish.");
      scheduleResolution();
      return;
    }
    setPending(null);
    clearTimeout(operationTimer);
    const title = operation.state === "succeeded" ? "Account switch complete"
      : operation.state === "partial" ? "Account changed · follow-up required" : "Account switch failed";
    operationNotice(title, subject + " · " + (operation.error?.message || operation.effects?.followup || "Review the current account state."), true);
    renderConnection();
  }

  function scheduleResolution() {
    clearTimeout(operationTimer);
    if (state.pending && state.session) operationTimer = setTimeout(() => { void resolveOperation(); }, 2500);
  }

  async function resolveOperation() {
    if (!state.pending || !state.session || state.resolving || state.submitting) return;
    state.resolving = true;
    const id = state.pending;
    try {
      const operation = await request("operations/" + encodeURIComponent(id));
      if (state.pending === id) acceptOperation(operation);
    } catch (error) {
      if (error.status === 404 && state.connected && state.payload && !state.payload.snapshot.stale) {
        // A fresh snapshot is required before another user-initiated attempt.
        const payload = await request("snapshot").catch(() => null);
        if (payload && !payload.snapshot.stale && state.pending === id) {
          state.payload = payload;
          setPending(null);
          operationNotice("Switch result unavailable", "The operation is no longer tracked. The latest account state is shown below. Review it before starting another switch.", true);
          render();
        }
      } else if (state.pending === id) {
        operationNotice("Waiting for switch result", "The request will not be repeated. Reconnecting to read the original operation.");
      }
    } finally {
      state.resolving = false;
      scheduleResolution();
    }
  }

  async function confirmSwitch() {
    if ($("#confirm-switch").disabled || !state.selected) return;
    const selected = state.selected;
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    const id = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
    state.submitting = true;
    setPending(id);
    state.selected = null;
    $("#switch-dialog").close();
    operationNotice("Sending switch request", providerName(selected.provider) + " · Account #" + selected.slot);
    renderConnection();
    try {
      const operation = await request("switches", "POST", {
        slot: selected.slot, provider: selected.provider, expectedRevision: selected.revision,
        acknowledgeInterruption: selected.provider === "codex", requestId: id
      });
      acceptOperation(operation);
    } catch (error) {
      if (state.pending !== id) return;
      if (error.status && error.status < 500) {
        setPending(null);
        operationNotice("Switch not accepted", error.message, true);
      } else {
        operationNotice("Waiting for switch result", "The response was interrupted. The request will not be repeated; this browser will query its result.");
        scheduleResolution();
      }
      renderConnection();
    } finally {
      state.submitting = false;
      scheduleResolution();
    }
  }

  function renderDetails(slot) {
    const account = state.payload?.snapshot.accounts.find((row) => row.slot === slot);
    if (!account) {
      replaceRegion("#details-content", '<h2 id="details-title">Account removed</h2><p class="details-summary">This account is no longer in the host roster.</p>');
      return;
    }
    const usage = account.usage || account.lastGoodUsage;
    const stale = !account.usage && !!account.lastGoodUsage;
    const windows = [
      ["5-hour usage", usage?.fiveHour], ["7-day usage", usage?.sevenDay],
      ...(usage?.scoped || []).map((pool) => [pool.name, pool])
    ];
    const windowHtml = windows.map(([name, window]) => '<div class="detail-window"><h3>' + escape(name) +
      '</h3><strong class="' + usageClass(window?.pct) + '">' + (Number.isFinite(window?.pct) ? escape(window.pct) + "%" : "—") +
      '</strong><p>used · ' + escape(window?.resetsAt ? "Resets " + dateText(window.resetsAt) : "Reset unavailable") +
      "</p>" + (window?.aheadOfPace === undefined ? "" : "<p>" +
        (window.aheadOfPace ? "Above" : "Within") + " expected pace" +
        (window.willLastToReset === false ? " · May run out before reset" : "") + "</p>") + "</div>").join("");
    let extra = "";
    if (usage?.credits) extra += '<li><span>Credits</span><span>' + escape(usage.credits.unlimited ? "Unlimited" : usage.credits.balance ?? "Unknown") + "</span></li>";
    if (usage?.spend) extra += '<li><span>Extra usage</span><span>' + escape(usage.spend.used) + " / " +
      escape(usage.spend.limit ?? "—") + " " + escape(usage.spend.currency || "") + "</span></li>";
    if (account.resetCredits !== null) extra += '<li><span>Reset credits</span><span>' + escape(account.resetCredits) +
      (account.resetCreditsEndAt ? " · Ends " + escape(dateText(account.resetCreditsEndAt)) : "") + "</span></li>";
    replaceRegion("#details-content", '<div class="dialog-top"><span class="provider-mark">' + icon(account.provider) +
      '</span><span class="eyebrow">' + providerName(account.provider) + " · #" + slot +
      '</span></div><h2 id="details-title">' + escape(nameOf(account)) + '</h2><p class="dialog-subtitle">' +
      escape(account.email) + '</p><ul class="connection-details details-meta"><li><span>Plan</span><span>' +
      escape(account.plan) + "</span></li>" + (account.organization ? '<li><span>Organization</span><span>' +
      escape(account.organization) + "</span></li>" : "") + '<li><span>Status</span><span>' +
      escape(account.disabled ? "Disabled" : statusNames[account.usageStatus] || "Usage unavailable") +
      '</span></li><li><span>Last reading</span><span>' + escape(account.fetchedAt ? dateText(account.fetchedAt) : "None") +
      "</span></li>" + extra + "</ul>" + (stale ? '<p class="details-summary">Last good reading only. Current usage is unavailable.</p>' : "") +
      '<div class="detail-windows">' + windowHtml + "</div>");
  }

  $("#pair-address").textContent = location.origin;
  $("#pair-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    if (state.checking) return;
    const code = $("#pair-code").value.trim();
    await pairBrowser(code);
  });
  $("#refresh").addEventListener("click", async () => {
    if (!writable()) return;
    $("#refresh").disabled = true;
    try {
      await request("refresh", "POST");
      toast("Usage refresh queued.");
    } catch (error) { toast(error.message); }
    finally { renderConnection(); }
  });
  $("#search").addEventListener("input", (event) => { state.query = event.target.value; renderAccounts(); renderConnection(); });
  $("#sort").addEventListener("change", (event) => { state.sort = event.target.value; renderAccounts(); renderConnection(); });
  all("[data-filter]").forEach((button) => button.addEventListener("click", () => {
    state.filter = button.dataset.filter;
    all("[data-filter]").forEach((item) => item.setAttribute("aria-pressed", String(item === button)));
    renderAccounts();
    renderConnection();
  }));
  document.addEventListener("click", (event) => {
    const target = event.target.closest("[data-switch], [data-details]");
    if (target?.dataset.switch) openSwitch(Number(target.dataset.switch));
    if (target?.dataset.details) {
      state.details = Number(target.dataset.details);
      renderDetails(state.details);
      $("#details-dialog").showModal();
    }
  });
  $("#switch-content").addEventListener("change", validateDialog);
  $("#confirm-switch").addEventListener("click", () => { void confirmSwitch(); });
  $("#cancel-switch").addEventListener("click", () => $("#switch-dialog").close());
  $("#switch-dialog").addEventListener("close", () => { state.selected = null; });
  $("#close-details").addEventListener("click", () => $("#details-dialog").close());
  $("#details-dialog").addEventListener("close", () => { state.details = null; });
  $("#dismiss-operation").addEventListener("click", () => { $("#operation-panel").hidden = true; });
  $("#logout").addEventListener("click", async () => {
    $("#logout").disabled = true;
    try { await request("session", "DELETE"); setPending(null); showPair("This browser has been disconnected."); }
    catch (error) { toast(error.message); }
    finally { $("#logout").disabled = false; }
  });
  $("#copy-address").addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(state.session.origin + "/");
      toast("Console address copied.");
    } catch {
      const range = document.createRange();
      range.selectNodeContents($("#console-address"));
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      toast("Address selected. Copy it with your browser.");
    }
  });
  window.addEventListener("hashchange", () => {
    const code = takePairingLink();
    if (code !== null) { pairingLink = code; void checkSession(); }
    else route();
  });
  window.addEventListener("offline", () => { state.connected = false; renderConnection(); });
  window.addEventListener("online", () => { if (state.session) openStream(); else void checkSession(); });
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden && state.session) {
      state.connected = false;
      renderConnection();
      openStream();
      void checkSession();
    }
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "/" && state.session && !event.target.closest("input, select, textarea, dialog")) {
      event.preventDefault();
      location.hash = "accounts";
      $("#search").focus();
    }
  });
  setInterval(() => {
    if (!state.session) return;
    if (Date.now() >= state.session.expiresAt * 1000) {
      showPair("Your session expired. Pair this browser again.");
    } else if (state.connected && Date.now() - state.lastEvent > 12000) {
      state.connected = false;
      renderConnection();
    }
  }, 1000);
  void checkSession();
})();
