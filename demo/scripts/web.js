const sgr = (code) => (s) => `\x1b[${code}m${s}\x1b[0m`;
const bold = sgr(1), gray = sgr(90), green = sgr(32), cyan = sgr(36);
const pad = (n, w) => String(n).padStart(w, "0");
const sleep = (ms) => std.process.exec("sleep", [String(ms / 1000)]);
const still = std.env.get("DEMO_STILL") === "1";
const port = std.env.get("PORT") ?? "5173";

let clock = (9 * 3600 + 41 * 60 + 1) * 1000;

function stamp() {
  const s = Math.floor(clock / 1000);
  const h = Math.floor(s / 3600) % 24;
  return `${h % 12 || 12}:${pad(Math.floor(s / 60) % 60, 2)}:${pad(s % 60, 2)} ${h < 12 ? "AM" : "PM"}`;
}

const actions = {
  hmr: "hmr update ",
  reload: "page reload ",
  deps: "new dependencies optimized: ",
};

function format(action, text) {
  const message = action in actions ? green(actions[action]) + gray(text) : green(text);
  return `${gray(stamp())} ${cyan(bold("[vite]"))} ${message}`;
}

const history = [
  [2140, "deps", "react-day-picker"],
  [3, "info", "optimized dependencies changed. reloading"],
  [4260, "hmr", "/src/pages/Product.tsx"],
  [2810, "hmr", "/src/components/PriceTag.tsx"],
  [1930, "hmr", "/src/styles/product.css"],
  [3470, "hmr", "/src/components/PriceTag.tsx"],
  [2280, "reload", "src/lib/api.ts"],
  [4010, "hmr", "/src/components/CartDrawer.tsx"],
  [2650, "hmr", "/src/components/CartLine.tsx"],
  [1720, "hmr", "/src/components/CartDrawer.tsx"],
  [3340, "hmr", "/src/pages/Checkout.tsx"],
  [2090, "hmr", "/src/pages/Checkout.tsx, /src/app.css"],
  [3860, "hmr", "/src/components/AddressForm.tsx"],
  [2470, "hmr", "/src/hooks/useCart.ts"],
  [1590, "hmr", "/src/components/AddressForm.tsx"],
  [4120, "reload", "src/lib/money.ts"],
  [2930, "hmr", "/src/pages/Orders.tsx"],
  [2210, "hmr", "/src/components/OrderRow.tsx"],
];

const live = [
  [9600, "hmr", "/src/pages/Orders.tsx"],
  [6300, "hmr", "/src/components/OrderRow.tsx"],
  [7100, "hmr", "/src/styles/orders.css"],
  [5400, "hmr", "/src/pages/Orders.tsx"],
  [8800, "reload", "src/lib/format.ts"],
  [6900, "hmr", "/src/components/OrderRow.tsx"],
];

async function play(lines, pace) {
  for (const [advance, ...line] of lines) {
    clock += advance;
    if (advance >= 100) await sleep(pace(advance));
    std.log(format(...line));
  }
}

export async function main() {
  await sleep(300);
  std.log("");
  std.log(`  ${green(bold("VITE"))} ${green("v5.4.8")}  ${gray("ready in")} ${bold("412")} ${gray("ms")}`);
  std.log("");
  std.log(`  ${green("➜")}  ${bold("Local")}:   ${cyan("http://localhost:")}${cyan(bold(port))}${cyan("/")}`);
  std.log(`  ${green("➜")}  ${gray(bold("Network"))}${gray(": use ")}${bold("--host")}${gray(" to expose")}`);
  std.log(`  ${gray(green("➜"))}  ${gray("press ")}${bold("h + enter")}${gray(" to show help")}`);
  await play(history, () => 30);
  for (;;) {
    if (still) await sleep(60000);
    else await play(live, (a) => Math.min(4000, Math.max(1500, a / 2)));
  }
}
