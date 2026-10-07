// Explicit update-provider fixtures; no Docker mutation or external release publication.
const {chromium} = require('playwright');
const AxeBuilder = require('@axe-core/playwright').default;
const assert = require('node:assert/strict');
const fs = require('node:fs');
(async () => {
  const browser = await chromium.launch(process.env.PAPERBOY_BROWSER_CHANNEL ? {channel:process.env.PAPERBOY_BROWSER_CHANNEL} : {});
  const url = process.env.PAPERBOY_PREVIEW_URL || 'http://127.0.0.1:8026';
  let checks = 0;
  for (const theme of ['light','dark']) for (const width of [1440,390,320]) {
    const context = await browser.newContext({viewport:{width,height:1000},colorScheme:theme,reducedMotion:'reduce'});
    const state = await (await context.request.get(`${url}/api/state`)).json();
    const updates = {current:'0.3.0',latest:'0.4.0',available:true,managed:true,automatic:false,phase:'idle',checked_at:new Date().toISOString(),release_url:'https://github.com/thekozugroup/Paperboy/releases/tag/v0.4.0'};
    state.updates = updates;
    const page = await context.newPage();
    const errors=[]; page.on('pageerror',e=>errors.push(e.message));
    await page.route('**/api/**', async route => {
      const pathname = new URL(route.request().url()).pathname;
      let result = {ok:true};
      if (pathname === '/api/state') result = state;
      else if (pathname === '/api/auth/status') result = {authenticated:true,initialized:true,csrf:'fixture'};
      else if (pathname === '/api/updates') result = updates;
      else if (pathname === '/api/updates/policy') updates.automatic = route.request().postDataJSON().automatic;
      else if (pathname === '/api/updates/install') {assert.equal(route.request().postDataJSON().version,'0.4.0');updates.phase='waiting';}
      else if (pathname !== '/api/updates/check') throw Error(`Unexpected request ${pathname}`);
      await route.fulfill({contentType:'application/json',body:JSON.stringify(result)});
    });
    async function check(name) {
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false,`${name}: overflow`);
      const axe=await new AxeBuilder({page}).withTags(['wcag2a','wcag2aa','wcag21aa']).analyze();
      assert.deepEqual(axe.violations.map(v=>({id:v.id,targets:v.nodes.map(n=>n.target)})),[],`${theme} ${width} ${name}`);
      checks++;
    }
    await page.goto(`${url}/#settings`);
    await page.getByRole('heading',{name:'Paperboy 0.4.0 is available.'}).waitFor();
    await check('available');
    const toggle=page.getByRole('switch',{name:'Automatic updates'});
    await toggle.click();await page.waitForFunction(()=>document.querySelector('[role=switch]').getAttribute('aria-checked')==='true');assert.equal(await toggle.getAttribute('aria-checked'),'true');
    await toggle.click();await page.waitForFunction(()=>document.querySelector('[role=switch]').getAttribute('aria-checked')==='false');assert.equal(await toggle.getAttribute('aria-checked'),'false');
    await page.locator('.update-content').screenshot({path:`evidence/updates-${theme}-${width}.png`});
    await page.getByRole('button',{name:'Install update',exact:true}).click();
    await page.getByRole('heading',{name:'Waiting for printing to finish'}).waitFor();
    assert.equal(await page.getByRole('button',{name:'Install update',exact:true}).isDisabled(),true);
    await check('waiting');
    updates.phase='finishing';
    await page.reload();await page.getByRole('heading',{name:'Finishing update'}).waitFor();
    assert.equal(await page.getByRole('switch',{name:'Automatic updates'}).isDisabled(),true);
    await check('finishing');
    updates.phase='idle';updates.pin='0.3.0';
    await page.reload();await page.getByText('Paperboy 0.3.0 · Pinned to 0.3.0').waitFor();
    assert.equal(await page.getByRole('switch',{name:'Automatic updates'}).isDisabled(),true);
    assert.equal(await page.getByRole('button',{name:'Install update',exact:true}).count(),0);
    await check('pinned');
    updates.pin='';updates.managed=false;updates.error='Couldn’t check for updates. Try again later.';
    await page.reload();await page.getByText(updates.error,{exact:true}).waitFor();
    await page.getByText('Enable updates on this server',{exact:true}).click();
    await page.getByRole('link',{name:'Installation & update guide'}).waitFor();
    await check('unmanaged/offline');assert.deepEqual(errors,[]);await context.close();
  }
  await browser.close();console.log(`Passed: ${checks} update layout/accessibility checks; manual, automatic, pinned, waiting, and offline UI.`);
})().catch(error=>{console.error(error);process.exit(1);});
