// Loads the WebAssembly module. A file of its own rather than an inline script, so the server's
// Content-Security-Policy can allow scripts from this origin only (no 'unsafe-inline').
import init from "./pkg/atm_finder_web.js";
init().catch((error) => {
  const status = document.getElementById("status");
  status.hidden = false;
  status.textContent = "WebAssembly konnte nicht geladen werden / failed to load: " + error;
});
