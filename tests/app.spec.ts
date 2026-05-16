import { expect, test } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  page.on("pageerror", (error) => {
    throw error;
  });
  page.on("console", (message) => {
    if (message.type() === "error") {
      throw new Error(message.text());
    }
  });

  await page.goto("/");
  await expect(page.locator("#status-line")).toContainText("placements", {
    timeout: 15_000,
  });
});

test("continuous prime presets show early progress in fastest mode", async ({
  page,
}) => {
  for (const preset of ["PrimeKnight", "PrimeGap"]) {
    await page.selectOption("#board-select", "ContinuousArchimedean");
    await expect(page.locator("#piece-radius-output")).toHaveText("0.50");
    await page.selectOption("#army-preset-select", preset);
    await page.click("#start-button");

    await page.waitForFunction(
      () => {
        const text = document.querySelector("#status-line")?.textContent ?? "";
        return /^[1-9]\d* placements/.test(text);
      },
      null,
      { timeout: 20_000 },
    );

    await expect(page.locator("#placement-log")).toContainText(
      `army=${preset}`,
    );
    await page.click("#pause-button");
    await page.reload();
    await expect(page.locator("#status-line")).toContainText("placements", {
      timeout: 15_000,
    });
  }
});

test("lattice render-only controls do not reset placements", async ({
  page,
}) => {
  await page.selectOption("#board-select", "LatticeHex");
  await expect(page.locator("#status-line")).toContainText("0 placements");
  await page.waitForTimeout(500);
  await page.selectOption("#board-select", "LatticeSquare");
  await expect(page.locator("#status-line")).toContainText("0 placements");
  await page.waitForTimeout(500);

  await page.click("#step-button");
  await expect(page.locator("#status-line")).toContainText("1 placements", {
    timeout: 15_000,
  });

  await page.selectOption("#shape-select", "Hex");
  await page.evaluate(() => {
    const slider = document.querySelector<HTMLInputElement>(
      "#piece-radius-slider",
    );
    if (!slider) throw new Error("missing piece radius slider");
    slider.value = "0.30";
    slider.dispatchEvent(new Event("input", { bubbles: true }));
  });

  await expect(page.locator("#status-line")).toContainText("1 placements");
  await expect(page.locator("#placement-log")).toContainText(
    "placements logged: 1",
  );
});

test("custom rows use order labels and can be deleted to an empty placeholder", async ({
  page,
}) => {
  const rows = page.locator(".army-row");
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(0)).toContainText("1. (2, 1)");
  await expect(rows.nth(1)).toContainText("2. (2, 1)");
  await expect(rows.nth(0)).toHaveAttribute("draggable", "true");
  await expect(page.locator("#army-list")).not.toContainText("gap");

  await page.locator('.army-row button[title="Delete"]').nth(1).click();
  await page.locator('.army-row button[title="Delete"]').nth(0).click();

  await expect(page.locator(".army-empty-row")).toBeVisible();
});

test("hex shape, spiral track, and compressed export stay wired", async ({
  page,
}) => {
  await page.selectOption("#board-select", "LatticeHex");
  await page.waitForTimeout(500);
  await page.selectOption("#shape-select", "Hex");
  await page.evaluate(() => {
    const slider = document.querySelector<HTMLInputElement>(
      "#track-opacity-slider",
    );
    if (!slider) throw new Error("missing track opacity slider");
    slider.value = "45";
    slider.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await expect(page.locator("#track-opacity-output")).toHaveText("45%");

  await page.click("#step-button");
  await expect(page.locator("#status-line")).toContainText("1 placements");

  const download = page.waitForEvent("download");
  await page.click("#download-jpeg-button");
  expect((await download).suggestedFilename()).toContain("image-half");
});
