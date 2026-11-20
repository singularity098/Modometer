// SPDX-License-Identifier: MIT
// Copyright (c) 2026 singularity098
// Renders resources/branding/*.svg to PNG with Playwright's Chromium:
//   node scripts/branding_png.js
const path = require("path");
const fs = require("fs");
const { chromium } = require("playwright");

const dir = path.join(__dirname, "..", "resources", "branding");
const jobs = [
  ["icon.svg", "icon-512.png", 512, 512, 1],
  ["icon.svg", "icon-64.png", 64, 64, 1],
  ["banner.svg", "banner.png", 1280, 320, 2],
];

(async () => {
  const browser = await chromium.launch();
  for (const [src, out, w, h, scale] of jobs) {
    const page = await browser.newPage({ viewport: { width: w, height: h }, deviceScaleFactor: scale });
    const svg = fs.readFileSync(path.join(dir, src), "utf8");
    await page.setContent(
      `<style>html,body{margin:0;background:transparent}svg{display:block;width:${w}px;height:${h}px}</style>${svg}`);
    await page.screenshot({ path: path.join(dir, out), omitBackground: true });
    await page.close();
    console.log("wrote", path.join("resources", "branding", out));
  }
  await browser.close();
})();
