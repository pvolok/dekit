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
  [12, "starting PostgreSQL 16.4 on x86_64-pc-linux-gnu, 64-bit"],
  [1, 'listening on IPv4 address "127.0.0.1", port 5432'],
  [3, "database system was shut down at 2026-05-11 18:02:44 UTC"],
  [3, "database system is ready to accept connections"],
  [195, "connection authorized: user=app database=larkspur_dev"],
  [7, "statement: SELECT version FROM schema_migrations"],
  [15, "statement: BEGIN"],
  [4, "statement: CREATE TABLE discount_codes (code text)"],
  [18, "statement: ALTER TABLE orders ADD COLUMN note text"],
  [6, "statement: CREATE INDEX ON orders (customer_id)"],
  [41, "statement: COMMIT"],
  [490, "connection authorized: user=app database=larkspur_dev"],
  [1210, "execute s1: SELECT * FROM products WHERE category = $1"],
  [0, "parameters: $1 = 'lamps'", "DETAIL"],
  [2364, "execute s2: SELECT * FROM products WHERE id = $1"],
  [0, "parameters: $1 = '218'", "DETAIL"],
  [1741, "execute s3: INSERT INTO cart_items VALUES ($1, $2, $3)"],
  [0, "parameters: $1 = 7, $2 = 218, $3 = 1", "DETAIL"],
  [13486, "statement: BEGIN"],
  [1, "execute s4: INSERT INTO orders VALUES ($1, $2, $3)"],
  [0, "parameters: $1 = 10482, $2 = 31, $3 = 14800", "DETAIL"],
  [4, "statement: COMMIT"],
  [3427, "execute s5: SELECT * FROM orders WHERE customer_id = $1"],
  [204, "duration: 204.118 ms"],
];

const live = [
  [9000, "checkpoint starting: time"],
  [3815, "checkpoint complete: wrote 38 buffers (0.2%); write=3.8 s"],
  [6200, "execute s1: SELECT * FROM products WHERE category = $1"],
  [0, "parameters: $1 = 'desks'", "DETAIL"],
  [4100, "execute s2: SELECT * FROM products WHERE id = $1"],
  [0, "parameters: $1 = '305'", "DETAIL"],
  [5300, "execute s3: INSERT INTO cart_items VALUES ($1, $2, $3)"],
  [0, "parameters: $1 = 7, $2 = 305, $3 = 2", "DETAIL"],
  [7800, "execute s6: DELETE FROM sessions WHERE expires_at < now()"],
  [9400, "execute s2: SELECT * FROM products WHERE id = $1"],
  [0, "parameters: $1 = '412'", "DETAIL"],
];

async function play(lines, pace) {
  for (const [advance, text, level = "LOG"] of lines) {
    clock += advance;
    if (advance >= 100) await sleep(pace(advance));
    std.log(`${stamp()} ${level}:  ${text}`);
  }
}

export async function main() {
  await play(startup, () => 30);
  for (;;) {
    if (still) await sleep(60000);
    else await play(live, (a) => Math.min(4000, Math.max(1500, a / 2)));
  }
}
