const sgr = (code) => (s) => `\x1b[${code}m${s}\x1b[0m`;
const gray = sgr(90), green = sgr(32);
const sleep = (ms) => std.process.exec("sleep", [String(ms / 1000)]);

const tests = [
  ["cart.spec.ts:8:5", "adds a product to the cart", 1.4],
  ["cart.spec.ts:21:5", "updates the quantity", 1.1],
  ["cart.spec.ts:34:5", "removes a product", 0.9],
  ["checkout.spec.ts:9:5", "guest checks out", 3.2],
  ["checkout.spec.ts:40:5", "asks for an address", 1.6],
  ["checkout.spec.ts:58:5", "applies a discount code", 2.3],
  ["search.spec.ts:6:5", "finds products by name", 1.2],
  ["search.spec.ts:19:5", "filters by category", 1.0],
  ["account.spec.ts:7:5", "shows past orders", 1.8],
];

export async function main() {
  std.log("");
  std.log(`Running ${tests.length} tests using 3 workers`);
  std.log("");
  for (const [i, [at, title, s]] of tests.entries()) {
    await sleep(s * 250);
    const n = String(i + 1).padStart(2);
    std.log(`  ${green("✓")} ${gray(n)} [chromium] › ${at} › ${title} ${gray(`(${s.toFixed(1)}s)`)}`);
  }
  std.log("");
  std.log(`  ${green(`${tests.length} passed`)} ${gray("(6.1s)")}`);
  std.log("");
}
