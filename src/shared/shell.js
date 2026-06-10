/* Shared shell-page helpers (vanilla, no build step). */
/* global window, document */
(function () {
  const tauri = window.__TAURI__ || {};
  window.hermes = {
    invoke: (cmd, args) => tauri.core.invoke(cmd, args),
    listen: (ev, cb) => tauri.event.listen(ev, cb),
  };

  // Apply boot theme: pre-paint background + light/dark class.
  window.hermes.applyBootTheme = function (info) {
    if (info && info.bgHex) {
      document.documentElement.style.setProperty("--hermes-bg", info.bgHex);
    }
    if (info && info.isDark === false) {
      document.documentElement.classList.add("light");
    }
  };

  // Stay in sync if the page theme changes while a shell window is open.
  if (tauri.event) {
    tauri.event.listen("theme-changed", (e) => {
      const p = e.payload || {};
      if (p.hex) document.documentElement.style.setProperty("--hermes-bg", p.hex);
      document.documentElement.classList.toggle("light", p.isDark === false);
    });
  }
})();
