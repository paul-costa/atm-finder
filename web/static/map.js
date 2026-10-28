// The things the Rust/WebAssembly side cannot do on its own: the Leaflet map, the file download
// and the clipboard. All are plain functions on `window`, called through wasm-bindgen (see ui.rs).
(function () {
  "use strict";

  // Escape text for insertion into popup HTML.
  function esc(value) {
    const map = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
    return String(value == null ? "" : value).replace(/[&<>"']/g, (c) => map[c]);
  }

  // Popup for one ATM from its GeoJSON properties; `labels` holds the translated words.
  function popupHtml(properties, labels) {
    const title = properties.operator || properties.brand || properties.name || "ATM";
    const town = [properties.postcode, properties.city].filter(Boolean).join(" ");
    const address = properties.street ? [properties.street, town].filter(Boolean).join(", ") : labels.no_address;
    let html = `<strong>${esc(title)}</strong><br>${esc(address)}`;
    if (properties.opening_hours) {
      html += `<br><small>${esc(labels.hours)}: ${esc(properties.opening_hours)}</small>`;
    }
    // A Google Maps search for the position; "Save" there adds the place to a list.
    const gmaps = `https://www.google.com/maps/search/?api=1&query=${properties.lat.toFixed(6)}%2C${properties.lon.toFixed(6)}`;
    html += `<br><a href="${esc(properties.url)}" target="_blank" rel="noopener">${esc(labels.osm)}</a>`;
    html += ` · <a href="${gmaps}" target="_blank" rel="noopener">${esc(labels.gmaps)}</a>`;
    return html;
  }

  window.atmMap = {
    map: null,
    atmLayer: null,
    boundaryLayer: null,

    // Create the map once, centred on Austria, with the standard OSM tiles.
    init(elementId) {
      if (this.map || typeof L === "undefined") return;
      this.map = L.map(elementId, { scrollWheelZoom: false }).setView([47.6, 14.1], 7);
      L.tileLayer("https://tile.openstreetmap.org/{z}/{x}/{y}.png", {
        maxZoom: 19,
        attribution: '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a>',
      }).addTo(this.map);
    },

    clear() {
      if (!this.map) return;
      if (this.atmLayer) this.map.removeLayer(this.atmLayer);
      if (this.boundaryLayer) this.map.removeLayer(this.boundaryLayer);
      this.atmLayer = this.boundaryLayer = null;
    },

    // Draw the ATMs (a GeoJSON FeatureCollection) and the boundary (a GeoJSON Feature), then
    // zoom to them. All three arguments arrive as JSON text from Rust.
    show(atmsText, boundaryText, labelsText) {
      if (!this.map) return;
      this.clear();
      const labels = JSON.parse(labelsText || "{}");
      const boundary = JSON.parse(boundaryText || "null");
      if (boundary && boundary.geometry) {
        this.boundaryLayer = L.geoJSON(boundary, { style: { color: "#4ade80", weight: 2, fillOpacity: 0.05 } }).addTo(this.map);
      }
      const atms = JSON.parse(atmsText || '{"type":"FeatureCollection","features":[]}');
      this.atmLayer = L.geoJSON(atms, {
        pointToLayer: (feature, latlng) =>
          L.circleMarker(latlng, { radius: 6, color: "#0b0b0c", weight: 1.5, fillColor: "#4ade80", fillOpacity: 0.9 }),
        onEachFeature: (feature, layer) => layer.bindPopup(popupHtml(feature.properties, labels)),
      }).addTo(this.map);
      // The map container was hidden when the map was created, so Leaflet measured it as
      // zero pixels; refresh the size before fitting, otherwise the zoom is wrong.
      this.map.invalidateSize();
      const bounds = (this.boundaryLayer || this.atmLayer).getBounds();
      if (bounds.isValid()) this.map.fitBounds(bounds, { padding: [16, 16] });
    },
  };

  // Offer `text` as a file download by clicking an invisible link to a blob URL.
  window.atmDownload = function (fileName, mimeType, text) {
    const blob = new Blob([text], { type: mimeType });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = fileName;
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };

  // Put `text` on the clipboard. Without the Clipboard API (a page served over plain http) show
  // it in a prompt to copy by hand. Returns a promise either way.
  window.atmCopy = function (text) {
    if (navigator.clipboard && window.isSecureContext) return navigator.clipboard.writeText(text);
    window.prompt("", text);
    return Promise.resolve();
  };
})();
