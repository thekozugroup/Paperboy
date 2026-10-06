const { chromium } = require('playwright');
const AxeBuilder = require('@axe-core/playwright').default;
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');

(async () => {
  const browser = await chromium.launch(process.env.PAPERBOY_BROWSER_CHANNEL ? {channel:process.env.PAPERBOY_BROWSER_CHANNEL} : {});
  const context = await browser.newContext();
  const page = await context.newPage();
  const url = process.env.PAPERBOY_PREVIEW_URL || 'http://127.0.0.1:8026';
  const report = [];
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  fs.mkdirSync('evidence', {recursive:true});
  for (const theme of ['light','dark']) {
    await page.emulateMedia({colorScheme:theme, reducedMotion:'reduce'});
    for (const width of [1440,768,390,320]) {
      await page.setViewportSize({width,height:1000});
      for (const route of ['activity','people','settings']) {
        await page.goto(`${url}/#${route}`);
        await page.getByRole('heading',{name:route[0].toUpperCase()+route.slice(1),exact:true}).waitFor();
        const overflow = await page.evaluate(() => document.documentElement.scrollWidth > innerWidth);
        assert.equal(overflow,false,`${theme} ${width}px ${route}: horizontal overflow`);
        const results = await new AxeBuilder({page}).withTags(['wcag2a','wcag2aa','wcag21aa']).analyze();
        const violations = results.violations.map(v => ({id:v.id,impact:v.impact,nodes:v.nodes.map(n => n.target)}));
        report.push({theme,width,route,violations});
        assert.deepEqual(violations,[],`${theme} ${width}px ${route}: accessibility violations`);
        if (width === 1440 || width === 390) await page.screenshot({path:`evidence/${route}-${theme}-${width}.png`,fullPage:true});
      }
    }
  }
  await page.goto(url);
  await page.getByRole('button',{name:'In progress',exact:true}).click();
  await page.getByRole('heading',{name:'The queue is clear'}).waitFor();
  await page.getByRole('button',{name:'Needs attention',exact:true}).click();
  await page.getByRole('heading',{name:'Everything looks good'}).waitFor();
  await page.getByRole('button',{name:'Blocked',exact:true}).click();
  await page.getByRole('heading',{name:'No blocked messages'}).waitFor();
  await page.getByRole('button',{name:'People',exact:true}).click();
  await page.getByRole('button',{name:'Add person',exact:true}).click();
  assert.equal(await page.getByLabel('Name',{exact:true}).evaluate(el => el === document.activeElement),true);
  await page.getByRole('button',{name:'Cancel',exact:true}).click();
  await page.getByRole('button',{name:'Remove Alex',exact:true}).click();
  await page.getByText('Remove access?',{exact:true}).waitFor();
  await page.getByRole('button',{name:'Keep',exact:true}).click();
  await page.getByRole('button',{name:'Settings',exact:true}).click();
  await page.getByText('Edit email connection',{exact:true}).click();
  await page.getByLabel('Resend API key').waitFor();
  await page.getByText('Printing limits',{exact:true}).click();
  await page.getByLabel('Pages per file').waitFor();
  assert.deepEqual(errors,[],'Browser runtime errors');
  fs.writeFileSync(path.join('evidence','browser-qa.json'),JSON.stringify({report,errors,interactionChecks:8},null,2));
  console.log(`Passed: ${report.length} layout/accessibility checks; 8 interaction checks; no browser errors.`);
  await browser.close();
})().catch(error => {console.error(error);process.exit(1);});
