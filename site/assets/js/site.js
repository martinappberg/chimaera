/* chimaera site: theme toggle, mobile nav, and live download links.
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
  // Asset-name suffix match — NOT substring: updater artifacts share the
  // installer's name plus a suffix (chimaera_x.AppImage.sig), and the API's
  // asset order is not guaranteed, so a substring match can hand the hero
  // button a signature file.
  function endsWith(name, suffix) {
    return name.length >= suffix.length && name.indexOf(suffix, name.length - suffix.length) !== -1;
  }
  // Match a release asset by name suffix; returns its browser_download_url.
  function find(assets, needle) {
    for (var i = 0; i < assets.length; i++) {
      if (endsWith(assets[i].name, needle))
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
      var appimage = find(assets, ".AppImage");
      var linuxX64 = find(assets, "x86_64-unknown-linux-musl");
      var linuxArm = find(assets, "aarch64-unknown-linux-musl");
      var macDaemon = find(assets, "aarch64-apple-darwin");

      setHref("dl-app", dmg);
      setHref("dl-linux-app", appimage);
      setHref("dl-linux-x64", linuxX64);
      setHref("dl-linux-arm", linuxArm);
      setHref("dl-macos", macDaemon);

      // Human size for an asset matched by name suffix, e.g. "12.3 MB".
      function sizeOf(needle) {
        for (var i = 0; i < assets.length; i++) {
          if (endsWith(assets[i].name, needle) && assets[i].size)
            return (assets[i].size / 1048576).toFixed(1) + " MB";
        }
        return null;
      }

      // The primary button follows the visitor's OS; macOS is the shipped
      // default so an unknown/blocked UA still gets a working button.
      var ua = navigator.userAgent || "";
      var isLinux = ua.indexOf("Linux") !== -1 && ua.indexOf("Android") === -1;
      var label = document.getElementById("dl-app-label");
      var note = document.getElementById("dl-app-note");
      if (isLinux && appimage) {
        setHref("dl-app", appimage);
        if (label) label.textContent = "Download for Linux";
        // The shipped default icon is the Apple mark — swap it, or Linux
        // visitors get "Download for Linux" under an Apple logo.
        var icon = document.getElementById("dl-app-icon");
        if (icon) {
          icon.setAttribute("fill", "none");
          icon.setAttribute("stroke", "currentColor");
          icon.setAttribute("stroke-width", "1.8");
          icon.setAttribute("stroke-linecap", "round");
          icon.setAttribute("stroke-linejoin", "round");
          icon.innerHTML = '<path d="M5 16l6-6-6-6M12 18h7"/>';
        }
        if (note) {
          var lsz = sizeOf(".AppImage");
          note.textContent =
            "x86_64 · AppImage" + (lsz ? " · " + lsz : "") +
            " · auto-updates (.deb/.rpm on GitHub)";
        }
      } else if (dmg && note) {
        var msz = sizeOf(".dmg");
        note.textContent =
          "Apple Silicon · .dmg" + (msz ? " · " + msz : "") + " · auto-updates";
      }

      if (tag) {
        var relEl = document.getElementById("release-tag");
        if (relEl) relEl.textContent = tag + " · native app";
      }
    })
    .catch(function () {
      /* Keep the shipped fallbacks (→ /releases/latest). Nothing to do. */
    });
})();
