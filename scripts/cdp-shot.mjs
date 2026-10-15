// Usage: node cdp-shot.mjs <out.png> [js-to-eval]
import { chromium } from 'playwright-core';
const [, , out, code] = process.argv;
const browser = await chromium.connectOverCDP('http://127.0.0.1:9333');
const page = browser.contexts()[0].pages().find(p => p.url().includes('1420') || p.url().includes('tauri')) ?? browser.contexts()[0].pages()[0];
if (code) console.log(JSON.stringify(await page.evaluate(code), null, 1));
await page.waitForTimeout(1500);
await page.screenshot({ path: out });
console.log('url', page.url());
await browser.close();
