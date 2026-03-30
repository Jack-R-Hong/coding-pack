import { test, expect } from '@playwright/test';
import { initPage, goTo, goToPlugin, PLUGIN_BASE } from './helpers';

test.describe('Coding Pack — SDK Plugin Pages', () => {
  test.beforeEach(async ({ page }) => {
    await initPage(page);
  });

  // ── Overview (custom layout) ──

  test('navigates to overview from sidebar', async ({ page }) => {
    const link = page.locator(`aside a[href="${PLUGIN_BASE}/overview"]`);
    await expect(link).toBeVisible();
    await link.click();
    await page.waitForURL(`**${PLUGIN_BASE}/overview`);
    await expect(page.locator('h1')).toContainText('Coding Pack');
  });

  test('overview displays pack status section', async ({ page }) => {
    await goToPlugin(page, '/overview');
    const main = page.locator('main');
    await expect(main.locator('text=Pack Status')).toBeVisible({ timeout: 5000 });
    await expect(main.locator('text=Plugins Installed')).toBeVisible();
    await expect(main.getByText('Workflows', { exact: true }).first()).toBeVisible();
    await expect(main.locator('text=AI Agents')).toBeVisible();
  });

  test('overview displays coding and bootstrap workflow tabs', async ({ page }) => {
    await goToPlugin(page, '/overview');
    await expect(page.getByRole('button', { name: /Coding/ })).toBeVisible({ timeout: 5000 });
    await expect(page.getByRole('button', { name: /Bootstrap/ })).toBeVisible();
  });

  test('overview shows coding workflows by default', async ({ page }) => {
    await goToPlugin(page, '/overview');
    await expect(page.locator('text=coding-quick-dev')).toBeVisible({ timeout: 5000 });
    await expect(page.locator('text=coding-feature-dev')).toBeVisible();
    await expect(page.locator('text=coding-bug-fix')).toBeVisible();
  });

  test('overview switches to bootstrap workflows', async ({ page }) => {
    await goToPlugin(page, '/overview');
    const bootstrapTab = page.getByRole('button', { name: /Bootstrap/ });
    await expect(bootstrapTab).toBeVisible({ timeout: 5000 });
    await bootstrapTab.click();
    await page.waitForTimeout(300);
    await expect(page.locator('text=bootstrap-plugin')).toBeVisible();
    await expect(page.locator('text=bootstrap-cycle')).toBeVisible();
  });

  test('overview expands workflow card to show details', async ({ page }) => {
    await goToPlugin(page, '/overview');
    const header = page.locator('button').filter({ hasText: 'coding-quick-dev' });
    await expect(header).toBeVisible({ timeout: 5000 });
    await header.click();
    await page.waitForTimeout(300);
    await expect(page.getByText('quick_spec', { exact: true })).toBeVisible({ timeout: 3000 });
    await expect(page.getByText('Requires:')).toBeVisible();
  });

  test('overview shows AI team section with all agents', async ({ page }) => {
    await goToPlugin(page, '/overview');
    await expect(page.locator('text=BMAD AI Team')).toBeVisible({ timeout: 5000 });
    for (const name of ['Winston', 'Amelia', 'Quinn', 'Bob', 'Barry']) {
      await expect(page.getByText(name, { exact: true })).toBeVisible();
    }
  });

  test('overview has execute buttons on workflow cards', async ({ page }) => {
    await goToPlugin(page, '/overview');
    const execButtons = page.locator('main button:has-text("Execute")');
    await expect(execButtons.first()).toBeVisible({ timeout: 5000 });
    const count = await execButtons.count();
    expect(count).toBeGreaterThanOrEqual(6);
  });

  test('overview refresh button works', async ({ page }) => {
    await goToPlugin(page, '/overview');
    const refreshBtn = page.locator('header button:has-text("Refresh"), .page-header button:has-text("Refresh")');
    await expect(refreshBtn).toBeVisible({ timeout: 5000 });
    await refreshBtn.click();
    await page.waitForTimeout(500);
    await expect(page.locator('h1')).toContainText('Coding Pack');
  });

  // ── Board (kanban layout) ──

  test('board page is in sidebar navigation', async ({ page }) => {
    await goToPlugin(page, '/board');
    const link = page.locator(`aside a[href="${PLUGIN_BASE}/board"]`).first();
    await expect(link).toBeVisible({ timeout: 5000 });
  });

  // ── Epics (table layout) ──

  test('epics page is in sidebar navigation', async ({ page }) => {
    await goToPlugin(page, '/epics');
    const link = page.locator(`aside a[href="${PLUGIN_BASE}/epics"]`).first();
    await expect(link).toBeVisible({ timeout: 5000 });
  });

  test('epics page renders table', async ({ page }) => {
    await goToPlugin(page, '/epics');
    await expect(page.locator('table, [role="table"]')).toBeVisible({ timeout: 5000 });
  });

  test('epics table has expected columns', async ({ page }) => {
    await goToPlugin(page, '/epics');
    for (const col of ['Epic', 'Title', 'Status', 'Phase', 'Progress', 'Stories']) {
      await expect(page.getByText(col).first()).toBeVisible({ timeout: 5000 });
    }
  });

  // ── Worktrees (table layout) ──

  test('worktrees page is in sidebar navigation', async ({ page }) => {
    await goToPlugin(page, '/worktrees');
    const link = page.locator(`aside a[href="${PLUGIN_BASE}/worktrees"]`).first();
    await expect(link).toBeVisible({ timeout: 5000 });
  });

  test('worktrees page renders table or empty state', async ({ page }) => {
    await goToPlugin(page, '/worktrees');
    const main = page.locator('main');
    const hasTable = await main.locator('table, [role="table"]').isVisible({ timeout: 5000 }).catch(() => false);
    const hasEmpty = await main.getByText(/no worktrees|no active|empty/i).isVisible({ timeout: 5000 }).catch(() => false);
    expect(hasTable || hasEmpty).toBeTruthy();
  });

  test('worktrees table has expected columns', async ({ page }) => {
    await goToPlugin(page, '/worktrees');
    const table = page.locator('table, [role="table"]');
    if (await table.isVisible({ timeout: 5000 }).catch(() => false)) {
      for (const col of ['Task', 'Branch', 'Path', 'Working Tree']) {
        await expect(page.getByText(col).first()).toBeVisible({ timeout: 3000 });
      }
    }
  });

  // ── Workflows (table layout) ──

  test('workflows page renders table', async ({ page }) => {
    await goToPlugin(page, '/workflows');
    // SDK GenericTable renders a <table> element
    await expect(page.locator('table, [role="table"]')).toBeVisible({ timeout: 5000 });
  });

  test('workflows table has expected columns', async ({ page }) => {
    await goToPlugin(page, '/workflows');
    for (const col of ['Workflow ID', 'Description', 'Category', 'Steps']) {
      await expect(page.getByText(col).first()).toBeVisible({ timeout: 5000 });
    }
  });

  // ── Agents (table layout) ──

  test('agents page renders table', async ({ page }) => {
    await goToPlugin(page, '/agents');
    await expect(page.locator('table, [role="table"]')).toBeVisible({ timeout: 5000 });
  });

  test('agents table has expected columns', async ({ page }) => {
    await goToPlugin(page, '/agents');
    for (const col of ['Agent ID', 'Name', 'Role']) {
      await expect(page.getByText(col).first()).toBeVisible({ timeout: 5000 });
    }
  });

  // ── Status (detail layout) ──

  test('status page renders detail sections', async ({ page }) => {
    await goToPlugin(page, '/status');
    for (const section of ['Pack Health', 'Installed Plugins', 'Validation']) {
      await expect(page.getByText(section).first()).toBeVisible({ timeout: 5000 });
    }
  });

  // ── Execute (form layout) ──

  test('execute page renders form', async ({ page }) => {
    await goToPlugin(page, '/execute');
    await expect(page.locator('form, [role="form"]')).toBeVisible({ timeout: 5000 });
    await expect(page.getByText('Workflow').first()).toBeVisible();
    await expect(page.getByText('Task Description').first()).toBeVisible();
  });

  // ── Logs (stream layout) ──

  test('logs page renders stream view', async ({ page }) => {
    await goToPlugin(page, '/logs');
    // SDK GenericStream renders connection status
    await expect(page.locator('text=Connecting').or(page.locator('text=Connected')).or(page.locator('text=Execution Logs'))).toBeVisible({ timeout: 5000 });
  });

  // ── Legacy route fallback ──

  test('legacy /coding-pack route still works', async ({ page }) => {
    await goTo(page, '/coding-pack');
    await expect(page.locator('h1')).toContainText('Coding Pack');
  });
});
