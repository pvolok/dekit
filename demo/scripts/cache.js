const pad = (n, w) => String(n).padStart(w, "0");
const sleep = (ms) => std.process.exec("sleep", [String(ms / 1000)]);
const still = std.env.get("DEMO_STILL") === "1";

let clock = (9 * 3600 + 41 * 60) * 1000;

function stamp() {
  const s = Math.floor(clock / 1000);
  const hms = [Math.floor(s / 3600), Math.floor(s / 60) % 60, s % 60];
  return `${hms.map((n) => pad(n, 2)).join(":")}.${pad(clock % 1000, 3)}`;
}

const startup = [
  [101, "#", "Redis version=7.2.5, bits=64, pid=812", "812:C"],
  [0, "#", "Configuration loaded", "812:C"],
  [1, "*", "monotonic clock: POSIX clock_gettime"],
  [1, "*", "Running mode=standalone, port=6379."],
  [0, "*", "Server initialized"],
  [1, "*", "Loading RDB produced by version 7.2.5"],
  [0, "*", "RDB age 52 seconds"],
  [1, "*", "RDB memory usage when created 1.62 Mb"],
  [4, "*", "Done loading RDB, keys loaded: 1204"],
  [0, "*", "DB loaded from disk: 0.005 seconds"],
  [0, "*", "Ready to accept connections tcp"],
  [322, "-", "Accepted 127.0.0.1:52144"],
  [378, "-", "Accepted 127.0.0.1:52150"],
  [4203, "-", "DB 0: 1204 keys (318 volatile)"],
  [0, "-", "2 clients connected (0 replicas)"],
  [5000, "-", "DB 0: 1207 keys (320 volatile)"],
  [0, "-", "2 clients connected (0 replicas)"],
  [5000, "-", "DB 0: 1209 keys (321 volatile)"],
  [0, "-", "2 clients connected (0 replicas)"],
  [4118, "-", "Accepted 127.0.0.1:52168"],
  [882, "-", "DB 0: 1214 keys (324 volatile)"],
  [0, "-", "3 clients connected (0 replicas)"],
  [5000, "-", "DB 0: 1216 keys (324 volatile)"],
  [0, "-", "3 clients connected (0 replicas)"],
];

const live = [
  [8210, "-", "Client closed connection id=6"],
  [1790, "-", "DB 0: 1219 keys (322 volatile)"],
  [0, "-", "2 clients connected (0 replicas)"],
  [5000, "*", "100 changes in 300 seconds. Saving..."],
  [1, "*", "Background saving started by pid 1043"],
  [29, "*", "DB saved on disk", "1043:C"],
  [71, "*", "Background saving terminated with success"],
  [4900, "-", "DB 0: 1221 keys (323 volatile)"],
  [0, "-", "2 clients connected (0 replicas)"],
  [5000, "-", "DB 0: 1224 keys (325 volatile)"],
  [0, "-", "2 clients connected (0 replicas)"],
];

async function play(lines, pace) {
  for (const [advance, mark, text, who = "812:M"] of lines) {
    clock += advance;
    if (advance >= 100) await sleep(pace(advance));
    std.log(`${who} 12 May 2026 ${stamp()} ${mark} ${text}`);
  }
}

export async function main() {
  await play(startup, () => 30);
  for (;;) {
    if (still) await sleep(60000);
    else await play(live, (a) => Math.min(4000, Math.max(1500, a / 2)));
  }
}
