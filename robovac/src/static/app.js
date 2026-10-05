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

    const ov = $('#zone-overlay');
    frame.replaceChildren(document.importNode(parsed, true));
    if (ov) frame.append(ov);
    wireMap(frame);
    renderZones();
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

// --- zone drawing -----------------------------------------------------------
//
// Rectangles are kept in *map pixel* coordinates, the same space the SVG's
// viewBox uses. That means a drawn rectangle needs no scaling on the way out
// and the server can convert to map units with the pixel size it already knows.

const MAX_ZONES = 4;
const zones = [];
let drawMode = false;
let drawing = null;

function overlay() {
  return $('#zone-overlay');
}

function setDrawMode(on) {
  drawMode = on;
  const ov = overlay();
  const btn = $('#zone-draw');
  if (!ov || !btn) return;
  ov.toggleAttribute('data-active', on);
  btn.setAttribute('aria-pressed', String(on));
  btn.textContent = on ? 'Drawing… click to stop' : 'Draw a zone';
  const help = $('[data-role="zone-help"]');
  if (help) {
    help.textContent = on
      ? 'Drag on the map to draw. Drag again for another zone.'
      : `Up to ${MAX_ZONES} zones per run, which is this robot's limit.`;
  }
  // The overlay swallows pointer events, so stop the map from also toggling
  // rooms while a zone is being drawn.
  ov.style.pointerEvents = on ? 'auto' : 'none';
}

function renderZones() {
  const ov = overlay();
  if (!ov) return;
  ov.replaceChildren(
    ...zones.map((z, i) => {
      const el = document.createElement('div');
      el.className = 'zone-shape';
      el.style.left = `${Math.min(z.x0, z.x1)}px`;
      el.style.top = `${Math.min(z.y0, z.y1)}px`;
      el.style.width = `${Math.abs(z.x1 - z.x0)}px`;
      el.style.height = `${Math.abs(z.y1 - z.y0)}px`;

      const tag = document.createElement('span');
      tag.className = 'zone-index';
      tag.textContent = String(i + 1);
      el.append(tag);
      return el;
    }),
  );

  const count = $('[data-role="zone-count"]');
  if (count) count.textContent = String(zones.length);
  const clean = $('#zone-clean');
  if (clean) clean.disabled = zones.length === 0;
}

/** Map a pointer event to the SVG's own coordinate system. */
function toSvgPoint(evt) {
  const svg = $('#map-frame svg');
  const frame = $('#map-frame');
  if (!svg || !frame) return null;

  const box = svg.getBoundingClientRect();
  const px = Number(svg.dataset.pixelSize) || 1;
  const vx = Number(svg.dataset.viewX) || 0;
  const vy = Number(svg.dataset.viewY) || 0;
  // preserveAspectRatio="xMidYMid meet" letterboxes, so account for the offset
  // and the scale actually applied to the drawn content.
  const vb = svg.viewBox.baseVal;
  if (!vb || vb.width === 0) return null;

  const scale = Math.min(box.width / vb.width, box.height / vb.height);
  const offsetX = (box.width - vb.width * scale) / 2;
  const offsetY = (box.height - vb.height * scale) / 2;

  return {
    x: (evt.clientX - box.left - offsetX) / scale + vx,
    y: (evt.clientY - box.top - offsetY) / scale + vy,
  };
}

function wireZoneDrawing() {
  const ov = overlay();
  if (!ov) return;

  ov.addEventListener('pointerdown', (evt) => {
    if (!drawMode || zones.length >= MAX_ZONES) return;
    const p = toSvgPoint(evt);
    if (!p) return;
    ov.setPointerCapture(evt.pointerId);
    drawing = p;
    evt.preventDefault();
  });

  ov.addEventListener('pointermove', (evt) => {
    if (!drawing) return;
    const p = toSvgPoint(evt);
    if (!p) return;
    // Preview by mutating the in-progress rectangle.
    zones.push({ x0: drawing.x, y0: drawing.y, x1: p.x, y1: p.y });
    renderZones();
  });

  const finish = (evt) => {
    if (!drawing) return;
    const p = toSvgPoint(evt) ?? drawing;
    const last = zones.pop();
    drawing = null;
    if (last) {
      const w = Math.abs(p.x - last.x0);
      const h = Math.abs(p.y - last.y0);
      // Ignore accidental clicks; the server rejects sub-pixel zones anyway.
      if (w > 1 && h > 1) {
        zones.push({ x0: last.x0, y0: last.y0, x1: p.x, y1: p.y });
      }
      renderZones();
    }
  };
  ov.addEventListener('pointerup', finish);
  ov.addEventListener('pointercancel', finish);

  $('#zone-draw')?.addEventListener('click', () => setDrawMode(!drawMode));
  $('#zone-clear')?.addEventListener('click', () => {
    zones.length = 0;
    renderZones();
  });
  $('#zone-clean')?.addEventListener('click', async () => {
    if (zones.length === 0) return;
    await withButton($('#zone-clean'), () =>
      post('/api/clean-zones', {
        zones: zones.map((z) => [z.x0, z.y0, z.x1, z.y1]),
      }),
    );
    setDrawMode(false);
  });
}

// --- obstacles ---------------------------------------------------------------
//
// Valetudo rate-limits obstacle photos hard, so the list shows markers
// immediately and photos load strictly on demand, one at a time.

async function loadObstacles() {
  const section = $('[data-role="obstacles-section"]');
  if (!section) return;

  try {
    const res = await fetch('/api/obstacles');
    if (res.status === 404) return; // robot has no camera
    const data = await res.json();
    renderObstacles(data.obstacles || []);
  } catch {
    section.hidden = true;
  }
}

function renderObstacles(obstacles) {
  const section = $('[data-role="obstacles-section"]');
  const list = $('[data-role="obstacle-list"]');
  const count = $('[data-role="obstacle-count"]');
  if (!section || !list) return;

  section.hidden = false;
  if (count) count.textContent = `${obstacles.length} reported`;

  if (obstacles.length === 0) {
    list.replaceChildren();
    const empty = document.createElement('p');
    empty.className = 'obstacle-empty';
    empty.textContent = 'Nothing reported yet.';
    list.append(empty);
    return;
  }

  list.replaceChildren(
    ...obstacles.map((o) => {
      const item = document.createElement('li');
      item.className = 'obstacle-item';

      const img = document.createElement('img');
      img.alt = `Obstacle ${o.id}`;
      img.loading = 'lazy';
      img.addEventListener('click', () => {
        img.src = `/api/obstacles/image?id=${encodeURIComponent(o.id)}`;
        img.style.cursor = 'default';
      });

      const meta = document.createElement('span');
      meta.className = 'obstacle-meta';
      meta.textContent = `${Math.round(o.x)}, ${Math.round(o.y)}`;

      item.append(img, meta);
      return item;
    }),
  );
}

const obstacleToggle = $('#obstacle-capture');
if (obstacleToggle) {
  obstacleToggle.addEventListener('change', async () => {
    obstacleToggle.disabled = true;
    try {
      await post('/api/obstacles/enabled', { enabled: obstacleToggle.checked });
    } catch (err) {
      alert(`Failed: ${err.message}`);
      obstacleToggle.checked = !obstacleToggle.checked;
    } finally {
      obstacleToggle.disabled = false;
    }
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
wireZoneDrawing();
renderZones();
loadObstacles();
loadMap();
connect();