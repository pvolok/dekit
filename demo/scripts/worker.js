const sgr = (code) => (s) => `\x1b[${code}m${s}\x1b[0m`;
const gray = sgr(90), green = sgr(32), yellow = sgr(33), cyan = sgr(36);
const pad = (n, w) => String(n).padStart(w, "0");
const sleep = (ms) => std.process.exec("sleep", [String(ms / 1000)]);
const still = std.env.get("DEMO_STILL") === "1";

let clock = (9 * 3600 + 41 * 60) * 1000 + 430;
let order = 10482;

function stamp() {
  const s = Math.floor(clock / 1000);
  const hms = [Math.floor(s / 3600), Math.floor(s / 60) % 60, s % 60];
  return `${hms.map((n) => pad(n, 2)).join(":")}.${pad(clock % 1000, 3)}`;
}

function format(queue, job, arg, ms) {
  if (queue === "INFO") return `${gray(stamp())}  ${green("INFO".padEnd(10))}${job}`;
  if (typeof arg === "function") arg = arg();
  const status = ms === undefined ? yellow("retry 1 in 30s") : `${green("done")} ${gray(`${ms}ms`.padStart(6))}`;
  return `${gray(stamp())}  ${cyan(queue.padEnd(10))}${job.padEnd(20)}${gray(arg.padEnd(14))}${status}`;
}

const startup = [
  [1, "INFO", "worker 2.4.0 started, concurrency 4"],
  [2, "INFO", "connected to postgres and redis"],
  [2, "INFO", "queues: default, mail, images, search, webhooks"],
  [2, "INFO", "schedule: cleanup-sessions every 15m"],
  [0, "INFO", "schedule: sales-summary daily at 02:00"],
  [470, "default", "CleanupSessions", "expired=12", 14],
  [9, "default", "ExpireCarts", "expired=3", 9],
  [1405, "images", "ResizeProductImage", "product=412", 212],
  [212, "images", "ResizeProductImage", "product=413", 198],
  [1888, "default", "SyncInventory", "supplier=4", 86],
  [1730, "default", "TrackCartEvent", "cart=7", 3],
  [3391, "default", "TrackCartEvent", "cart=7", 2],
  [10080, "mail", "OrderConfirmation", "order=10482", 84],
  [29, "default", "ReserveStock", "order=10482", 31],
  [62, "webhooks", "DeliverWebhook", "order.created"],
  [3398, "mail", "AbandonedCart", "cart=6", 61],
  [4410, "images", "ResizeProductImage", "product=414", 205],
  [4020, "default", "SyncInventory", "supplier=9", 92],
  [2940, "default", "TrackCartEvent", "cart=7", 3],
  [5550, "search", "IndexProduct", "product=412", 47],
  [22, "search", "IndexProduct", "product=413", 44],
  [21, "search", "IndexProduct", "product=414", 45],
  [9720, "webhooks", "DeliverWebhook", "order.created", 143],
  [3150, "default", "TrackCartEvent", "cart=9", 2],
];

const live = [
  [9400, "default", "TrackCartEvent", "cart=7", 3],
  [8810, "mail", "OrderConfirmation", () => `order=${++order}`, 79],
  [31, "default", "ReserveStock", () => `order=${order}`, 28],
  [58, "webhooks", "DeliverWebhook", "order.created", 131],
  [6200, "default", "SyncInventory", "supplier=4", 88],
  [7300, "images", "ResizeProductImage", "product=415", 207],
  [4100, "default", "TrackCartEvent", "cart=9", 2],
  [8900, "mail", "AbandonedCart", "cart=11", 58],
];

async function play(lines, pace) {
  for (const [advance, ...line] of lines) {
    clock += advance;
    if (advance >= 100) await sleep(pace(advance));
    std.log(format(...line));
  }
}

export async function main() {
  await play(startup, () => 30);
  for (;;) {
    if (still) await sleep(60000);
    else await play(live, (a) => Math.min(4000, Math.max(1500, a / 2)));
  }
}
