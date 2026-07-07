/* chimaera site — theme toggle, mobile nav, and live download links.
   No dependencies; degrades gracefully if the GitHub API is unreachable
   (every button ships with a working /releases/latest fallback href). */
(function () {
  "use strict";

  var REPO = "martinappberg/chimaera";
  var doc = document.documentElement;

  /* ---- Theme -------------------------------------------------------------- */
  function setTheme(t) {
    doc.setAttribute("data-theme", t);
    try {
      localStorage.setItem("chimaera-theme", t);
    } catch (e) {}
  }
  var toggle = document.getElementById("theme-toggle");
  if (toggle) {
    toggle.addEventListener("click", function () {
      setTheme(doc.getAttribute("data-theme") === "dark" ? "light" : "dark");
    });
  }
  // Follow OS changes only while the visitor hasn't made an explicit choice.
  try {
    var mq = matchMedia("(prefers-color-scheme: dark)");
    mq.addEventListener("change", function (e) {
      if (!localStorage.getItem("chimaera-theme"))
        doc.setAttribute("data-theme", e.matches ? "dark" : "light");
    });
  } catch (e) {}

  /* ---- Sticky-nav border on scroll --------------------------------------- */
  var nav = document.getElementById("nav");
  if (nav) {
    var onScroll = function () {
      nav.classList.toggle("scrolled", window.scrollY > 8);
    };
    onScroll();
    window.addEventListener("scroll", onScroll, { passive: true });
  }

  /* ---- Mobile sheet ------------------------------------------------------- */
  var sheet = document.getElementById("sheet");
  var burger = document.getElementById("burger");
  function closeSheet() {
    if (sheet) sheet.classList.remove("open");
  }
  if (burger && sheet) {
    burger.addEventListener("click", function () {
      sheet.classList.add("open");
    });
    sheet.addEventListener("click", function (e) {
      if (e.target.closest("[data-close]")) closeSheet();
    });
    document.addEventListener("keydown", function (e) {
      if (e.key === "Escape") closeSheet();
    });
  }

  /* ---- Footer year -------------------------------------------------------- */
  var year = document.getElementById("year");
  if (year) year.textContent = String(new Date().getFullYear());

  /* ---- Live downloads from the latest GitHub release ---------------------- */
  // Match a release asset by substring; returns its browser_download_url.
  function find(assets, needle) {
    for (var i = 0; i < assets.length; i++) {
      if (assets[i].name.indexOf(needle) !== -1)
        return assets[i].browser_download_url;
    }
    return null;
  }
  function setHref(id, url) {
    var el = document.getElementById(id);
    if (el && url) el.setAttribute("href", url);
  }

  var appList = document.getElementById("asset-list");
  if (!appList) return; // not the landing page

  fetch("https://api.github.com/repos/" + REPO + "/releases/latest", {
    headers: { Accept: "application/vnd.github+json" },
  })
    .then(function (r) {
      if (!r.ok) throw new Error("no release");
      return r.json();
    })
    .then(function (rel) {
      var assets = rel.assets || [];
      var tag = rel.tag_name || "";

      var dmg = find(assets, ".dmg");
      var linuxX64 = find(assets, "x86_64-unknown-linux-musl");
      var linuxArm = find(assets, "aarch64-unknown-linux-musl");
      var macDaemon = find(assets, "aarch64-apple-darwin");

      setHref("dl-app", dmg);
      setHref("dl-linux-x64", linuxX64);
      setHref("dl-linux-arm", linuxArm);
      setHref("dl-macos", macDaemon);

      if (tag) {
        var relEl = document.getElementById("release-tag");
        if (relEl)
          relEl.textContent = tag + " · macOS (Apple Silicon)";
      }
      if (dmg) {
        var note = document.getElementById("dl-app-note");
        // Surface the human size next to the .dmg when we can.
        for (var i = 0; i < assets.length; i++) {
          if (assets[i].name.indexOf(".dmg") !== -1 && assets[i].size) {
            var mb = (assets[i].size / 1048576).toFixed(1);
            if (note) note.textContent = "Apple Silicon · .dmg · " + mb + " MB · auto-updates";
            break;
          }
        }
      }
    })
    .catch(function () {
      /* Keep the shipped fallbacks (→ /releases/latest). Nothing to do. */
    });
})();
