const sgr = (code) => (s) => `\x1b[${code}m${s}\x1b[0m`;
const bold = sgr(1), gray = sgr(90), yellow = sgr(33), underline = sgr(4);
const sleep = (ms) => std.process.exec("sleep", [String(ms / 1000)]);

const files = [
  ["web/src/pages/Checkout.tsx", [
    ["48", "11", "'discount' is defined but never used", "no-unused-vars"],
    ["112", "7", "Unexpected console statement", "no-console"],
  ]],
  ["web/src/hooks/useCart.ts", [
    ["27", "9", "Prefer const over let", "prefer-const"],
  ]],
  ["api/orders/refund.ts", [
    ["31", "3", "Unexpected console statement", "no-console"],
  ]],
];

export async function main() {
  await sleep(900);
  std.log("");
  for (const [file, problems] of files) {
    std.log(underline(file));
    const width = (i) => Math.max(...problems.map((p) => p[i].length));
    const [lw, cw, mw] = [width(0), width(1), width(2)];
    for (const [line, col, text, rule] of problems) {
      std.log(`  ${gray(`${line.padStart(lw)}:${col.padEnd(cw)}`)}  ${yellow("warning")}  ${text.padEnd(mw)}  ${gray(rule)}`);
    }
    std.log("");
  }
  std.log(yellow(bold("✖ 4 problems (0 errors, 4 warnings)")));
  std.log("");
}
