'use strict';

const axios = require('axios');

const PLUGIN_NAME = 'homebridge-valetudo-l40';
const BASE = '/api/v2/robot';

const ACTIVE_STATUSES = new Set(['cleaning', 'returning', 'moving', 'manual_control']);
const CHARGING_STATES = new Set(['cleaning', 'emptying', 'drying', 'cooling']);

// HomeKit's air-quality scale is coarser than the robot's fan presets. These two
// maps are the closest honest translation; the exact preset is what gets sent to
// the robot when the user picks a level.
const FAN_TO_AIR_QUALITY = {
  quiet: 1, min: 1, low: 1,
  medium: 2, balanced: 2, high: 2,
  turbo: 3,
  max: 4,
  custom: 2,
};
const AIR_QUALITY_TO_FAN = { 1: 'quiet', 2: 'medium', 3: 'turbo', 4: 'max' };

function humanise(value) {
  const cleaned = String(value || '').trim();
  if (!cleaned || cleaned === 'none') return cleaned;
  return cleaned.replace(/[_.]+/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase());
}

/** `('filter', 'none')` -> 'Filter'; `('brush', 'side_right')` -> 'Brush Side Right'. */
function consumableLabel(type, subType) {
  const base = humanise(type);
  if (!subType || subType === 'none') return base;
  return `${base} ${humanise(subType)}`;
}

/**
 * HomeKit cannot express a floor plan, obstacles, room segments or zones; those
 * live in the robovac web UI. What this bridge exposes is what HomeKit actually
 * models: power state, running state, a fan-speed selector, battery readings and
 * the dock's maintenance actions.
 */
class ValetudoVacuum {
  constructor(log, config) {
    this.log = log;
    this.config = config;
    this.name = config.name || 'Valetudo Vacuum';
    this.host = String(config.host || '').replace(/\/+$/, '');
    this.username = config.username || '';
    this.password = config.password || '';
    this.refreshInterval = Math.max(10, Number(config.refreshInterval) || 30) * 1000;
    this.exposeDockTriggers = config.exposeDockTriggers !== false;

    if (!this.host) {
      throw new Error(`${PLUGIN_NAME}: "host" is required (e.g. 192.0.2.46)`);
    }

    this.client = axios.create({
      baseURL: `${this.host}${BASE}`,
      timeout: 8000,
      headers: { 'content-type': 'application/json' },
    });
    if (this.username) {
      this.client.defaults.auth = { username: this.username, password: this.password };
    }

    this.state = {
      status: 'unknown',
      battery: null,
      fanSpeed: null,
      dockStatus: null,
      consumables: [],
    };
    this.pollHandle = null;
    this.accessory = null;
  }

  async get(path) {
    const res = await this.client.get(path);
    return res.data;
  }

  async put(path, body) {
    const res = await this.client.put(path, body);
    return res.data;
  }

  /** Fetch everything the accessories need in one pass. */
  async refresh() {
    try {
      const [state, presets, consumables] = await Promise.all([
        this.get('/state'),
        this.get('/capabilities/FanSpeedControlCapability/presets').catch(() => []),
        this.get('/capabilities/ConsumableMonitoringCapability').catch(() => []),
      ]);

      this.state.status = 'unknown';
      this.state.battery = null;
      this.state.fanSpeed = null;
      this.state.dockStatus = null;

      for (const attr of state.attributes || []) {
        switch (attr.__class) {
          case 'StatusStateAttribute':
            this.state.status = attr.value;
            break;
          case 'BatteryStateAttribute':
            this.state.battery = attr.level;
            break;
          case 'DockStatusStateAttribute':
            this.state.dockStatus = attr.value;
            break;
          case 'PresetSelectionStateAttribute':
            if (attr.type === 'fan_speed') this.state.fanSpeed = attr.value;
            break;
          default:
            break;
        }
      }

      this.state.fanPresets = Array.isArray(presets) ? presets : [];
      this.state.consumables = Array.isArray(consumables) ? consumables : [];

      this.pushState();
    } catch (err) {
      this.log.warn(`${PLUGIN_NAME}: refresh failed: ${err.message}`);
    }
  }

