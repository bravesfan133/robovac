// Progressive enhancement only. Every control on the page works as a plain
// form/link without JavaScript; this just removes the round trips.

const selectedSegments = () =>
  [...document.querySelectorAll('input[name="segment"]:checked')].map((el) => el.value);

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

document.querySelectorAll('button[data-action]').forEach((btn) => {
  btn.addEventListener('click', async () => {
    btn.disabled = true;
    try {
      await post(`/api/control/${btn.dataset.action}`);
    } catch (err) {
      alert(`Failed: ${err.message}`);
    } finally {
      btn.disabled = false;
    }
  });
});

const fan = document.getElementById('fan-speed');
if (fan) {
  fan.addEventListener('change', async () => {
    try {
      await post('/api/fan-speed', { name: fan.value });
    } catch (err) {
      alert(`Failed: ${err.message}`);
    }
  });
}

const clean = document.getElementById('clean-selected');
if (clean) {
  clean.addEventListener('click', async (evt) => {
    const ids = selectedSegments();
    if (ids.length === 0) {
      evt.preventDefault();
      alert('Select at least one room.');
    }
  });
}

// Live status via SSE. The server pushes a summary; we only patch the text
// nodes that changed so the map image is not re-fetched.
function patch(selector, value) {
  const el = document.querySelector(selector);
  if (el && value != null && el.textContent.trim() !== String(value)) {
    el.textContent = value;
  }
}

const source = new EventSource('/events');
source.addEventListener('state', (evt) => {
  let payload;
  try {
    payload = JSON.parse(evt.data);
  } catch {
    return;
  }
  const s = payload.summary;
  if (!s) return;
  patch('.controls .stat:nth-child(1) .value', s.status);
  patch('.controls .stat:nth-child(2) .value', s.battery != null ? `${s.battery}%` : '—');
  patch('.controls .stat:nth-child(3) .value', s.dock_status);
});

// Camera: jsmpeg is loaded lazily so the page stays cheap when unused.
const cameraBtn = document.getElementById('camera-toggle');
if (cameraBtn) {
  cameraBtn.addEventListener('click', async () => {
    const video = document.getElementById('camera');
    if (video.dataset.playing === '1') {
      video.srcObject = null;
      video.dataset.playing = '0';
      cameraBtn.textContent = 'Start camera';
      return;
    }
    cameraBtn.disabled = true;
    try {
      const { default: JSMpeg } = await import(
        'https://cdn.jsdelivr.net/npm/jsmpeg@1.0.2/+esm'
      );
      video.dataset.player = '1';
      new JSMpeg.Player(video, {
        url: '/api/camera/stream',
        type: 'mpegts',
        live: true,
        autoplay: true,
      });
      video.dataset.playing = '1';
      cameraBtn.textContent = 'Stop camera';
    } catch (err) {
      alert(`Camera failed: ${err.message}`);
    } finally {
      cameraBtn.disabled = false;
    }
  });
}