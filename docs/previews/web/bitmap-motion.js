"use strict";

const Bitmap = (() => {
  const FPS = 15;
  const DIGIT_FRAMES = 3;
  const ROW_FRAMES = 2;
  const DITHER_FRAMES = 1;
  const reducedMotion = matchMedia("(prefers-reduced-motion: reduce)");
  const jobs = new Set();
  const numbers = new Map();
  const charts = new Map();
  const entrances = new WeakMap();
  const cards = new WeakSet();
  let ticking = false;
  let lastTick = 0;

  // One clock advances all effects in whole frames. It never interpolates.
  function tick(time) {
    if (time - lastTick >= 1000 / FPS) {
      lastTick = time - ((time - lastTick) % (1000 / FPS));
      for (const job of Array.from(jobs)) if (job() === false) jobs.delete(job);
    }
    ticking = jobs.size > 0;
    if (ticking) requestAnimationFrame(tick);
  }

  function schedule(job) {
    jobs.add(job);
    if (!ticking) {
      ticking = true;
      lastTick = performance.now();
      requestAnimationFrame(tick);
    }
    return () => jobs.delete(job);
  }

  const observer = new IntersectionObserver((entries) => {
    for (const entry of entries) {
      if (!entry.isIntersecting) continue;
      const start = entrances.get(entry.target);
      observer.unobserve(entry.target);
      entrances.delete(entry.target);
      if (start) start();
    }
  }, { threshold: 0.1 });

  function onEntrance(element, start) {
    if (reducedMotion.matches) { start(); return; }
    entrances.set(element, start);
    observer.observe(element);
  }

  reducedMotion.addEventListener("change", () => {
    if (!reducedMotion.matches) return;
    for (const record of [...numbers.values(), ...charts.values()]) {
      const start = entrances.get(record.element);
      if (!start) continue;
      observer.unobserve(record.element);
      entrances.delete(record.element);
      start();
    }
  });

  function createNumber(element) {
    const record = { element, target: null, digits: [], cancel: null, entered: false };
    numbers.set(element.dataset.numberKey, record);
    return record;
  }

  function animateNumber(record, value, replay = false) {
    record.cancel?.();
    const prior = record.target;
    const target = String(value).padStart(2, "0");
    const previous = replay ? "0".repeat(target.length) : record.digits.join("").padStart(target.length, "0").slice(-target.length);
    record.target = value;
    record.element.dataset.value = value;
    record.element.setAttribute("aria-label", `${value} percent`);
    record.element.innerHTML = Array.from(target, (_, index) => `<span class="digit" aria-hidden="true" data-place="${target.length - index - 1}"><span class="digit-strip"><span>${previous[index]}</span><span>0</span></span></span>`).join("");
    const cells = Array.from(record.element.children);
    const direction = !replay && prior !== null && value < prior ? -1 : 1;
    let lastStop = 0;
    const plans = Array(target.length);
    for (let index = target.length - 1; index >= 0; index--) {
      const from = Number(previous[index]);
      const to = Number(target[index]);
      const count = ((to - from) * direction + 10) % 10;
      const stop = count ? Math.max(count * DIGIT_FRAMES, lastStop + 4) : 0;
      if (count) lastStop = stop;
      plans[index] = { from, to, count, stop, start: stop - count * DIGIT_FRAMES };
    }
    record.digits = Array.from(previous, Number);
    const finish = () => {
      cells.forEach((cell, index) => {
        cell.firstElementChild.style.transform = "translateY(0px)";
        cell.firstElementChild.children[0].textContent = target[index];
        cell.classList.remove("digit-stop");
      });
      record.digits = Array.from(target, Number);
    };
    if (reducedMotion.matches || !lastStop) { finish(); return; }
    let frame = 0;
    record.cancel = schedule(() => {
      // A filtered record can be mounted again with the same target.
      if (!record.element.isConnected || reducedMotion.matches) { finish(); return false; }
      frame++;
      cells.forEach((cell, index) => {
        const plan = plans[index];
        const strip = cell.firstElementChild;
        cell.classList.toggle("digit-stop", !!plan.count && frame === plan.stop);
        if (frame >= plan.stop) {
          strip.style.transform = "translateY(0px)";
          strip.children[0].textContent = plan.to;
          record.digits[index] = plan.to;
          if (plan.count && frame === plan.stop) cell.dataset.stopFrame = frame;
          return;
        }
        if (frame < plan.start) return;
        const elapsed = frame - plan.start;
        const step = Math.floor(elapsed / DIGIT_FRAMES);
        const current = (plan.from + direction * step + 100) % 10;
        strip.children[0].textContent = current;
        strip.children[1].textContent = (current + direction + 10) % 10;
        record.digits[index] = current;
        const offset = Math.round(cell.clientHeight * (elapsed % DIGIT_FRAMES) / DIGIT_FRAMES);
        strip.style.transform = `translateY(-${offset}px)`;
      });
      return frame <= lastStop;
    });
  }

  function createChart(element) {
    const compact = element.classList.contains("compact");
    const rows = compact ? 3 : 5;
    const columns = compact ? 1 : 2;
    const groups = 8;
    const pitch = compact ? 4 : 6;
    const stride = compact ? 10 : 18;
    const radius = compact ? 1 : 1.5;
    const circles = [];
    for (let group = 0; group < groups; group++) {
      for (let row = 0; row < rows; row++) {
        for (let column = 0; column < columns; column++) {
          circles.push(`<circle class="chart-dot" data-row="${group * rows + row}" cx="${3 + group * stride + column * pitch}" cy="${3 + (rows - row - 1) * pitch}" r="${radius}"/>`);
        }
      }
    }
    element.innerHTML = `<svg viewBox="0 0 ${groups * stride - (compact ? 4 : 6)} ${rows * pitch}" aria-hidden="true">${circles.join("")}</svg><span class="quota-track" aria-hidden="true"><span class="quota-fill"></span></span>`;
    const record = { element, nodes: Array.from(element.querySelectorAll("circle")), rows: groups * rows, current: 0, target: null, cancel: null, entered: false };
    charts.set(element.dataset.chartKey, record);
    return record;
  }

  function animateChart(record, value, replay = false) {
    record.cancel?.();
    record.target = value;
    const targetRows = Math.round(value / 100 * record.rows);
    if (replay) record.current = 0;
    const birth = new Map();
    const draw = (frame) => {
      record.nodes.forEach((dot) => {
        const row = Number(dot.dataset.row);
        dot.classList.toggle("on", row < record.current);
        dot.classList.toggle("hot", row < record.current && birth.has(row) && frame - birth.get(row) < ROW_FRAMES * 2);
      });
      record.element.dataset.litRows = record.current;
    };
    if (reducedMotion.matches) { record.current = targetRows; draw(0); return; }
    draw(0);
    if (record.current === targetRows) return;
    let frame = 0;
    let lastChange = 0;
    record.cancel = schedule(() => {
      if (!record.element.isConnected || reducedMotion.matches) { record.current = targetRows; birth.clear(); draw(frame); return false; }
      frame++;
      if (frame % ROW_FRAMES === 0 && record.current !== targetRows) {
        if (record.current < targetRows) birth.set(record.current++, frame);
        else record.current--;
        lastChange = frame;
      }
      draw(frame);
      return record.current !== targetRows || frame < lastChange + ROW_FRAMES * 2;
    });
  }

  function mountNumbers(root) {
    root.querySelectorAll("[data-number-key]").forEach((placeholder) => {
      const value = Number(placeholder.dataset.value);
      const existing = numbers.get(placeholder.dataset.numberKey);
      const record = existing || createNumber(placeholder);
      if (existing && placeholder !== record.element) placeholder.replaceWith(record.element);
      if (record.target === value) return;
      if (record.entered) animateNumber(record, value);
      else {
        record.element.innerHTML = '<span class="number-placeholder" aria-hidden="true">00</span>';
        onEntrance(record.element, () => { record.entered = true; animateNumber(record, value); });
      }
    });
  }

  function mountCharts(root) {
    root.querySelectorAll("[data-chart-key]").forEach((placeholder) => {
      const value = Number(placeholder.dataset.value);
      const existing = charts.get(placeholder.dataset.chartKey);
      const record = existing || createChart(placeholder);
      if (existing && placeholder !== record.element) placeholder.replaceWith(record.element);
      record.element.dataset.value = value;
      record.element.setAttribute("aria-valuenow", value);
      record.element.style.setProperty("--quota-used", `${value}%`);
      if (record.target === value) return;
      if (record.entered) animateChart(record, value);
      else onEntrance(record.element, () => { record.entered = true; animateChart(record, value); });
    });
  }

  function mountDither(root) {
    root.querySelectorAll(".dither-card").forEach((element) => {
      if (cards.has(element)) return;
      cards.add(element);
      let level = 0;
      let pointerInside = false;
      let focused = false;
      let cancel;
      const update = () => {
        cancel?.();
        const target = pointerInside || focused ? 4 : 0;
        if (reducedMotion.matches) { level = target; element.dataset.density = level; return; }
        let frame = 0;
        cancel = schedule(() => {
          if (!element.isConnected) return false;
          if (reducedMotion.matches) { level = target; element.dataset.density = level; return false; }
          if (++frame % DITHER_FRAMES === 0) {
            level += Math.sign(target - level);
            element.dataset.density = level;
          }
          return level !== target;
        });
      };
      element.dataset.density = "0";
      element.addEventListener("pointerenter", (event) => { if (event.pointerType !== "touch") { pointerInside = true; update(); } });
      element.addEventListener("pointerleave", () => { pointerInside = false; update(); });
      element.addEventListener("focusin", (event) => { focused = event.target.matches(":focus-visible"); update(); });
      element.addEventListener("focusout", (event) => { if (!element.contains(event.relatedTarget)) { focused = false; update(); } });
    });
  }

  function mount(root = document) {
    mountNumbers(root);
    mountCharts(root);
    mountDither(root);
  }

  function replay() {
    for (const record of numbers.values()) {
      if (record.entered && record.element.isConnected) animateNumber(record, record.target, true);
    }
    for (const record of charts.values()) {
      if (record.entered && record.element.isConnected) animateChart(record, record.target, true);
    }
  }

  function number(value, key) {
    return `<span class="odometer" role="img" aria-label="${value} percent" data-number-key="${key}" data-value="${value}"><span class="number-placeholder" aria-hidden="true">00</span></span>`;
  }

  function chart(value, key, compact = false) {
    return `<span class="dot-chart${compact ? " compact" : ""}" role="meter" aria-label="Quota used" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${value}" data-chart-key="${key}" data-value="${value}"></span>`;
  }

  return Object.freeze({ mount, replay, number, chart, previous: (key) => numbers.get(key)?.target ?? null, config: Object.freeze({ fps: FPS, digitFrames: DIGIT_FRAMES, rowFrames: ROW_FRAMES, ditherFrames: DITHER_FRAMES }) });
})();