  pushState() {
    if (!this.accessory) return;
    const { Characteristic } = this.hap;

    this.vacuumService.updateCharacteristic(
      Characteristic.Active,
      ACTIVE_STATUSES.has(this.state.status),
    );

    if (this.state.battery !== null) {
      this.batteryService.updateCharacteristic(Characteristic.BatteryLevel, this.state.battery);
    }

    this.batteryService.updateCharacteristic(
      Characteristic.ChargingState,
      CHARGING_STATES.has(this.state.dockStatus)
        ? Characteristic.ChargingState.CHARGING
        : Characteristic.ChargingState.NOT_CHARGING,
    );
  }

  start(callback) {
    this.registerAccessory(callback);
    this.refresh();
    this.pollHandle = setInterval(() => this.refresh(), this.refreshInterval);
    callback();
  }

  stop(callback) {
    if (this.pollHandle) {
      clearInterval(this.pollHandle);
      this.pollHandle = null;
    }
    callback();
  }

  registerAccessory(callback) {
    this.hap = require('homebridge').util;
    const { Service, Characteristic } = this.hap;
    this.Characteristic = Characteristic;

    const acc = new this.hap.Accessory(this.name, PLUGIN_NAME);

    acc.addService(Service.AccessoryInformation)
      .setCharacteristic(Characteristic.Manufacturer, 'Valetudo')
      .setCharacteristic(Characteristic.Model, this.name)
      .setCharacteristic(Characteristic.SerialNumber, this.host);

    // Power.
    this.vacuumService = new Service.Vacuum('Vacuum');
    this.vacuumService
      .addCharacteristic(Characteristic.Name, this.name)
      .addLinkedCharacteristic(Characteristic.On);

    this.vacuumService.getCharacteristic(Characteristic.On)
      .onGet(async () => {
        if (this.state.status === 'unknown') await this.refresh();
        return ACTIVE_STATUSES.has(this.state.status);
      })
      .onSet(async (value) => {
        await this.control(value ? 'start' : 'stop');
      });

    this.vacuumService.addOptionalCharacteristic(Characteristic.TargetAirQuality)
      .onGet(async () => {
        if (this.state.fanSpeed === null) await this.refresh();
        return FAN_TO_AIR_QUALITY[this.state.fanSpeed] ?? 2;
      })
      .onSet(async (value) => {
        const preset = AIR_QUALITY_TO_FAN[value];
        if (!preset) return;
        try {
          await this.put('/capabilities/FanSpeedControlCapability/preset', { name: preset });
          this.state.fanSpeed = preset;
        } catch (err) {
          this.log.warn(`${PLUGIN_NAME}: fan speed failed: ${err.message}`);
        }
      });

    // Running state.
    const runState = new Service.StatefulRunState('Run State');
    runState.getCharacteristic(Characteristic.RunState)
      .onGet(() => {
        switch (this.state.status) {
          case 'cleaning':
          case 'moving':
          case 'manual_control':
            return Characteristic.RunState.RUNNING;
          case 'paused':
            return Characteristic.RunState.STOPPED;
          case 'error':
            return Characteristic.RunState.ERROR;
          case 'docked':
            return Characteristic.RunState.NOT_RUNNING;
          default:
            return Characteristic.RunState.OFF;
        }
      });

    // Battery.
    this.batteryService = new Service.Battery('Battery');
    this.batteryService.getCharacteristic(Characteristic.Name)
      .setValue(`${this.name} Battery`);
    this.batteryService.getCharacteristic(Characteristic.BatteryLevel)
      .onGet(async () => {
        if (this.state.battery === null) await this.refresh();
        return this.state.battery ?? 100;
      })
      .onSet(() => {});
    this.batteryService.getCharacteristic(Characteristic.ChargingState)
      .onGet(async () => {
        if (this.state.dockStatus === null) await this.refresh();
        return CHARGING_STATES.has(this.state.dockStatus)
          ? Characteristic.ChargingState.CHARGING
          : Characteristic.ChargingState.NOT_CHARGING;
      })
      .onSet(() => {});
    this.batteryService.getCharacteristic(Characteristic.StatusLowBattery)
      .onGet(async () => {
        if (this.state.battery === null) await this.refresh();
        return (this.state.battery ?? 100) <= 15
          ? Characteristic.StatusLowBattery.BATTERY_LEVEL_LOW
          : Characteristic.StatusLowBattery.BATTERY_LEVEL_NORMAL;
      })
      .onSet(() => {});

    acc.addService(this.vacuumService);
    acc.addService(runState);
    acc.addService(this.batteryService);

    // Percentage consumables become their own Battery services, which is the
    // closest HomeKit has to a wear indicator.
    for (const item of this.state.consumables) {
      if (!item.remaining || item.remaining.unit !== 'percent') continue;
      const label = consumableLabel(item.type, item.subType);
      const service = new Service.Battery(label);
      service.getCharacteristic(Characteristic.Name).setValue(label);
      service.getCharacteristic(Characteristic.BatteryLevel)
        .onGet(() => item.remaining.value)
        .onSet(() => {});
      service.getCharacteristic(Characteristic.StatusLowBattery)
        .onGet(() =>
          item.remaining.value <= 15
            ? Characteristic.StatusLowBattery.BATTERY_LEVEL_LOW
            : Characteristic.StatusLowBattery.BATTERY_LEVEL_NORMAL,
        )
        .onSet(() => {});
      service.updateCharacteristic(Characteristic.BatteryLevel, item.remaining.value);
      acc.addService(service);
    }

    // Dock maintenance actions as momentary switches.
    if (this.exposeDockTriggers) {
      const triggers = [
        ['Auto empty', '/capabilities/AutoEmptyDockManualTriggerCapability'],
        ['Mop wash', '/capabilities/MopDockCleanManualTriggerCapability'],
        ['Mop dry', '/capabilities/MopDockDryManualTriggerCapability'],
      ];
      for (const [label, path] of triggers) {
        const service = new Service.Switch(label);
        service.getCharacteristic(Characteristic.Name).setValue(label);
        service.getCharacteristic(Characteristic.On)
          .onGet(() => false)
          .onSet(async (value) => {
            if (!value) return;
            try {
              await this.put(path, {});
              this.log.info(`${PLUGIN_NAME}: ${label} triggered`);
            } catch (err) {
              this.log.warn(`${PLUGIN_NAME}: ${label} failed: ${err.message}`);
            }
            service.updateCharacteristic(Characteristic.On, false);
          });
        acc.addService(service);
      }
    }

    this.accessory = acc;
    this.accessory.publish();
    this.accessory.on('identify', () => this.log.info(`${PLUGIN_NAME}: identify`));

    // Consumables are only known after the first refresh, so build the extra
    // services once we have them rather than blocking startup on the network.
    this.accessory.on('get', () => {});

    if (this.state.consumables.length === 0) {
      this.get('/capabilities/ConsumableMonitoringCapability')
        .then((items) => {
          this.state.consumables = Array.isArray(items) ? items : [];
          this.addConsumableServices(callback);
        })
        .catch(() => {});
    } else {
      this.addConsumableServices(callback);
    }
  }

