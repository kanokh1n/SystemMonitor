// Типы данных, которые присылает Rust (src-tauri/src/metrics.rs и docker.rs).

export interface Snapshot {
  cpu: {
    usage: number;
    perCore: number[];
    brand: string;
    cores: number;
    physicalCores: number | null;
    loadAvg: [number, number, number];
  };
  memory: {
    total: number;
    used: number;
    available: number;
    swapTotal: number;
    swapUsed: number;
    breakdown: {
      app: number;
      wired: number;
      compressed: number;
      cached: number;
      pressure: number | null;
      pressureLevel: "normal" | "warning" | "critical" | null;
    } | null;
  };
  disks: {
    name: string;
    mountPoint: string;
    fileSystem: string;
    total: number;
    available: number;
    removable: boolean;
  }[];
  diskIo: { readPerSec: number; writePerSec: number };
  network: {
    rxPerSec: number;
    txPerSec: number;
    rxTotal: number;
    txTotal: number;
    interfaces: NetInterface[];
  };
  battery: {
    percent: number;
    state: "charging" | "discharging" | "full" | "empty" | "paused" | "unknown";
    healthPercent: number;
    cycleCount: number | null;
    powerWatts: number;
    minutesToEmpty: number | null;
    minutesToFull: number | null;
  } | null;
  temperatures: { label: string; celsius: number }[];
  processes: Process[];
  system: { hostName: string; os: string; kernel: string; arch: string; uptimeSecs: number };
}

export interface Process {
  pid: number;
  name: string;
  app: string | null;
  cpu: number;
  memory: number;
  readPerSec: number;
  writePerSec: number;
}

export interface DockerSnapshot {
  available: boolean;
  error: string | null;
  containers: Container[];
}

export interface Container {
  id: string;
  name: string;
  image: string;
  state: string;
  status: string;
  ports: string[];
  cpuPercent: number | null;
  memoryUsed: number | null;
  memoryLimit: number | null;
}

export interface NetInterface {
  name: string;
  mac: string;
  ipv4: string[];
  ipv6: string[];
  rxPerSec: number;
  txPerSec: number;
  rxTotal: number;
  txTotal: number;
  errors: number;
}

export interface NetDetails {
  gateway: string | null;
  defaultInterface: string | null;
  dns: string[];
  listening: { port: number; address: string; process: string; pid: number }[];
  connections: { process: string; pid: number; connections: number; remoteHosts: number }[];
}
