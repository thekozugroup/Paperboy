// UI contract checks with explicit fake providers. No Resend calls or print submissions.
const {chromium} = require('playwright');
const AxeBuilder = require('@axe-core/playwright').default;
const assert = require('node:assert/strict');
const fs = require('node:fs');

(async () => {
  const browser = await chromium.launch(process.env.PAPERBOY_BROWSER_CHANNEL ? {channel:process.env.PAPERBOY_BROWSER_CHANNEL} : {});
  const url = process.env.PAPERBOY_PREVIEW_URL || 'http://127.0.0.1:8026';
  const report = [];
  for (const [width, theme] of [[1440,'light'],[390,'dark']]) {
    const context = await browser.newContext({viewport:{width,height:1000},colorScheme:theme,reducedMotion:'reduce'});
    const fixture = await (await context.request.get(`${url}/api/state`)).json();
    fixture.settings.setup_complete = false;
    fixture.settings.inbox = '';
    fixture.settings.printer = null;
    fixture.settings.api_key_set = false;
    fixture.senders = []; fixture.jobs = [];
    const page = await context.newPage();
    await page.route('**/api/**', async route => {
      const pathname = new URL(route.request().url()).pathname;
      const data = route.request().postDataJSON();
      let result = {ok:true}, status = 200;
      switch (pathname) {
        case '/api/auth/status': result = {initialized:false,authenticated:false,demo:true}; break;
        case '/api/auth/setup': result = {initialized:true,authenticated:true,csrf:'test',demo:true}; break;
        case '/api/state': result = fixture; break;
        case '/api/settings/email':
          if (data.api_key === 're_bad') {status=400;result={detail:'Resend rejected the API key. Use a key with full access.'};}
          else {fixture.settings.inbox=data.inbox;fixture.settings.api_key_set=true;}
          break;
        case '/api/printers/discover': result={printers:width===1440 ? [{name:'Virtual home printer',uri:'ipp://192.168.1.50:631/ipp/print',location:'Test network'}] : [],message:'No printers found. You can add one by IP address.'}; break;
        case '/api/printer': fixture.settings.printer={...data,queue:'test'}; break;
        case '/api/senders': fixture.senders.push({...data,created_at:new Date().toISOString()}); break;
        case '/api/setup/complete': fixture.settings.setup_complete=true; break;
        default: throw new Error(`Unexpected mock route: ${pathname}`);
      }
      await route.fulfill({status,contentType:'application/json',body:JSON.stringify(result)});
    });
    async function check(step) {
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth),false);
      const results=await new AxeBuilder({page}).withTags(['wcag2a','wcag2aa','wcag21aa']).analyze();
      assert.deepEqual(results.violations.map(v=>({id:v.id,nodes:v.nodes.map(n=>n.target)})),[]);
      report.push({width,theme,step,violations:0});
    }
    await page.goto(url);
    await page.getByLabel('Setup code').fill('mock-setup');
    await page.getByLabel('Choose an owner password').fill('test-owner-password');
    await check('owner');
    await page.getByRole('button',{name:'Set up Paperboy',exact:true}).click();
    await page.getByRole('heading',{name:'Connect your inbox',exact:true}).waitFor();
    await check('email');
    await page.getByLabel('Resend API key').fill('re_bad');
    await page.getByLabel('Printing email address').fill('print@test.resend.app');
    await page.getByRole('button',{name:'Connect Resend',exact:true}).click();
    await page.getByRole('alert').filter({hasText:'Resend rejected'}).waitFor();
    await page.getByLabel('Resend API key').fill('re_mock');
    await page.getByRole('button',{name:'Connect Resend',exact:true}).click();
    await page.getByRole('heading',{name:'Find your printer',exact:true}).waitFor();
    await check('printer');
    if (width===1440) {
      await page.getByRole('button',{name:/Virtual home printer/}).click();
    } else {
      await page.getByText('Add by IP address',{exact:true}).click();
      await page.getByLabel('Printer address').fill('ipp://192.168.1.50:631/ipp/print');
      await page.getByRole('button',{name:'Connect printer',exact:true}).click();
    }
    await page.getByRole('button',{name:'Use this printer',exact:true}).click();
    await page.getByRole('heading',{name:'Make it a family thing',exact:true}).waitFor();
    await check('people');
    assert.equal(await page.getByRole('button',{name:'Start Paperboy',exact:true}).isDisabled(),true);
    await page.getByLabel('Name',{exact:true}).fill('Alex');
    await page.getByLabel('Email address',{exact:true}).fill('alex@example.com');
    await page.getByRole('button',{name:'Add person',exact:true}).click();
    await page.getByText('alex@example.com',{exact:true}).waitFor();
    await page.getByRole('button',{name:'Start Paperboy',exact:true}).click();
    await page.getByRole('heading',{name:'Activity',exact:true}).waitFor();
    await page.getByRole('heading',{name:'Waiting for the first email',exact:true}).waitFor();
    await context.close();
  }
  fs.writeFileSync('evidence/setup-qa.json',JSON.stringify({providers:'mocked',report},null,2));
  console.log('Passed: 2 complete mocked onboarding flows; 8 setup layout/accessibility checks.');
  await browser.close();
})().catch(error=>{console.error(error);process.exit(1);});
