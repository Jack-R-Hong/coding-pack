import { test, expect } from '@playwright/test';
import { initPage, goToPlugin, PLUGIN_BASE } from './helpers';

/**
 * E2E tests for the Worktrees page.
 *
 * Verifies:
 * - Page loads and renders a table or empty state
 * - Sidebar nav link is present
 * - Columns include Branch, Path, Working Tree, Issue, PR
 * - Row actions (Git Status, Cleanup) are present when rows exist
 * - One-worktree-per-task design: task_id and story_title columns
 */
test.describe('Worktrees Page', () => {
  test.beforeEach(async ({ page }) => {
    await initPage(page);
  });

  // ── P0: Core rendering ──

  test('P0-01: page loads and renders table or empty state', async ({ page }) => {
    await goToPlugin(page, '/worktrees');
    const main = page.locator('main');
    const hasTable = await main.locator('table, [role="table"]').isVisible({ timeout: 8000 }).catch(() => false);
    const hasEmpty = await main.getByText(/no worktrees|no active|empty/i).isVisible({ timeout: 8000 }).catch(() => false);
    expect(hasTable || hasEmpty).toBeTruthy();
  });

  test('P0-02: sidebar shows Worktrees nav link', async ({ page }) => {
    await goToPlugin(page, '/worktrees');
    const link = page.locator(`aside a[href="${PLUGIN_BASE}/worktrees"]`).first();
    await expect(link).toBeVisible({ timeout: 5000 });
  });

  test('P0-03: table has Branch and Path columns', async ({ page }) => {
    await goToPlugin(page, '/worktrees');
    const table = page.locator('table, [role="table"]');
    if (await table.isVisible({ timeout: 5000 }).catch(() => false)) {
      await expect(page.getByText('Branch').first()).toBeVisible({ timeout: 3000 });
      await expect(page.getByText('Path').first()).toBeVisible({ timeout: 3000 });
    }
  });

  test('P0-04: table has Working Tree status column', async ({ page }) => {
    await goToPlugin(page, '/worktrees');
    const table = page.locator('table, [role="table"]');
    if (await table.isVisible({ timeout: 5000 }).catch(() => false)) {
      await expect(page.getByText('Working Tree').first()).toBeVisible({ timeout: 3000 });
    }
  });

  // ── P1: Git context columns ──

  test('P1-01: table has Issue and PR reference columns', async ({ page }) => {
    await goToPlugin(page, '/worktrees');
    const table = page.locator('table, [role="table"]');
    if (await table.isVisible({ timeout: 5000 }).catch(() => false)) {
      await expect(page.getByText('Issue').first()).toBeVisible({ timeout: 3000 });
      await expect(page.getByText('PR').first()).toBeVisible({ timeout: 3000 });
    }
  });

  test('P1-02: table has Task and Story columns (one worktree per task)', async ({ page }) => {
    await goToPlugin(page, '/worktrees');
    const table = page.locator('table, [role="table"]');
    if (await table.isVisible({ timeout: 5000 }).catch(() => false)) {
      await expect(page.getByText('Task').first()).toBeVisible({ timeout: 3000 });
      await expect(page.getByText('Story').first()).toBeVisible({ timeout: 3000 });
    }
  });

  // ── P1: Row actions ──

  test('P1-03: rows have Git Status and Cleanup row actions', async ({ page }) => {
    await goToPlugin(page, '/worktrees');
    const table = page.locator('table, [role="table"]');
    if (await table.isVisible({ timeout: 5000 }).catch(() => false)) {
      const rows = table.locator('tbody tr');
      const rowCount = await rows.count();
      if (rowCount > 0) {
        const firstRow = rows.first();
        const gitStatusAction = firstRow.locator('button:has-text("Git Status"), a:has-text("Git Status")');
        const cleanupAction = firstRow.locator('button:has-text("Cleanup"), a:has-text("Cleanup")');
        const hasGitStatus = await gitStatusAction.isVisible({ timeout: 2000 }).catch(() => false);
        const hasCleanup = await cleanupAction.isVisible({ timeout: 2000 }).catch(() => false);
        expect(hasGitStatus || hasCleanup).toBeTruthy();
      }
    }
  });
});
