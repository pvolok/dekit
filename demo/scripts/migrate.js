const sgr = (code) => (s) => `\x1b[${code}m${s}\x1b[0m`;
const bold = sgr(1), gray = sgr(90), green = sgr(32);
const sleep = (ms) => std.process.exec("sleep", [String(ms / 1000)]);

const applied = [
  "20260112_create_customers",
  "20260112_create_products",
  "20260113_create_categories",
  "20260115_create_carts",
  "20260115_create_cart_items",
  "20260120_create_orders",
  "20260120_create_order_items",
  "20260202_add_product_images",
  "20260209_create_addresses",
  "20260218_create_stock",
  "20260226_add_order_status",
  "20260305_create_reviews",
  "20260317_create_sessions",
  "20260402_add_customer_locale",
  "20260414_create_webhooks",
];

const pending = [
  ["20260428_create_discount_codes", 18],
  ["20260503_add_order_notes", 6],
  ["20260511_index_orders_by_customer", 41],
];

export async function main() {
  std.log(`${bold("migrate")}  larkspur_dev at 127.0.0.1:5432`);
  std.log(`${bold("migrate")}  18 migrations, 15 applied, 3 pending`);
  std.log("");
  for (const name of applied) std.log(gray(`  ·  ${name}`));
  for (const [name, ms] of pending) {
    await sleep(ms + 30);
    std.log(`  ${green("✓")}  ${name.padEnd(36)} ${gray(`${ms}ms`.padStart(5))}`);
  }
  std.log("");
  std.log(`${bold("migrate")}  ${green("3 migrations applied in 65ms")}`);
}
