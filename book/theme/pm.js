// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
// Pulse Mesh book chrome. The sidebar's head and the menu bar together are the
// website's header: its mark and name, its pages, its theme switch and its call
// to action. Under the brand the sidebar names the three books and, when the
// menu bar has no room, carries the website's pages. Previous and next are
// labelled links, a skip link leads past the table of contents, the search box
// offers the other two books, and when the book is served from inside the
// website (under /docs/) the website's live visualizations replace the PNG
// figures. Everything else the theme does is CSS (theme/custom.css).
(function () {
  "use strict";

  function meta(name) {
    var m = document.querySelector('meta[name="' + name + '"]');
    return m ? m.content : "";
  }
  var root = meta("pm-root");
  var version = meta("pm-version");
  // The book's title names it: "Pulse Mesh", "Pulse Mesh Daemon" or "Pulse Mesh Profiles".
  var bookTitle = meta("pm-book");
  var book = /daemon/i.test(bookTitle) ? "daemon" : /profiles/i.test(bookTitle) ? "profiles" : "standard";
  var BOOKS = [["standard", "Standard", "docs/"], ["daemon", "Daemon", "docs/daemon/"], ["profiles", "Profiles", "docs/profiles/"]];
  // Served from inside the website: the site root is everything before /docs/,
  // however deep the book sits (/docs/, /docs/daemon/, /docs/profiles/).
  var at = location.pathname.indexOf("/docs/");
  var site = at >= 0 ? location.pathname.slice(0, at + 1) : null;
  // A host site describes itself in <site>/pm-site.json:
  //   {"home": "/", "pages": [[label, href], ...], "cta": [label, short label, href], "figures": false}
  // An href that starts with "/" is a path on the host; any other is relative to
  // the site root. "figures": false keeps the exported PNGs, for a host that does
  // not carry the website's visualizations. Without the file the menu bar names
  // only the book, and the mark leads to the site root.
  var SITE_PAGES = [];
  var CTA = null;
  var siteHome = site;
  var siteFigures = true;
  // Which website page holds each visualization, and the two ids that do not follow the pattern.
  var VIZ_PAGE = {
    A01: "index", A02: "index",
    A03: "primitives", A04: "primitives", A05: "primitives", A06: "primitives", A07: "primitives", A08: "primitives", A09: "primitives",
    A10: "ring", A11: "ring", A12: "ring", A13: "ring",
    A14: "transport", A15: "transport", A16: "transport", A17: "transport", A18: "transport",
    A19: "conformance", A20: "conformance", A21: "conformance"
  };
  var VIZ_ID = { A01: "viz-A01-hero", A02: "viz-A02-matrix" };
  function siteHref(href) { return /^\//.test(href) ? href : site + href; }
  function homeHref() { return site ? siteHome : root + "index.html"; }
  function siteManifest() {
    if (!site || !window.fetch) { return Promise.resolve(null); }
    return fetch(site + "pm-site.json")
      .then(function (r) { return r.ok ? r.json() : null; })
      .catch(function () { return null; });
  }
  function el(tag, cls, text) {
    var e = document.createElement(tag);
    if (cls) { e.className = cls; }
    if (text) { e.textContent = text; }
    return e;
  }

  // ---- The website's mark (components/logo/Logo.tsx on the site) and its name.
  function brandMarkup(sub) {
    return '<svg class="pm-mark" viewBox="0 0 30 30" fill="none" stroke="currentColor" stroke-width="1" aria-hidden="true">' +
      '<line class="pm-mark-beam" x1="8" y1="8" x2="22" y2="22" stroke-width="0.8"/>' +
      '<g class="pm-mark-node-sm"><circle cx="8" cy="8" r="5"/><circle cx="8" cy="8" r="2.2" fill="currentColor"/></g>' +
      '<g class="pm-mark-node-lg"><circle cx="22" cy="22" r="6.5"/><circle cx="22" cy="22" r="3" fill="currentColor"/></g>' +
      '</svg><span class="pm-brand-text"><span class="pm-wordmark">Pulse Mesh</span>' +
      (sub ? '<span class="pm-brand-sub">' + sub + "</span>" : "") + "</span>";
  }
  function brandLink(cls, sub) {
    var a = el("a", cls);
    a.href = homeHref();
    a.setAttribute("aria-label", "Pulse Mesh home");
    a.innerHTML = brandMarkup(sub);
    return a;
  }

  // ---- Theme: mdBook's "light" is Paper and "navy" is Dark. The choice is the
  // website's, kept as "pm-theme" (theme/head.hbs applies it before the first paint).
  function isDark() { return document.documentElement.classList.contains("navy"); }
  function mirrorTheme() {
    // The website's scripts read data-theme; keep it in step with mdBook's class.
    if (isDark()) { document.documentElement.setAttribute("data-theme", "dark"); }
    else { document.documentElement.removeAttribute("data-theme"); }
  }
  function setTheme(dark) {
    // mdBook's own theme buttons apply the class and its highlight sheets; they stay in the DOM, hidden.
    var b = document.getElementById("mdbook-theme-" + (dark ? "navy" : "light"));
    if (b) { b.click(); }
    try { localStorage.setItem("pm-theme", dark ? "dark" : "light"); } catch (e) { /* storage can be off; the switch still applies */ }
    mirrorTheme();
    window.dispatchEvent(new Event("resize")); // live canvases re-read their colours
  }
  function themeToggle() {
    var t = el("button", "pm-theme-toggle");
    t.type = "button";
    t.setAttribute("aria-label", "Switch between dark and light");
    t.innerHTML = '<svg class="pm-icon-sun" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true">' +
      '<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" stroke-linecap="round"/></svg>' +
      '<svg class="pm-icon-moon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true">' +
      '<path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5Z" stroke-linejoin="round"/></svg>';
    t.addEventListener("click", function () { setTheme(!isDark()); });
    return t;
  }

  // ---- The website's pages, as a list: in the menu bar, and in the sidebar when the menu bar has no room.
  function siteList(cls) {
    var ul = el("ul", cls);
    var docs = site + "docs/";
    var pages = SITE_PAGES.slice();
    if (!pages.some(function (p) { return siteHref(p[1]) === docs; })) { pages.push(["Docs", docs]); }
    pages.forEach(function (p) {
      var li = el("li"), a = el("a", "", p[0]);
      a.href = siteHref(p[1]);
      if (siteHref(p[1]) === docs) { a.setAttribute("aria-current", "true"); }
      li.appendChild(a);
      ul.appendChild(li);
    });
    return ul;
  }
  function ctaLink() {
    var a = el("a", "pm-nav-cta");
    a.href = siteHref(CTA[2]);
    a.appendChild(el("span", "pm-cta-full", CTA[0]));
    a.appendChild(el("span", "pm-cta-short", CTA[1]));
    a.appendChild(document.createTextNode(" →"));
    return a;
  }

  // ---- Menu bar: the mark (while the sidebar is closed), then the pages, the theme switch and the call to action.
  function menuBar() {
    var bar = document.getElementById("mdbook-menu-bar");
    if (!bar || bar.querySelector(".pm-nav-brand")) { return; }
    var title = bar.querySelector(".menu-title");
    if (title) {
      // mdBook's title is an h1 on every page; the brand is not the page's heading.
      var div = el("div", title.className);
      div.appendChild(brandLink("pm-nav-brand", ""));
      title.parentNode.replaceChild(div, title);
    }
    var right = bar.querySelector(".right-buttons");
    if (right) { right.appendChild(themeToggle()); }
  }
  function menuBarSite() {
    var right = document.querySelector("#mdbook-menu-bar .right-buttons");
    if (!right || !site) { return; }
    right.insertBefore(siteList("pm-nav-links"), right.querySelector(".pm-theme-toggle"));
    if (CTA) { right.appendChild(ctaLink()); }
  }

  // ---- Sidebar: the brand and the three books above the table of contents.
  var head = null;
  function sidebarHead() {
    var nav = document.getElementById("mdbook-sidebar");
    if (!nav || nav.querySelector(".pm-sidebar-brand")) { return; }
    head = el("div", "pm-sidebar-head");
    head.appendChild(brandLink("pm-sidebar-brand", "The " + book + (version ? " · v" + version : "")));
    if (site) {
      var books = el("ul", "pm-books");
      books.setAttribute("aria-label", "The three books");
      BOOKS.forEach(function (b) {
        var li = el("li"), a = el("a", "", b[1]);
        a.href = site + b[2];
        if (b[0] === book) { a.setAttribute("aria-current", "true"); }
        li.appendChild(a);
        books.appendChild(li);
      });
      head.appendChild(books);
    }
    nav.insertBefore(head, nav.firstChild);
    syncTabIndex();
    if (window.MutationObserver) {
      new MutationObserver(syncTabIndex).observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });
    }
    // The table of contents is absolutely positioned inside the sidebar;
    // push it below the head, and re-measure once the web fonts load.
    var box = nav.querySelector(".sidebar-scrollbox");
    if (box) {
      var place = function () { box.style.top = head.offsetHeight + "px"; };
      place();
      if (document.fonts && document.fonts.ready) { document.fonts.ready.then(place); }
      window.addEventListener("resize", place);
      if (window.ResizeObserver) { new ResizeObserver(place).observe(head); }
    }
  }
  function sidebarSite() {
    if (!head || !site) { return; }
    head.appendChild(siteList("pm-site-links"));
    Array.prototype.forEach.call(document.querySelectorAll(".pm-sidebar-brand, .pm-nav-brand"), function (a) { a.href = homeHref(); });
    syncTabIndex();
  }
  // mdBook takes the sidebar's links out of the tab order while it is closed, but
  // only the links it found at load; the ones added here follow the sidebar's state.
  function syncTabIndex() {
    var open = document.documentElement.classList.contains("sidebar-visible");
    Array.prototype.forEach.call(document.querySelectorAll(".pm-sidebar-head a"), function (a) { a.tabIndex = open ? 0 : -1; });
  }

  // ---- A skip link, first in the tab order, past the table of contents to the text.
  function skipLink() {
    if (document.querySelector(".pm-skip")) { return; }
    var a = el("a", "pm-skip", "Skip to content");
    a.href = "#mdbook-content";
    a.addEventListener("click", function (e) {
      var main = document.querySelector("#mdbook-content main");
      if (!main) { return; }
      e.preventDefault();
      main.setAttribute("tabindex", "-1");
      main.focus();
    });
    document.body.insertBefore(a, document.body.firstChild);
  }

  // ---- Search: say which book the box searches, and offer the other two with the same words.
  function searchElsewhere() {
    var input = document.getElementById("mdbook-searchbar");
    if (!input) { return; }
    input.placeholder = "Search the " + book + " book";
    var form = document.getElementById("mdbook-searchbar-outer");
    if (!site || !form || form.querySelector(".pm-search-elsewhere")) { return; }
    var p = el("p", "pm-search-elsewhere", "Also search ");
    var others = BOOKS.filter(function (b) { return b[0] !== book; });
    var links = others.map(function (b, i) {
      var a = el("a", "", "the " + b[0] + " book");
      a.setAttribute("data-base", site + b[2]);
      p.appendChild(a);
      if (i < others.length - 1) { p.appendChild(document.createTextNode(" · ")); }
      return a;
    });
    function update() {
      var q = input.value.trim();
      links.forEach(function (a) { a.href = a.getAttribute("data-base") + (q ? "?search=" + encodeURIComponent(q) : ""); });
    }
    input.addEventListener("input", update);
    update();
    form.appendChild(p);
  }

  // ---- Previous and next: labelled links that name the chapter, at the foot of the page.
  function chapterNav() {
    var toc = document.getElementById("mdbook-sidebar");
    var titleFor = function (href) {
      if (!toc) { return ""; }
      var links = toc.querySelectorAll("a[href]");
      for (var i = 0; i < links.length; i++) {
        if (links[i].href === href) { return links[i].textContent.trim(); }
      }
      return "";
    };
    var links = document.querySelectorAll(".nav-wrapper a.mobile-nav-chapters");
    Array.prototype.forEach.call(links, function (a) {
      var next = a.classList.contains("next");
      var kicker = el("span", "pm-nav-kicker", next ? "Next" : "Previous");
      var target = el("span", "pm-nav-target", titleFor(a.href) || (next ? "Next chapter" : "Previous chapter"));
      a.textContent = "";
      a.appendChild(kicker);
      a.appendChild(target);
    });
  }

  // ---- Figures: the website's visualizations, live, in place of the exported PNGs.
  function liveFigures() {
    if (!site || !siteFigures || !window.fetch || !window.DOMParser) { return; }
    var figures = Array.prototype.filter.call(document.querySelectorAll("figure.pm-figure"), function (f) {
      var img = f.querySelector("img.pm-fig-light");
      return img && /\/viz\/A\d+\.png$/.test(img.getAttribute("src") || "");
    });
    if (!figures.length) { return; }
    var pages = {};
    function pageDoc(name) {
      if (!pages[name]) {
        pages[name] = fetch(site + name + ".html").then(function (r) {
          if (!r.ok) { throw new Error("site page " + name + ": " + r.status); }
          return r.text();
        }).then(function (text) { return new DOMParser().parseFromString(text, "text/html"); });
      }
      return pages[name];
    }
    function script(src) {
      return new Promise(function (resolve, reject) {
        var s = document.createElement("script");
        s.src = src;
        s.onload = resolve;
        s.onerror = function () { reject(new Error("script " + src)); };
        document.body.appendChild(s);
      });
    }
    var core = null;
    function coreLoaded() {
      // common.js carries the canvas set-up and the colour helpers; viz-core.js the drawing primitives.
      core = core || script(site + "assets/js/common.js").then(function () { return script(site + "assets/js/viz-core.js"); }).then(mirrorTheme);
      return core;
    }
    function mount(fig) {
      var id = fig.querySelector("img.pm-fig-light").getAttribute("src").match(/\/viz\/(A\d+)\.png$/)[1];
      var page = VIZ_PAGE[id];
      if (!page) { return Promise.resolve(); }
      return pageDoc(page).then(function (doc) {
        var block = doc.getElementById(VIZ_ID[id] || "viz-" + id);
        var scripts = doc.querySelectorAll("script[src]");
        var src = null;
        for (var i = 0; i < scripts.length; i++) {
          if (scripts[i].getAttribute("src").indexOf("js/viz/" + id + "-") >= 0) { src = scripts[i].getAttribute("src"); }
        }
        if (!block || !src) { return; }
        var live = document.importNode(block, true);
        var caption = live.querySelector(".pm-viz-caption");
        if (caption) { caption.parentNode.removeChild(caption); }
        Array.prototype.forEach.call(fig.querySelectorAll("img"), function (img) { img.parentNode.removeChild(img); });
        fig.insertBefore(live, fig.firstChild);
        fig.classList.add("is-live");
        return coreLoaded().then(function () { return script(site + src); });
      });
    }
    mirrorTheme();
    // One at a time: each script looks its block up by id as soon as it runs.
    figures.reduce(function (chain, fig) {
      return chain.then(function () { return mount(fig); }).catch(function () { /* the PNG stays */ });
    }, Promise.resolve()).then(function () { window.dispatchEvent(new Event("resize")); });
  }


  // ---- The website's document header: the page's kind as the kicker, its heading, the audience
  // line as the lede, and the book, the part and the reading time as the meta row. The introduction
  // and the part openers carry heroes of their own and are left alone.
  function docHeader() {
    var main = document.querySelector("#mdbook-content main");
    if (!main || main.querySelector(".pm-hero, .pm-part, .pm-doc-header")) { return; }
    var h1 = main.querySelector("h1");
    var kind = main.querySelector("p.pm-kind");
    if (!h1 || !kind) { return; }
    var header = el("header", "pm-doc-header");
    var b = kind.querySelector("b");
    var kicker = el("p", "pm-doc-kicker");
    if (b) { var k = el("b", "", b.textContent.trim()); kicker.appendChild(k); }
    var rest = kind.textContent.replace(b ? b.textContent : "", "").replace(/^\s*·\s*/, "").trim();
    var title = el("h1"); title.innerHTML = h1.innerHTML;
    var lede = el("p", "pm-doc-lede", rest);
    var meta = el("p", "pm-doc-meta");
    var parts = ["The " + book + " book"];
    var active = document.querySelector("#mdbook-sidebar .chapter li.chapter-item.expanded > .chapter-link-wrapper > a.active, #mdbook-sidebar .chapter a.active");
    if (active) {
      var li = active.closest("li.chapter-item"), part = null;
      while (li && !part) { li = li.previousElementSibling; if (li && li.classList.contains("part-title")) { part = li.textContent.trim(); } }
      if (part) { parts.push(part); }
    }
    var words = main.textContent.split(/\s+/).length;
    parts.push(Math.max(1, Math.round(words / 220)) + " min read");
    parts.forEach(function (t, i) { if (i) { meta.appendChild(el("span", "pm-sep", "·")); } meta.appendChild(el("span", "", t)); });
    header.appendChild(kicker); header.appendChild(title); header.appendChild(lede); header.appendChild(meta);
    h1.classList.add("pm-doc-raw");
    kind.parentNode.removeChild(kind);
    h1.parentNode.insertBefore(header, h1);
    // "You will have" and "You need" open an example or a workflow: set them as its lede block.
    var p = header.nextElementSibling; var n = 0;
    while (p && p.tagName === "P" && n < 2 && /^(You will have|You need)/.test(p.textContent)) { p.classList.add("pm-lede-block"); p = p.nextElementSibling; n++; }
  }

  function init() {
    mirrorTheme();
    docHeader();
    chapterNav();
    skipLink();
    sidebarHead();
    menuBar();
    searchElsewhere();
    siteManifest().then(function (m) {
      if (m && m.pages && m.pages.length) {
        // The older form names the home page first and has no "home".
        if (m.home) { SITE_PAGES = m.pages; siteHome = siteHref(m.home); }
        else { SITE_PAGES = m.pages.slice(1); siteHome = siteHref(m.pages[0][1]); }
      }
      if (m && m.cta && m.cta.length === 3) { CTA = m.cta; }
      if (m && m.figures === false) { siteFigures = false; }
      menuBarSite();
      sidebarSite();
      liveFigures();
    });
  }
  if (document.readyState === "loading") { document.addEventListener("DOMContentLoaded", init); }
  else { init(); }
})();
