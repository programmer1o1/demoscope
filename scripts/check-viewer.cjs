// Loads generated viewer pages in headless Chromium and fails on any runtime
// error. Run through scripts/check-viewer.sh, which installs Playwright into a
// cache directory and puts it on NODE_PATH.
//
//   node scripts/check-viewer.cjs viewer1.html [viewer2.html ...]

const path = require('path');
const { chromium } = require('playwright');

const SETTLE_MS = 3000;

async function checkPage(browser, file) {
  const page = await browser.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(`uncaught: ${e.message}`));
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(`console.error: ${msg.text()}`);
  });

  await page.goto('file://' + path.resolve(file), { waitUntil: 'load', timeout: 60000 });
  await page.waitForTimeout(SETTLE_MS);
  // Run playback too, so per-frame code paths (animate, updateMultiPlayer, ...) execute.
  await page.evaluate(() => { if (typeof togglePlay === 'function') togglePlay(); });
  await page.waitForTimeout(SETTLE_MS);
  await page.close();
  return errors;
}

(async () => {
  const files = process.argv.slice(2);
  if (files.length === 0) {
    console.error('usage: check-viewer.cjs <viewer.html>...');
    process.exit(2);
  }
  const browser = await chromium.launch({
    args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'],
  });
  let failed = 0;
  for (const file of files) {
    const errors = await checkPage(browser, file);
    if (errors.length === 0) {
      console.log(`PASS  ${file}`);
    } else {
      failed++;
      console.log(`FAIL  ${file}`);
      for (const e of [...new Set(errors)].slice(0, 10)) console.log(`      ${e}`);
    }
  }
  await browser.close();
  console.log(`${files.length - failed} passed, ${failed} failed`);
  process.exit(failed ? 1 : 0);
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
