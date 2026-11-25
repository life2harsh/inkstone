const { test, expect, devices } = require('@playwright/test');
const { createHash, randomBytes } = require('node:crypto');
const { execSync } = require('node:child_process');

// Needs a running server; see INKSTONE_LIVE_MODE_SETUP_URL below.
const setupUrl = process.env.INKSTONE_LIVE_MODE_SETUP_URL;
// A command that runs SQL from stdin against that server's database.
const sql = process.env.INKSTONE_LIVE_SQL;
const ownerEmail = 'owner@example.test';
const ownerPassword = 'owner correct horse battery staple';

async function fillSettled(page, selector, value) {
  await expect(async () => {
    await page.locator(selector).fill(value);
    await expect(page.locator(selector)).toHaveValue(value, { timeout: 500 });
  }).toPass({ timeout: 10_000 });
}

async function webModule(page) {
  const url = await page.evaluate(() => performance.getEntriesByType('resource')
    .map(entry => entry.name)
    .find(name => /\/pkg\/(?:[^/]+\/)?inkstone_web\.js$/.test(new URL(name).pathname)));
  return url;
}

async function liveStats(page) {
  const url = await webModule(page);
  return page.evaluate(async moduleUrl => JSON.parse((await import(moduleUrl)).live_stats_for_test()), url);
}

async function openFile(page, path) {
  const file = page.locator(`.file-entry[data-path="${path}"] .file-select-btn`);
  const folder = page.getByRole('button', { name: path.split('/')[0], exact: true });
  await expect(file.or(folder)).toBeVisible({ timeout: 30_000 });
  if (!(await file.isVisible())) await folder.click();
  await file.click();
  await expect(page.locator('.page-surface')).toBeVisible({ timeout: 30_000 });
  await expect(page.locator('.loading-state, .closed-state')).toHaveCount(0);
}

// A device that has been unlocked once stays unlocked until sign out.
async function staysUnlocked(page) {
  await expect(page.locator('.explorer-new-btn')).toBeVisible({ timeout: 20_000 });
  await expect(page.locator('.unlock-password')).toHaveCount(0);
}

async function ensureSidebar(page) {
  const toggle = page.locator('.sidebar-toggle-btn');
  if (await toggle.isVisible() && !/sidebar-open/.test(await page.locator('.app-container').getAttribute('class'))) {
    await toggle.click();
  }
}

async function signIn(page, email, password) {
  await page.goto('/');
  await page.locator('.auth-email').fill(email);
  await page.locator('.auth-password').fill(password);
  await page.getByRole('button', { name: 'Sign in' }).click();
}

// Submits an account form, confirming the recovery key when asked.
async function submitWithRecovery(page, button) {
  await page.getByRole('button', { name: button }).click();
  const confirmation = page.locator('.recovery-confirmation');
  await expect(confirmation.or(page.locator('.explorer-new-btn'))).toBeVisible({ timeout: 20_000 });
  let recoveryKey = null;
  if (await confirmation.isVisible()) {
    recoveryKey = await page.locator('.recovery-key-output').inputValue();
    await confirmation.check();
    await page.getByRole('button', { name: button }).click();
  }
  return recoveryKey;
}

let ownerRecoveryKey = null;