  addConsumableServices(callback) {
    const { Service, Characteristic } = this.hap;
    for (const item of this.state.consumables) {
      if (!item.remaining || item.remaining.unit !== 'percent') continue;
      const label = consumableLabel(item.type, item.subType);
      if (this.accessory.getServiceByName(label)) continue;

      const service = new Service.Battery(label);
      service.getCharacteristic(Characteristic.Name).setValue(label);
      service.getCharacteristic(Characteristic.BatteryLevel)
        .onGet(() => item.remaining.value)
        .onSet(() => {});
      service.getCharacteristic(Characteristic.StatusLowBattery)
        .onGet(() =>
          item.remaining.value <= 15
            ? Characteristic.StatusLowBattery.BATTERY_LEVEL_LOW
            : Characteristic.StatusLowBattery.BATTERY_LEVEL_NORMAL,
        )
        .onSet(() => {});
      service.updateCharacteristic(Characteristic.BatteryLevel, item.remaining.value);
      this.accessory.addService(service);
    }
    this.pushState();
    callback();
  }

  async control(action) {
    try {
      await this.put('/capabilities/BasicControlCapability', { action });
      this.log.info(`${PLUGIN_NAME}: ${action}`);
      setTimeout(() => this.refresh(), 1500);
    } catch (err) {
      this.log.warn(`${PLUGIN_NAME}: ${action} failed: ${err.message}`);
    }
  }
}

module.exports = (homebridge) => {
  homebridge.registerAccessory(PLUGIN_NAME, ValetudoVacuum);
};
module.exports.ValetudoVacuum = ValetudoVacuum;