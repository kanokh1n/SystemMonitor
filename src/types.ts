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
  memory: { total: number; used: number; available: number; swapTotal: number; swapUsed: number };
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
    interfaces: { name: string; ips: string[]; rxPerSec: number; txPerSec: number }[];
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
