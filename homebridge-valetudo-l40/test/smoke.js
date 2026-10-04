// Drives the plugin against scripts/fake-valetudo.mjs using a minimal HAP stub.
// Run:  node scripts/fake-valetudo.mjs 8081 &
//       cd homebridge-valetudo-l40 && node test/smoke.js
'use strict';

const { EventEmitter } = require('events');

const makeCharacteristic = (name) => {
  const handlers = { get: null, set: null };
  return {
    name,
    value: null,
    onGet(fn) { handlers.get = fn; return this; },
    addLinkedCharacteristic() { return this; },
    onSet(fn) { handlers.set = fn; return this; },
    setValue(v) { this.value = v; return this; },
    updateValue(v) { this.value = v; return this; },
    updateCharacteristic(_c, v) { this.value = v; return this; },
    get handlers() { return handlers; },
  };
};

const makeService = (displayName) => ({
  displayName,
  characteristics: [],
  // In HAP-NodeJS these return the Characteristic, not the service.
  addCharacteristic(c) {
    const ch = makeCharacteristic(c);
    this.characteristics.push(ch);
    return ch;
  },
  addOptionalCharacteristic(c) { return this.addCharacteristic(c); },
  addLinkedCharacteristic(c) { return this.addCharacteristic(c); },
  setCharacteristic(c, v) {
    const existing = this.characteristics.find((x) => x.name === c);
    if (existing) existing.value = v;
    return this;
  },
  getCharacteristic(c) {
    let ch = this.characteristics.find((x) => x.name === c);
    if (!ch) ch = this.addCharacteristic(c);
    return ch;
  },
  updateCharacteristic(c, v) {
    const ch = this.getCharacteristic(c);
    ch.value = v;
  },
});

// `new Service.Vacuum()` must yield the stub object, so the proxy returns a
// constructor that returns it.
const classProxy = new Proxy({}, {
  get: (_t, name) =>
    function StubService(displayName) {
      // HAP derives displayName from the label when one is passed.
      return makeService(displayName || String(name));
    },
});

const hap = {
  Accessory: class {
    constructor(name) {
      this.name = name;
      this.services = [];
      this.emitter = new EventEmitter();
    }
    // HAP services are sometimes passed as a class (`Service.Vacuum`) and
    // sometimes as an instance. Normalise both to an object.
    addService(input) {
      const s = typeof input === 'function' ? new input() : input;
      this.services.push(s);
      return {
        setCharacteristic: (c, v) => s.setCharacteristic(c, v),
        addLinkedCharacteristic: (c) => s.addLinkedCharacteristic(c),
      };
    }
    getServiceByName(n) { return this.services.find((s) => s.displayName === n); }
    publish() {}
    on(ev, fn) { this.emitter.on(ev, fn); }
  },
  Service: classProxy,
  Characteristic: new Proxy({}, { get: (_t, k) => String(k) }),
};

const hbPath = require.resolve('homebridge');
require.cache[hbPath] = { id: hbPath, filename: hbPath, loaded: true, exports: { util: hap } };

const plugin = require('../index.js');
plugin({ util: hap, registerAccessory: () => {} });
const AccessoryClass = plugin.ValetudoVacuum;

const messages = [];
const log = {
  info: (m) => messages.push(`INFO ${m}`),
  warn: (m) => messages.push(`WARN ${m}`),
  debug: () => {},
  error: (m) => messages.push(`ERROR ${m}`),
};

async function main() {
  const acc = new AccessoryClass(log, {
    name: 'Test Vacuum',
    host: process.env.FAKE_VALETUDO || 'http://127.0.0.1:8081',
  });

  await new Promise((resolve) => acc.start(resolve));

  // Give the first poll and the deferred consumable fetch time to land.
  await new Promise((r) => setTimeout(r, 1500));

  const names = acc.accessory.services.map((s) => s.displayName).sort();
  console.log('services :', names.join(', '));
  console.log('state    :', JSON.stringify(acc.state));

  const failures = [];
  const expect = (cond, msg) => { if (!cond) failures.push(msg); };

  expect(names.includes('Vacuum'), 'Vacuum service missing');
  expect(names.includes('Battery'), 'Battery service missing');
  expect(
    acc.accessory.services.some((s) => s.characteristics.some((c) => c.name === 'RunState')),
    'Run State characteristic missing',
  );
  expect(acc.state.status !== 'unknown', 'status never populated');
  expect(acc.state.battery !== null, 'battery never populated');
  expect(acc.state.consumables.length > 0, 'consumables never fetched');
  expect(names.some((n) => /Filter/i.test(n)), 'percentage consumable service missing');
  expect(names.includes('Auto empty'), 'dock trigger switch missing');

  // The On characteristic should read true once cleaning.
  await acc.control('start');
  await new Promise((r) => setTimeout(r, 1800));
  const on = acc.vacuumService.getCharacteristic('On');
  const active = await on.handlers.get();
  console.log('after start, status =', acc.state.status, '| On reads', active);
  expect(ACTIVE(acc.state.status) === active, 'On does not reflect status');

  await new Promise((resolve) => acc.stop(resolve));

  for (const m of messages) console.log(m);

  if (failures.length) {
    console.error('\nFAILURES:');
    for (const f of failures) console.error(' -', f);
    process.exit(1);
  }
  console.log('\nall smoke assertions passed');
}

function ACTIVE(status) {
  return ['cleaning', 'returning', 'moving', 'manual_control'].includes(status);
}

main().catch((err) => {
  console.error('smoke test crashed:', err);
  process.exit(1);
});