test.describe('Live mode and accounts', () => {
  test.describe.configure({ mode: 'serial' });
  test.skip(!setupUrl, 'Set INKSTONE_LIVE_MODE_SETUP_URL to an owner setup link on a fresh server');

  test('pen strokes go peer to peer while edits stay on the server', async ({ browser }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium-desktop', 'one browser pair is enough');
    test.setTimeout(120_000);
    const owner = await (await browser.newContext()).newPage();
    const guest = await (await browser.newContext()).newPage();
    const suffix = Date.now().toString(36);

    await owner.goto(setupUrl);
    await expect(owner.locator('.owner-email')).toHaveValue(ownerEmail);
    await owner.locator('.owner-password').fill(ownerPassword);
    await owner.locator('.owner-password-confirmation').fill(ownerPassword);
    ownerRecoveryKey = await submitWithRecovery(owner, 'Create account');
    await expect(owner.locator('.explorer-new-btn')).toBeVisible({ timeout: 20_000 });

    const path = `Live ${suffix}/Board.md`;
    await owner.locator('.explorer-new-btn').click();
    await owner.locator('.explorer-create-input').fill(path);
    await owner.locator('.explorer-create-input').press('Enter');
    await expect(owner.locator('.phase-badge')).toHaveText('Live');

    const guestEmail = `guest-${suffix}@example.test`;
    await owner.locator('.settings-open-btn').click();
    await owner.locator('[data-settings-category="Sharing"]').click();
    await owner.locator('.invite-email').fill(guestEmail);
    await owner.locator('.invite-btn').click();
    await expect(owner.locator('.invite-url')).not.toHaveValue('');
    const shareUrl = await owner.locator('.invite-url').inputValue();
    await owner.locator('.settings-close-btn').click();
    await guest.goto(shareUrl);
    await guest.locator('.auth-email').fill(guestEmail);
    const guestPassword = `guest correct horse battery staple ${suffix}`;
    await guest.locator('.auth-password').fill(guestPassword);
    await submitWithRecovery(guest, 'Register and accept');
    // The owner hands the workspace key to the guest's device.
    await expect(guest.getByText('Awaiting key approval')).toBeVisible({ timeout: 20_000 });
    await owner.locator('.settings-open-btn').click();
    await owner.locator('[data-settings-category="Sharing"]').click();
    await owner.locator('.approve-key-request-btn').click();
    await expect(owner.locator('.approve-key-request-btn')).toHaveCount(0, { timeout: 20_000 });
    await owner.locator('.settings-close-btn').click();
    await guest.reload();
    await staysUnlocked(guest);
    await openFile(guest, path);
    await expect(guest.locator('.phase-badge')).toHaveText('Live');

    const guestPhone = await (await browser.newContext({ ...devices['Pixel 7'] })).newPage();
    await signIn(guestPhone, guestEmail, guestPassword);
    await staysUnlocked(guestPhone);
    await ensureSidebar(guestPhone);
    await openFile(guestPhone, path);
    await expect(guestPhone.locator('.phase-badge')).toHaveText('Live');
    await guestPhone.context().close();

    // Off by default: nothing direct.
    const ownerToggle = owner.locator('.live-toggle-btn');
    await expect(ownerToggle).toHaveAttribute('aria-pressed', 'false');
    expect((await liveStats(owner)).open).toBe(0);

    // One side turning it on is enough for a direct line both ways.
    await ownerToggle.click();
    await expect(ownerToggle).toHaveAttribute('aria-pressed', 'true');
    await expect(ownerToggle).toHaveAttribute('data-direct', '1', { timeout: 20_000 });
    await expect(ownerToggle).toContainText('Live · 1');
    await expect.poll(async () => (await liveStats(guest)).open, { timeout: 20_000 }).toBe(1);

    // A stroke in progress reaches the guest over the direct channel.
    const before = (await liveStats(guest)).received;
    await owner.locator('.ink-dock [data-page-ink="tool"][data-tool="pen"]').click();
    const surface = owner.locator('.page-surface');
    const box = await surface.boundingBox();
    await owner.mouse.move(box.x + 60, box.y + 80);
    await owner.mouse.down();
    for (let step = 1; step <= 30; step += 1) {
      await owner.mouse.move(box.x + 60 + step * 8, box.y + 80 + Math.sin(step / 3) * 20);
    }
    await expect.poll(async () => (await liveStats(guest)).received - before).toBeGreaterThan(5);
    await owner.mouse.up();

    // The finished stroke is a durable edit and arrives by the server too.
    await guest.reload();
    await staysUnlocked(guest);
    await openFile(guest, path);
    await expect.poll(() => guest.locator('#page-ink-canvas').evaluate(canvas => {
      const data = canvas.getContext('2d').getImageData(0, 0, canvas.width, canvas.height).data;
      let painted = 0;
      for (let index = 3; index < data.length; index += 4) if (data[index] > 0) painted += 1;
      return painted;
    }), { timeout: 20_000 }).toBeGreaterThan(50);

    // Turning it off drops the line on both ends.
    await ownerToggle.click();
    await expect(ownerToggle).toHaveAttribute('aria-pressed', 'false');
    expect((await liveStats(owner)).open).toBe(0);
    await expect.poll(async () => (await liveStats(guest)).open, { timeout: 10_000 }).toBe(0);

    // The choice is remembered.
    await ownerToggle.click();
    await owner.reload();
    await staysUnlocked(owner);
    await openFile(owner, path);
    await expect(owner.locator('.live-toggle-btn')).toHaveAttribute('aria-pressed', 'true', { timeout: 20_000 });
    await owner.context().close();
    await guest.context().close();
  });

  test('note text typed on a laptop and a phone reaches the other live', async ({ browser }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium-desktop', 'one browser pair is enough');
    test.setTimeout(120_000);
    const laptop = await (await browser.newContext()).newPage();
    const phone = await (await browser.newContext({ ...devices['Pixel 7'] })).newPage();
    const suffix = Date.now().toString(36);
    const path = `Sync ${suffix}.md`;

    await signIn(laptop, ownerEmail, ownerPassword);
    await staysUnlocked(laptop);
    await ensureSidebar(laptop);
    await laptop.locator('.explorer-new-btn').click();
    await laptop.locator('.explorer-create-input').fill(path);
    await laptop.locator('.explorer-create-input').press('Enter');
    await expect(laptop.locator('.document-title-input')).toBeFocused({ timeout: 15_000 });
    await laptop.keyboard.type('Shared page');
    await laptop.locator('.page-tail').click();
    await laptop.keyboard.type('Written on the laptop');
    await expect(laptop.locator('.sync-status')).toHaveText('Encrypted sync is live', { timeout: 15_000 });

    await signIn(phone, ownerEmail, ownerPassword);
    await staysUnlocked(phone);
    await ensureSidebar(phone);
    await openFile(phone, path);
    await expect(phone.locator('.document-title-input')).toHaveValue('Shared page', { timeout: 15_000 });
    await expect(phone.locator('.page-surface')).toContainText('Written on the laptop', { timeout: 15_000 });

    await laptop.keyboard.type(' and more');
    await expect(phone.locator('.page-surface')).toContainText('Written on the laptop and more', { timeout: 15_000 });

    await phone.locator('.page-tail').tap();
    await phone.keyboard.press('Enter');
    await phone.keyboard.type('Then the phone');
    await expect(laptop.locator('.page-surface')).toContainText('Then the phone', { timeout: 15_000 });
    await expect(laptop.locator('.sync-status')).toHaveText('Encrypted sync is live');
    await expect(phone.locator('.sync-status')).toHaveText('Encrypted sync is live');
    await phone.context().close();
    await laptop.context().close();
  });

  test('comments placed on a laptop reach a phone, take replies, and resolve for both', async ({ browser }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium-desktop', 'one browser pair is enough');
    test.setTimeout(120_000);
    const laptop = await (await browser.newContext()).newPage();
    const phone = await (await browser.newContext({ ...devices['Pixel 7'] })).newPage();
    const path = `Review ${Date.now().toString(36)}.md`;

    await signIn(laptop, ownerEmail, ownerPassword);
    await staysUnlocked(laptop);
    await ensureSidebar(laptop);
    await laptop.locator('.explorer-new-btn').click();
    await laptop.locator('.explorer-create-input').fill(path);
    await laptop.locator('.explorer-create-input').press('Enter');
    await expect(laptop.locator('.document-title-input')).toBeFocused({ timeout: 15_000 });
    await laptop.keyboard.type('Review');
    await laptop.locator('.page-tail').click();
    await laptop.keyboard.type('The launch plan needs a second look');
    await expect(laptop.locator('.sync-status')).toHaveText('Encrypted sync is live', { timeout: 15_000 });

    await signIn(phone, ownerEmail, ownerPassword);
    await staysUnlocked(phone);
    await ensureSidebar(phone);
    await openFile(phone, path);
    await expect(phone.locator('.page-surface')).toContainText('second look', { timeout: 15_000 });

    const dockTool = laptop.locator('.ink-dock [data-page-ink="tool"][data-tool="comment"]');
    await dockTool.click();
    await expect(dockTool).toHaveClass(/active/);
    await expect(laptop.locator('.page-surface')).toHaveClass(/ink-mode/);
    const source = await laptop.locator('.page-source').last().boundingBox();
    const box = { x: source.x + 40, y: source.y + 10 };
    await laptop.mouse.click(box.x, box.y);
    await expect(laptop.locator('.comment-card[aria-label="New comment"] .comment-input')).toBeFocused();
    await laptop.keyboard.type('Who owns this?');
    await laptop.locator('.comment-card .comment-btn.primary').click();
    await expect(laptop.locator('.comment-card[aria-label="Comment thread"]')).toBeVisible();

    const phonePin = phone.locator('.comment-pin:not(.draft)');
    await expect(phonePin).toHaveCount(1, { timeout: 15_000 });
    await phonePin.tap();
    await expect(phone.locator('.comment-card .comment-body')).toHaveText('Who owns this?');
    await phone.locator('.comment-card .comment-input[aria-label="Reply"]').fill('I do');
    await phone.getByRole('button', { name: 'Send reply' }).tap();
    await expect(laptop.locator('.comment-card .comment-entry')).toHaveCount(2, { timeout: 15_000 });
    await expect(laptop.locator('.comment-card .comment-entry').last()).toContainText('I do');

    await laptop.getByRole('button', { name: 'Resolve thread' }).click();
    await expect(laptop.locator('.comment-pin:not(.draft)')).toHaveCount(0);
    await expect(phone.locator('.comment-card .comment-card-title')).toHaveText('Resolved', { timeout: 15_000 });
    await expect(phonePin).toHaveClass(/resolved/);
    await phone.getByRole('button', { name: 'Close' }).tap();
    await expect(phonePin).toHaveCount(0);
    await expect(phone.locator('.page-surface')).toContainText('The launch plan needs a second look');
    await expect(laptop.locator('.sync-status')).toHaveText('Encrypted sync is live');
    await expect(phone.locator('.sync-status')).toHaveText('Encrypted sync is live');
    await phone.context().close();
    await laptop.context().close();
  });

  test('fast typing and ink drawn on a laptop and a phone at once all survive', async ({ browser }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium-desktop', 'one browser pair is enough');
    test.setTimeout(180_000);
    const errors = [];
    const watch = (page, label) => {
      page.on('pageerror', error => errors.push(`${label} ${error.message}`));
      page.on('console', message => {
        if (message.type() === 'error' && !/Failed to load resource/.test(message.text())) errors.push(`${label} ${message.text()}`);
      });
    };
    const laptop = await (await browser.newContext()).newPage();
    const phoneContext = await browser.newContext({ ...devices['Pixel 7'] });
    const phone = await phoneContext.newPage();
    watch(laptop, 'laptop');
    watch(phone, 'phone');
    const path = `Together ${Date.now().toString(36)}.md`;

    await signIn(laptop, ownerEmail, ownerPassword);
    await staysUnlocked(laptop);
    await ensureSidebar(laptop);
    await laptop.locator('.explorer-new-btn').click();
    await laptop.locator('.explorer-create-input').fill(path);
    await laptop.locator('.explorer-create-input').press('Enter');
    await expect(laptop.locator('.document-title-input')).toBeFocused({ timeout: 15_000 });
    await laptop.keyboard.type('Together');
    await laptop.locator('.page-tail').click();
    await laptop.keyboard.type('first line\n\nsecond line\n\nthird line\n\n- one\ntwo');
    const blockText = async page => page.evaluate(async url => {
      const doc = JSON.parse((await import(url)).active_document_json_for_test());
      return doc.blocks.map(block => block.content.text ?? '').join('|');
    }, await webModule(page));
    await expect.poll(() => blockText(laptop), { timeout: 10_000 }).toMatch(/first line.*second line.*third line.*- one\n- two/s);
    await laptop.keyboard.press('Escape');
    await expect(laptop.locator('.sync-status')).toHaveText('Encrypted sync is live', { timeout: 15_000 });

    await signIn(phone, ownerEmail, ownerPassword);
    await staysUnlocked(phone);
    await ensureSidebar(phone);
    await openFile(phone, path);
    await expect.poll(() => blockText(phone), { timeout: 15_000 }).toBe(await blockText(laptop));

    await laptop.locator('.ink-dock [data-page-ink="tool"][data-tool="pen"]').click();
    await phone.locator('.ink-dock [data-page-ink="tool"][data-tool="pen"]').click();
    const cdp = await phoneContext.newCDPSession(phone);
    const touch = (type, points) => cdp.send('Input.dispatchTouchEvent', {
      type,
      touchPoints: points.map(([x, y], id) => ({ x, y, id }))
    });
    const phoneBox = await phone.locator('.page-rendered p', { hasText: 'second line' }).boundingBox();
    const laptopBox = await laptop.locator('.page-surface').boundingBox();
    for (let round = 0; round < 6; round += 1) {
      const sx = phoneBox.x + 20 + round * 10;
      const sy = phoneBox.y + round * 6;
      await touch('touchStart', [[sx, sy]]);
      for (let step = 1; step <= 20; step += 1) await touch('touchMove', [[sx + step * 8, sy + (step % 2) * 10]]);
      await touch('touchEnd', []);
      await laptop.mouse.move(laptopBox.x + 80, laptopBox.y + 200 + round * 12);
      await laptop.mouse.down();
      for (let step = 1; step <= 16; step += 1) await laptop.mouse.move(laptopBox.x + 80 + step * 12, laptopBox.y + 200 + round * 12 + (step % 2) * 12);
      await laptop.mouse.up();
    }
    const count = async page => page.evaluate(async url => {
      const doc = JSON.parse((await import(url)).active_document_json_for_test());
      return (doc.ink?.stonepen_json.match(/"Stroke":/g) ?? []).length;
    }, await webModule(page));
    await expect.poll(async () => [await count(laptop), await count(phone)], { timeout: 20_000 }).toEqual([12, 12]);
    await laptop.locator('.page-surface').evaluate(surface => { surface.dataset.kept = '1'; });
    await laptop.mouse.move(laptopBox.x + 80, laptopBox.y + 284);
    await laptop.mouse.down();
    for (let step = 1; step <= 16; step += 1) await laptop.mouse.move(laptopBox.x + 80 + step * 12, laptopBox.y + 284 + (step % 2) * 12);
    await laptop.mouse.up();
    await expect.poll(async () => [await count(laptop), await count(phone)], { timeout: 20_000 }).toEqual([13, 13]);
    await laptop.waitForTimeout(1500);
    await expect(laptop.locator('.page-surface[data-kept="1"]')).toHaveCount(1);
    await expect(laptop.locator('.topbar-save-status')).toHaveText('Saved');
    expect(errors).toEqual([]);
    expect(await blockText(phone)).toBe(await blockText(laptop));

    const chunkFetches = [];
    phone.on('request', request => {
      if (request.method() === 'GET' && /\/api\/assets\/.*\/chunks\//.test(request.url())) chunkFetches.push(request.url());
    });
    await laptop.locator('.ink-dock [data-page-ink="type"]').click();
    await laptop.evaluate(async () => {
      const canvas = document.createElement('canvas');
      canvas.width = 120;
      canvas.height = 80;
      const context = canvas.getContext('2d');
      context.fillStyle = '#2a7';
      context.fillRect(0, 0, 120, 80);
      const blob = await new Promise(resolve => canvas.toBlob(resolve, 'image/png'));
      const transfer = new DataTransfer();
      transfer.items.add(new File([blob], 'board.png', { type: 'image/png' }));
      window.dispatchEvent(new ClipboardEvent('paste', { clipboardData: transfer, bubbles: true }));
    });
    const shown = page => page.locator('.page-rendered img[data-asset-path="Assets/board.png"]').evaluate(image => image.complete && image.naturalWidth === 120).catch(() => false);
    await expect.poll(() => shown(laptop), { timeout: 15_000 }).toBe(true);
    await expect.poll(() => shown(phone), { timeout: 30_000 }).toBe(true);
    const fetchedOnce = chunkFetches.length;
    expect(fetchedOnce).toBeGreaterThan(0);
    await phone.waitForTimeout(16_000);
    expect(chunkFetches.length).toBe(fetchedOnce);
    expect(errors).toEqual([]);
    await phoneContext.close();
    await laptop.context().close();
  });

  test('vaults are created from the switcher and shared by link to readers and guests', async ({ browser }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium-desktop', 'one browser is enough');
    test.setTimeout(120_000);
    const owner = await (await browser.newContext()).newPage();
    const suffix = Date.now().toString(36);
    await signIn(owner, ownerEmail, ownerPassword);
    await staysUnlocked(owner);
    await ensureSidebar(owner);

    const vaultName = `Research ${suffix}`;
    await owner.locator('.vault-switcher').click();
    await owner.getByRole('menuitem', { name: 'New vault' }).click();
    const dialog = owner.getByRole('dialog', { name: 'New vault' });
    await expect(dialog.locator('.new-vault-name')).toBeFocused();
    await dialog.getByRole('button', { name: 'Create vault' }).click();
    await expect(dialog.getByRole('alert')).toContainText('name');
    await dialog.locator('.new-vault-name').fill(vaultName);
    await dialog.getByRole('radio', { name: /Public link/ }).click();
    await expect(dialog.getByRole('radio', { name: /Public link/ })).toHaveAttribute('aria-checked', 'true');
    await dialog.locator('.new-vault-name').press('Enter');

    // A public vault lands on its Sharing panel with the link ready.
    const linkField = owner.locator('.public-link-url');
    await expect(linkField).toHaveValue(/#view=[\w-]{43}&key=[\w-]{43}$/, { timeout: 30_000 });
    const link = await linkField.inputValue();
    await owner.locator('.settings-close-btn').click();
    await expect(owner.locator('.vault-name')).toHaveText(vaultName);
    await expect(owner.locator('.vault-switcher')).toContainText('Public link');

    await owner.locator('.explorer-new-btn').click();
    await owner.locator('.explorer-create-input').fill('Notes/Attention');
    await owner.locator('.explorer-create-input').press('Enter');
    await expect(owner.locator('.document-title-input')).toBeFocused({ timeout: 15_000 });
    await owner.keyboard.type('Attention is all you need');
    await expect(owner.locator('.sync-status')).toHaveText('Encrypted sync is live', { timeout: 15_000 });

    // The viewer needs no account, reads everything, and can change nothing.
    const viewer = await (await browser.newContext()).newPage();
    await viewer.goto(link);
    await expect(viewer.locator('.vault-name')).toHaveText(vaultName, { timeout: 30_000 });
    await expect(viewer.locator('.vault-switcher')).toContainText('View only');
    await openFile(viewer, 'Notes/Attention.md');
    await expect(viewer.locator('h1.document-title')).toHaveText('Attention is all you need', { timeout: 15_000 });
    for (const control of ['.explorer-new-btn', '.settings-open-btn', '.logout-btn', '.document-title-input']) {
      await expect(viewer.locator(control)).toHaveCount(0);
    }

    // Edits arrive live.
    await owner.keyboard.type(' (2017)');
    await expect(viewer.locator('h1.document-title')).toHaveText('Attention is all you need (2017)', { timeout: 15_000 });

    // The other vault is still there to switch back to.
    await owner.locator('.vault-switcher').click();
    await expect(owner.getByRole('menuitemradio')).toHaveCount(2);
    await owner.keyboard.press('Escape');
    await expect(owner.locator('.vault-menu')).toHaveCount(0);

    // Letting the link edit turns visitors into named guests, no account needed.
    await owner.locator('.vault-switcher').click();
    await owner.getByRole('menuitem', { name: 'Share this vault' }).click();
    await owner.getByRole('radio', { name: 'Edit' }).click();
    await expect(owner.getByRole('radio', { name: 'Edit' })).toHaveAttribute('aria-checked', 'true', { timeout: 15_000 });
    await expect(owner.locator('.public-link-url')).toHaveValue(link);
    await owner.locator('.settings-close-btn').click();
    await expect(owner.locator('.vault-switcher')).toContainText('Edit link');

    const guest = await (await browser.newContext()).newPage();
    await guest.goto(link);
    await expect(guest.getByRole('heading', { name: 'Join as a guest' })).toBeVisible({ timeout: 30_000 });
    await expect(guest.locator('.guest-name')).toBeFocused();
    await guest.getByRole('button', { name: 'Join', exact: true }).click();
    await expect(guest.getByRole('alert')).toContainText('name');
    await guest.locator('.guest-name').fill('Rahul');
    await guest.locator('.guest-name').press('Enter');
    await expect(guest.locator('.vault-switcher')).toContainText('Editing as Rahul', { timeout: 30_000 });
    await openFile(guest, 'Notes/Attention.md');
    const guestTitle = guest.locator('.document-title-input');
    await expect(guestTitle).toHaveValue('Attention is all you need (2017)', { timeout: 15_000 });
    await guestTitle.click();
    await guestTitle.press('End');
    await guest.keyboard.type(' by Vaswani');
    await expect(viewer.locator('h1.document-title')).toHaveText('Attention is all you need (2017) by Vaswani', { timeout: 15_000 });

    // The guest stays signed in on reload and shows up by name for the owner.
    await guest.reload();
    await expect(guest.locator('.vault-switcher')).toContainText('Editing as Rahul', { timeout: 30_000 });
    await expect(guest.getByRole('heading', { name: 'Join as a guest' })).toHaveCount(0);
    await owner.locator('.vault-switcher').click();
    await owner.getByRole('menuitem', { name: 'Share this vault' }).click();
    await expect(owner.locator('.settings-member-row', { hasText: 'Rahul (guest)' })).toBeVisible({ timeout: 15_000 });

    // Turning the link off locks the viewer and the guest out at once.
    owner.once('dialog', confirm => confirm.accept());
    await owner.getByRole('button', { name: 'Turn off link' }).click();
    await expect(owner.getByRole('button', { name: 'Create public link' })).toBeVisible({ timeout: 15_000 });
    await expect(owner.locator('.settings-member-row', { hasText: 'Rahul (guest)' })).toHaveCount(0, { timeout: 15_000 });
    await expect(viewer.getByText("This link isn't available")).toBeVisible({ timeout: 20_000 });
    await expect(guest.getByText("This link isn't available")).toBeVisible({ timeout: 20_000 });
    await viewer.goto(link);
    await expect(viewer.getByText("This link isn't available")).toBeVisible({ timeout: 20_000 });
  });

  test('a note with unsent ink keeps syncing in the background while another note is open', async ({ browser }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium-desktop', 'one browser pair is enough');
    test.setTimeout(120_000);
    const laptopContext = await browser.newContext();
    const laptop = await laptopContext.newPage();
    const phone = await (await browser.newContext({ ...devices['Pixel 7'] })).newPage();
    const suffix = Date.now().toString(36);
    const sketch = `Background ${suffix}/Sketch.md`;
    const other = `Background ${suffix}/Other.md`;

    await signIn(laptop, ownerEmail, ownerPassword);
    await staysUnlocked(laptop);
    for (const path of [other, sketch]) {
      await laptop.locator('.explorer-new-btn').click();
      await laptop.locator('.explorer-create-input').fill(path);
      await laptop.locator('.explorer-create-input').press('Enter');
      await expect(laptop.locator('.sync-status')).toHaveText('Encrypted sync is live', { timeout: 15_000 });
    }
    await laptop.locator('.page-tail').click();
    await laptop.keyboard.type('Before the sketch');
    await signIn(phone, ownerEmail, ownerPassword);
    await staysUnlocked(phone);
    await ensureSidebar(phone);
    await openFile(phone, sketch);
    await expect(phone.locator('.page-surface')).toContainText('Before the sketch', { timeout: 15_000 });

    await laptopContext.setOffline(true);
    await laptop.locator('.ink-dock [data-page-ink="tool"][data-tool="pen"]').click();
    const box = await laptop.locator('.page-surface').boundingBox();
    await laptop.mouse.move(box.x + 120, box.y + 260);
    await laptop.mouse.down();
    await laptop.mouse.move(box.x + 220, box.y + 280, { steps: 8 });
    await laptop.mouse.move(box.x + 320, box.y + 250, { steps: 8 });
    await laptop.mouse.up();
    await laptop.locator('.ink-dock [data-page-ink="type"]').click();
    await laptop.waitForTimeout(1000);
    await laptop.locator(`.file-entry[data-path="${other}"] .file-select-btn`).click();

    await phone.locator('.page-tail').tap();
    await phone.keyboard.press('Enter');
    await phone.keyboard.type('Added on the phone');
    await expect(phone.locator('.topbar-save-status')).toHaveText(/Saved/, { timeout: 15_000 });

    await laptopContext.setOffline(false);
    await laptop.waitForTimeout(20_000);
    await expect(laptop.locator('.sync-error', { hasText: 'Quarantined' })).toHaveCount(0);
    await laptop.locator(`.file-entry[data-path="${sketch}"] .file-select-btn`).click();
    await expect(laptop.locator('.page-surface')).toContainText('Added on the phone', { timeout: 15_000 });
    await laptopContext.close();
    await phone.context().close();
  });

  test('one password signs in, unlocks, changes, and survives a reset with the recovery key', async ({ browser }, testInfo) => {
    test.skip(testInfo.project.name !== 'chromium-desktop', 'one browser is enough');
    test.skip(!sql, 'Set INKSTONE_LIVE_SQL to reach the database for a reset token');
    test.setTimeout(120_000);
    const page = await (await browser.newContext()).newPage();

    // A new device: signing in unlocks in the same step.
    await signIn(page, ownerEmail, ownerPassword);
    await staysUnlocked(page);
    await page.reload();
    await staysUnlocked(page);

    // Changing the password needs the current one.
    const newPassword = 'a brand new owner password';
    await page.locator('.settings-open-btn').click();
    await page.locator('[data-settings-category="Password & Recovery"]').click();
    await fillSettled(page, '.current-password', 'not my password at all');
    await fillSettled(page, '.new-password', newPassword);
    await fillSettled(page, '.new-password-confirm', newPassword);
    await page.locator('.change-password-btn').click();
    await expect(page.locator('.settings-error')).toContainText('Current password is incorrect');
    await fillSettled(page, '.current-password', ownerPassword);
    await page.locator('.change-password-btn').click();
    await expect(page.locator('.settings-note', { hasText: 'Password changed.' })).toBeVisible({ timeout: 20_000 });
    await page.locator('.settings-close-btn').click();

    // Signed out, the old password no longer works and the new one opens everything.
    await ensureSidebar(page);
    await page.locator('.sidebar .logout-btn').click();
    await expect(page.locator('.auth-password')).toBeVisible({ timeout: 20_000 });
    await signIn(page, ownerEmail, ownerPassword);
    await expect(page.locator('.auth-error')).toContainText('Invalid email or password');
    await signIn(page, ownerEmail, newPassword);
    await staysUnlocked(page);

    // A reset by email can't unlock notes; the recovery key can.
    const recoveryKey = ownerRecoveryKey;
    const token = randomBytes(32).toString('base64url');
    const hash = createHash('sha256').update(token).digest('hex');
    execSync(sql, {
      input: `INSERT INTO auth_tokens (token_hash, purpose, user_id, email, expires_at, delivery_idempotency_key)
        SELECT decode('${hash}', 'hex'), 'password_reset', id, email, NOW() + INTERVAL '1 hour', gen_random_uuid()
        FROM users WHERE email = '${ownerEmail}';`,
    });
    const reset = await (await browser.newContext()).newPage();
    await reset.goto(`/#password-reset=${token}&email=${encodeURIComponent(ownerEmail)}`);
    await expect(reset.locator('.reset-email')).toHaveValue(ownerEmail);
    const resetPassword = 'reset owner password value';
    await reset.locator('.reset-password').fill(resetPassword);
    await reset.getByRole('button', { name: 'Reset password' }).click();
    await expect(reset.locator('.auth-status')).toContainText('recovery key');
    await signIn(reset, ownerEmail, resetPassword);
    await expect(reset.locator('.recovery-key-input')).toBeVisible({ timeout: 20_000 });
    await expect(reset.locator('.unlock-password')).toHaveCount(0);
    await expect(reset.locator('.recovery-password')).toHaveCount(0);
    expect(recoveryKey, 'owner setup should have kept the recovery key').toBeTruthy();
    await reset.locator('.recovery-key-input').fill(recoveryKey);
    await reset.getByRole('button', { name: 'Unlock with recovery key' }).click();
    await staysUnlocked(reset);

    // From now on the reset password does both jobs.
    const fresh = await (await browser.newContext()).newPage();
    await signIn(fresh, ownerEmail, resetPassword);
    await staysUnlocked(fresh);
  });
});
