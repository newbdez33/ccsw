"use strict";

(() => {
  const root = document.documentElement;
  const system = matchMedia("(prefers-color-scheme: dark)");
  const settings = {
    theme: { key: "ccsw-theme", values: ["bitmap", "paper"], fallback: "bitmap" },
    colorMode: { key: "ccsw-color-mode", values: ["system", "light", "dark"], fallback: "system" }
  };
  const preferences = {};

  function normalize(name, value) {
    const setting = settings[name];
    return setting.values.includes(value) ? value : setting.fallback;
  }

  function read(name) {
    try { return normalize(name, localStorage.getItem(settings[name].key)); }
    catch { return settings[name].fallback; }
  }

  function apply() {
    root.dataset.theme = preferences.theme;
    root.dataset.colorMode = preferences.colorMode;
    root.dataset.colorScheme = preferences.colorMode === "system"
      ? (system.matches ? "dark" : "light") : preferences.colorMode;
    document.querySelectorAll("[data-appearance]").forEach((input) => {
      input.checked = input.value === preferences[input.dataset.appearance];
    });
    const background = getComputedStyle(root).getPropertyValue("--bg").trim();
    if (background) document.querySelector('meta[name="theme-color"]').content = background;
  }

  for (const name of Object.keys(settings)) preferences[name] = read(name);
  // Run before stylesheets to apply the saved theme on the first paint.
  apply();

  system.addEventListener("change", () => {
    if (preferences.colorMode === "system") apply();
  });
  window.addEventListener("storage", (event) => {
    let changed = false;
    for (const [name, setting] of Object.entries(settings)) {
      if (event.key !== null && event.key !== setting.key) continue;
      preferences[name] = read(name);
      changed = true;
    }
    if (changed) apply();
  });

  document.addEventListener("DOMContentLoaded", () => {
    const dialog = document.querySelector("#appearance-dialog");
    document.querySelectorAll("[data-appearance-open]").forEach((button) => {
      button.addEventListener("click", () => dialog.showModal());
    });
    document.querySelectorAll("[data-appearance]").forEach((input) => {
      input.addEventListener("change", () => {
        if (!input.checked) return;
        const name = input.dataset.appearance;
        preferences[name] = normalize(name, input.value);
        try {
          if (preferences[name] === settings[name].fallback) localStorage.removeItem(settings[name].key);
          else localStorage.setItem(settings[name].key, preferences[name]);
        } catch {}
        apply();
      });
    });
    apply();
  });
})();
