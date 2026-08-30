/* eebus site — theme toggle, documentation search, table-of-contents highlighting. */
(function () {
  "use strict";

  var root = document.documentElement;

  /* ── Theme ──────────────────────────────────────────────────────────── */

  function resolved() {
    var t = root.getAttribute("data-theme");
    if (t === "light" || t === "dark") return t;
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }

  function applyTheme(mode) {
    root.setAttribute("data-theme", mode);
    var l = document.getElementById("syn-light"), d = document.getElementById("syn-dark");
    if (!l || !d) return;
    if (mode === "auto") {
      l.media = "(prefers-color-scheme: light)";
      d.media = "(prefers-color-scheme: dark)";
    } else {
      l.media = mode === "light" ? "all" : "not all";
      d.media = mode === "dark" ? "all" : "not all";
    }
  }

  var toggle = document.querySelector(".theme-toggle");
  if (toggle) {
    toggle.addEventListener("click", function () {
      var next = resolved() === "dark" ? "light" : "dark";
      applyTheme(next);
      try { localStorage.setItem("theme", next); } catch (e) {}
    });
  }

  /* ── Copy buttons on code blocks ────────────────────────────────────── */

  if (navigator.clipboard) {
    document.querySelectorAll(".prose pre, .prose-wide pre").forEach(function (pre) {
      var btn = document.createElement("button");
      btn.className = "copy";
      btn.type = "button";
      btn.textContent = "Copy";
      btn.setAttribute("aria-label", "Copy code to clipboard");
      btn.addEventListener("click", function () {
        navigator.clipboard.writeText(pre.innerText).then(function () {
          btn.textContent = "Copied";
          setTimeout(function () { btn.textContent = "Copy"; }, 1600);
        });
      });
      var wrap = document.createElement("div");
      wrap.className = "code-wrap";
      pre.parentNode.insertBefore(wrap, pre);
      wrap.appendChild(pre);
      wrap.appendChild(btn);
    });
  }

  /* ── Table of contents highlighting ─────────────────────────────────── */

  var tocLinks = Array.prototype.slice.call(document.querySelectorAll(".toc a"));
  if (tocLinks.length && "IntersectionObserver" in window) {
    var byId = {};
    tocLinks.forEach(function (a) {
      var id = decodeURIComponent(a.hash || "").slice(1);
      if (id) byId[id] = a;
    });
    var seen = new Set();
    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (e) {
        if (e.isIntersecting) seen.add(e.target.id); else seen.delete(e.target.id);
      });
      var first = Object.keys(byId).find(function (id) { return seen.has(id); });
      tocLinks.forEach(function (a) { a.classList.remove("active"); });
      if (first) byId[first].classList.add("active");
    }, { rootMargin: "-72px 0px -70% 0px" });
    Object.keys(byId).forEach(function (id) {
      var el = document.getElementById(id);
      if (el) io.observe(el);
    });
  }

  /* ── Search ─────────────────────────────────────────────────────────── */

  var input = document.getElementById("q");
  var panel = document.getElementById("results");
  if (!input || !panel || !window.SEARCH_INDEX_URL) return;

  var docs = null, loading = false, selected = -1;

  // The sidebar scrolls, so an absolutely positioned panel would be clipped by it.
  function place() {
    var r = input.getBoundingClientRect();
    var w = Math.min(380, document.documentElement.clientWidth - 24);
    panel.style.width = w + "px";
    panel.style.top = (r.bottom + 6) + "px";
    panel.style.left = Math.min(r.left, document.documentElement.clientWidth - w - 12) + "px";
  }
  window.addEventListener("resize", function () { if (!panel.hidden) place(); });

  function load() {
    if (docs || loading) return Promise.resolve();
    loading = true;
    return fetch(window.SEARCH_INDEX_URL)
      .then(function (r) { return r.json(); })
      .then(function (json) {
        var store = (json.documentStore && json.documentStore.docs) || {};
        docs = Object.keys(store).map(function (k) {
          var d = store[k];
          return {
            url: d.id,
            title: d.title || "",
            description: d.description || "",
            body: d.body || "",
            hay: ((d.title || "") + " " + (d.description || "") + " " + (d.body || "")).toLowerCase()
          };
        });
      })
      .catch(function () { docs = []; })
      .then(function () { loading = false; });
  }

  function terms(q) {
    return q.toLowerCase().split(/\s+/).filter(function (t) { return t.length > 1; });
  }

  function score(doc, ts) {
    var total = 0;
    for (var i = 0; i < ts.length; i++) {
      var t = ts[i];
      if (doc.hay.indexOf(t) === -1) return 0;
      if (doc.title.toLowerCase().indexOf(t) !== -1) total += 12;
      if (doc.description.toLowerCase().indexOf(t) !== -1) total += 5;
      var n = doc.body.toLowerCase().split(t).length - 1;
      total += Math.min(n, 6);
    }
    return total;
  }

  function excerpt(doc, ts) {
    var body = doc.body, at = -1;
    for (var i = 0; i < ts.length && at === -1; i++) at = body.toLowerCase().indexOf(ts[i]);
    if (at === -1) return doc.description || body.slice(0, 110);
    var from = Math.max(0, at - 45);
    return (from > 0 ? "…" : "") + body.slice(from, from + 130).replace(/\s+/g, " ") + "…";
  }

  function render(list, ts) {
    if (!list.length) {
      panel.innerHTML = '<p class="empty">Nothing found.</p>';
    } else {
      panel.innerHTML = list.map(function (d) {
        return '<a href="' + d.url + '"><b></b><small></small></a>';
      }).join("");
      panel.querySelectorAll("a").forEach(function (a, i) {
        a.querySelector("b").textContent = list[i].title;
        a.querySelector("small").textContent = excerpt(list[i], ts);
      });
    }
    place();
    panel.hidden = false;
    selected = -1;
  }

  function run() {
    var q = input.value.trim();
    if (q.length < 2) { panel.hidden = true; return; }
    load().then(function () {
      var ts = terms(q);
      if (!ts.length) { panel.hidden = true; return; }
      var hits = docs
        .map(function (d) { return { d: d, s: score(d, ts) }; })
        .filter(function (x) { return x.s > 0; })
        .sort(function (a, b) { return b.s - a.s; })
        .slice(0, 8)
        .map(function (x) { return x.d; });
      render(hits, ts);
    });
  }

  var timer;
  input.addEventListener("input", function () {
    clearTimeout(timer);
    timer = setTimeout(run, 90);
  });
  input.addEventListener("focus", load);

  input.addEventListener("keydown", function (e) {
    var items = panel.hidden ? [] : Array.prototype.slice.call(panel.querySelectorAll("a"));
    if (e.key === "Escape") { panel.hidden = true; input.blur(); return; }
    if (!items.length) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      selected = (selected + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length;
      items.forEach(function (a, i) { a.classList.toggle("sel", i === selected); });
      items[selected].scrollIntoView({ block: "nearest" });
    } else if (e.key === "Enter" && selected >= 0) {
      e.preventDefault();
      window.location.href = items[selected].getAttribute("href");
    }
  });

  document.addEventListener("click", function (e) {
    if (!panel.contains(e.target) && e.target !== input) panel.hidden = true;
  });

  document.addEventListener("keydown", function (e) {
    if (e.key === "/" && document.activeElement !== input && !/^(INPUT|TEXTAREA)$/.test(document.activeElement.tagName)) {
      e.preventDefault();
      input.focus();
    }
  });
})();
