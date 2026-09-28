const sgr = (code) => (s) => `\x1b[${code}m${s}\x1b[0m`;
const bold = sgr(1), gray = sgr(90), red = sgr(31), green = sgr(32), yellow = sgr(33), cyan = sgr(36);
const badge = (code) => (s) => `\x1b[1;7;${code}m ${s} \x1b[0m`;
const sleep = (ms) => std.process.exec("sleep", [String(ms / 1000)]);

const files = [
  ["api/cart/totals.test.ts", 12, 18],
  ["api/cart/coupons.test.ts", 9, 22],
  ["api/orders/create.test.ts", 14, 61],
  ["api/orders/refund.test.ts", 6, 34, [
    [true, "refunds a paid order in full"],
    [true, "refunds part of an order"],
    [false, "rejects a refund over the total"],
  ]],
  ["api/products/search.test.ts", 8, 27],
  ["web/src/cart/CartDrawer.test.tsx", 7, 112],
  ["web/src/checkout/AddressForm.test.tsx", 11, 86],
];

export async function main() {
  std.log("");
  std.log(`${badge(34)("RUN")} ${cyan("v2.1.8")}`);
  std.log("");
  for (const [file, count, ms, cases] of files) {
    await sleep(ms + 60);
    const took = ms >= 100 ? yellow(`${ms}ms`) : gray(`${ms}ms`);
    if (!cases) {
      std.log(` ${green("✓")} ${file} ${gray(`(${count} tests)`)} ${took}`);
      continue;
    }
    std.log(` ${yellow("❯")} ${file} ${gray(`(${count} tests |`)} ${red("1 failed")}${gray(")")} ${took}`);
    for (const [ok, name] of cases) {
      std.log(ok ? `   ${green("✓")} refund > ${name}` : red(`   × refund > ${name}`));
    }
  }
  std.log("");
  std.log(`${red("─".repeat(29))} ${badge(31)("Failed Tests 1")} ${red("─".repeat(29))}`);
  std.log("");
  std.log(`${badge(31)("FAIL")} api/orders/refund.test.ts > refund > rejects a refund over the total`);
  std.log(`${red(bold("AssertionError"))}${red(": expected 200 to be 422 // Object.is equality")}`);
  std.log("");
  std.log(green("- Expected"));
  std.log(red("+ Received"));
  std.log("");
  std.log(green("- 422"));
  std.log(red("+ 200"));
  std.log("");
  std.log(` ${cyan("❯")} api/orders/refund.test.ts${gray(":58:24")}`);
  std.log(gray("     56|     const res = await refund(order.id, { amount: 16000 });"));
  std.log(gray("     57| "));
  std.log(`     ${gray("58|")}     expect(res.status).toBe(422);`);
  std.log(`       ${gray("|")}                        ${red("^")}`);
  std.log(gray("     59|   });"));
  std.log("");
  std.log(red(`${"─".repeat(70)}[1/1]─`));
  std.log("");
  std.log(` ${gray("Test Files")}  ${red(bold("1 failed"))} ${gray("|")} ${green(bold("6 passed"))} ${gray("(7)")}`);
  std.log(`      ${gray("Tests")}  ${red(bold("1 failed"))} ${gray("|")} ${green(bold("66 passed"))} ${gray("(67)")}`);
  std.log(`   ${gray("Start at")}  09:41:01`);
  std.log(`   ${gray("Duration")}  1.62s`);
  std.log("");
  std.process.exit(1);
}
