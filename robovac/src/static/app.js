// Progressive enhancement. Every control on the page works as a plain form or
// link without JavaScript; this only removes round trips and adds the map
// interactions HomeKit-grade widgets cannot express.
//
// Two things need JS and cannot work without it:
//   1. Clicking a room on the map. The SVG is *inlined* rather than referenced,
//      because an <img>-loaded SVG is a separate document: page CSS does not
//      reach into it and click handlers cannot attach.
//   2. Live updates over SSE.

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

// --- selection state ---------------------------------------------------------
// Held in the URL fragment so a selection survives a reload and can be
// bookmarked or shared, and so the server can render the selected state on the
// very first paint, before this script runs.
const selected = new Set(readFragment());

function readFragment() {
  return new URLSearchParams(location.hash.slice(1))
    .get('segments')
    ?.split(',')
    .map((s) => s.trim())
    .filter(Boolean) ?? [];
}

function writeFragment() {
  const params = new URLSearchParams();
  if (selected.size) params.set('segments', [...selected].join(','));
  const next = params.toString();
  // replaceState rather than assignment: selecting rooms should not fill the
  // back button with dozens of entries.
  history.replaceState(null, '', next ? `#${params}` : location.pathname + location.search);
}

const selectedQuery = () => [...selected].join(',');

// --- map ---------------------------------------------------------------------

async function loadMap() {
  const frame = $('#map-frame');
  if (!frame) return;

  const query = selectedQuery();
  try {
    const res = await fetch(`/map.svg${query ? `?segments=${encodeURIComponent(query)}` : ''}`, {
      cache: 'no-store',
    });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const svg = await res.text();

    const doc = new DOMParser().parseFromString(svg, 'image/svg+xml');
    const parsed = doc.documentElement;
    if (parsed.nodeName === 'parsererror' || !parsed.querySelector('.vacuum-map')) {
      throw new Error('unparsable SVG');
    }

    frame.replaceChildren(document.importNode(parsed, true));
    wireMap(frame);
  } catch (err) {
    // Leave the <noscript> image in place rather than showing an empty box.
    frame.dataset.failed = '1';
    const note = $('[data-role="map-note"]');
    if (note) note.textContent = `Map unavailable (${err.message}).`;
  }
}

function wireMap(frame) {
  for (const group of $$('.segment', frame)) {
    group.addEventListener('click', (evt) => {
      // The label is a child of the group and carries its own action.
      if (evt.target.closest('[data-clean]')) return;
      const id = group.dataset.segmentId;
      if (id) toggle(id);
    });
  }

  // Clicking a room name cleans just that room.
  for (const label of $$('[data-clean]', frame)) {
    label.addEventListener('click', (evt) => {
      evt.stopPropagation();
      cleanSegments([label.dataset.clean]);
    });
  }
}

function toggle(id) {
  if (!id) return;
  if (selected.has(id)) selected.delete(id);
  else selected.add(id);
  syncSelection();
  writeFragment();
  loadMap();
}

/** Reflect the selection in the checkbox list and in the map's own attributes. */
function syncSelection() {
  for (const box of $$('input[name="segment"]')) {
    box.checked = selected.has(box.value);
  }

  for (const chip of $$('.chip')) {
    chip.classList.toggle('on', selected.has($('input', chip)?.value));
  }

  const frame = $('#map-frame');
  for (const group of $$('.segment', frame)) {
    group.dataset.selected = String(selected.has(group.dataset.segmentId));
  }

  const count = $('[data-role="selected-count"]');
  if (count) count.textContent = String(selected.size);

  const clean = $('#clean-selected');
  if (clean) clean.disabled = selected.size === 0;
}

// --- commands ----------------------------------------------------------------

async function post(url, body) {
  const res = await fetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body ?? {}),
  });
  if (!res.ok) {
    const detail = await res.json().catch(() => ({ error: res.statusText }));
    throw new Error(detail.error || res.statusText);
  }
  return res.json();
}

async function withButton(btn, fn) {
  if (!btn) return;
  btn.disabled = true;
  try {
    await fn();
  } catch (err) {
    alert(`Failed: ${err.message}`);
  } finally {
    btn.disabled = false;
    syncSelection();
  }
}

const cleanSegments = (ids) =>
  withButton($('#clean-selected'), () =>
    post('/api/clean-segments', { segment_ids: ids, iterations: 1 }),
  );

for (const btn of $$('button[data-action]')) {
  btn.addEventListener('click', () =>
    withButton(btn, () => post(`/api/control/${btn.dataset.action}`)),
  );
}

const fan = $('#fan-speed');
if (fan) {
  fan.addEventListener('change', () => post('/api/fan-speed', { name: fan.value }));
}

const clear = $('#clear-selection');
if (clear) {
  clear.addEventListener('click', () => {
    selected.clear();
    syncSelection();
    writeFragment();
    loadMap();
  });
}

// --- live updates ------------------------------------------------------------

function patch(field, value) {
  const el = document.querySelector(`[data-field="${field}"]`);
  if (el && value != null && el.textContent.trim() !== String(value)) {
    el.textContent = value;
  }
}

let lastMapVersion = null;

function connect() {
  const source = new EventSource('/events');

  source.addEventListener('state', (evt) => {
    let payload;
    try {
      payload = JSON.parse(evt.data);
    } catch {
      return;
    }

    const s = payload.summary;
    if (s) {
      patch('status', s.status);
      patch('battery', s.battery != null ? `${Math.round(s.battery)}%` : '—');
      patch('dock', s.dock_status);
    }

    // Only re-fetch the map when the server says the geometry actually moved.
    // The map document is large and unchanged most ticks.
    if (typeof payload.map_version === 'number' && payload.map_version !== lastMapVersion) {
      lastMapVersion = payload.map_version;
      loadMap();
    }
  });

  // EventSource reconnects on its own, but surfacing the state stops the UI
  // looking live when it is not.
  source.addEventListener('error', () => {
    const link = $('.status');
    if (link && link.classList.contains('ok')) link.classList.add('stale');
  });
}

// --- camera ------------------------------------------------------------------

const cameraBtn = $('#camera-toggle');
if (cameraBtn) {
  cameraBtn.addEventListener('click', async () => {
    const video = $('#camera');
    if (video.dataset.playing === '1') {
      video.srcObject = null;
      delete video.dataset.playing;
      cameraBtn.textContent = 'Start camera';
      return;
    }
    cameraBtn.disabled = true;
    try {
      const { default: JSMpeg } = await import('https://cdn.jsdelivr.net/npm/jsmpeg@1.0.2/+esm');
      new JSMpeg.Player(video, { url: '/api/camera/stream', type: 'mpegts', live: true, autoplay: true });
      video.dataset.playing = '1';
      cameraBtn.textContent = 'Stop camera';
    } catch (err) {
      alert(`Camera failed: ${err.message}`);
    } finally {
      cameraBtn.disabled = false;
    }
  });
}

// --- boot --------------------------------------------------------------------

// Server-rendered state is already in the DOM, so the first paint needs no JS.
syncSelection();
loadMap();
connect();