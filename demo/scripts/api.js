const sgr = (code) => (s) => `\x1b[${code}m${s}\x1b[0m`;
const bold = sgr(1), gray = sgr(90), red = sgr(31), green = sgr(32), yellow = sgr(33), cyan = sgr(36);
const pad = (n, w) => String(n).padStart(w, "0");
const sleep = (ms) => std.process.exec("sleep", [String(ms / 1000)]);
const still = std.env.get("DEMO_STILL") === "1";
const port = std.env.get("PORT") ?? "4000";

let clock = (9 * 3600 + 41 * 60) * 1000 + 800;
let order = 10482;

function stamp() {
  const s = Math.floor(clock / 1000);
  const hms = [Math.floor(s / 3600), Math.floor(s / 60) % 60, s % 60];
  return `${hms.map((n) => pad(n, 2)).join(":")}.${pad(clock % 1000, 3)}`;
}

const levels = { INFO: green, WARN: yellow };

function format(kind, text, status, ms) {
  if (typeof text === "function") text = text();
  if (kind in levels) return `${gray(stamp())}  ${levels[kind](kind.padEnd(6))} ${text}`;
  const color = status >= 500 ? red : status >= 400 ? yellow : status >= 300 ? cyan : green;
  const took = `${ms}ms`.padStart(6);
  return `${gray(stamp())}  ${bold(kind.padEnd(6))} ${text.padEnd(32)} ${color(status)} ${ms >= 100 ? yellow(took) : gray(took)}`;
}

const startup = [
  [2, "INFO", "loaded config/development.toml"],
  [4, "INFO", "postgres pool ready: 10 connections"],
  [3, "INFO", "redis connected at 127.0.0.1:6379"],
  [2, "INFO", "mounted 46 routes"],
  [2, "INFO", `listening on http://127.0.0.1:${port}`],
  [1127, "GET", "/api/session", 200, 2],
  [73, "GET", "/api/products?category=lamps", 200, 14],
  [38, "GET", "/api/categories", 304, 1],
  [2326, "GET", "/api/products/218", 200, 5],
  [25, "GET", "/api/products/218/reviews", 200, 9],
  [1716, "POST", "/api/cart/items", 201, 17],
  [3384, "PATCH", "/api/cart/items/3", 200, 11],
  [3378, "POST", "/api/checkout/quote", 200, 23],
  [1335, "POST", "/api/checkout", 422, 8],
  [1, "WARN", "checkout rejected: shipping address missing"],
  [5388, "POST", "/api/checkout", 201, 61],
  [3, "INFO", "order 10482 created: 2 items, total $148.00"],
  [113, "GET", "/api/orders/10482", 200, 6],
  [3311, "GET", "/api/account/orders", 200, 212],
  [1, "WARN", "slow query: orders_for_customer took 204ms"],
  [4423, "GET", "/api/products?category=desks", 200, 12],
  [35, "GET", "/api/products/999", 404, 2],
  [3576, "DELETE", "/api/cart/items/5", 204, 7],
];

const live = [
  [9120, "GET", "/api/session", 200, 2],
  [64, "GET", "/api/products?category=desks", 200, 11],
  [5210, "GET", "/api/products/305", 200, 4],
  [31, "GET", "/api/products/305/reviews", 200, 8],
  [4480, "POST", "/api/cart/items", 201, 15],
  [20, "GET", "/api/cart", 200, 3],
  [7730, "POST", "/api/checkout/quote", 200, 21],
  [3905, "POST", "/api/checkout", 201, 58],
  [2, "INFO", () => `order ${++order} created: 1 item, total $329.00`],
  [120, "GET", () => `/api/orders/${order}`, 200, 5],
  [6040, "GET", "/api/account", 200, 3],
  [8100, "POST", "/api/session/refresh", 200, 6],
  [5200, "GET", "/api/products?q=oak", 200, 18],
];

async function play(lines, pace) {
  for (const [advance, ...line] of lines) {
    clock += advance;
    if (advance >= 100) await sleep(pace(advance));
    std.log(format(...line));
  }
}

export async function main() {
  std.log(`${bold("larkspur-api")} ${gray("2.4.0, development")}`);
  await play(startup, () => 30);
  for (;;) {
    if (still) await sleep(60000);
    else await play(live, (a) => Math.min(4000, Math.max(1500, a / 2)));
  }
}
