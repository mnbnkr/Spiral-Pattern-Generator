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

async function pauseIfRunning(page) {
  const pause = page.locator("#pause-button");
  if (await pause.isEnabled()) {
    await pause.click();
  }
}

test("default simulation auto-runs", async ({ page }) => {
  await expect(page.locator("#board-select option").first()).toHaveText(
    "Triangle Lattice",
  );
  await expect(page.locator("#board-select")).toHaveValue("LatticeSquare");
  await page.waitForFunction(
    () => {
      const text = document.querySelector("#status-line")?.textContent ?? "";
      return !text.includes("0 placements") && text.includes("placements");
    },
    null,
    { timeout: 15_000 },
  );
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
  await pauseIfRunning(page);
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
  await pauseIfRunning(page);
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
  await pauseIfRunning(page);
  await page.selectOption("#board-select", "LatticeHex");
  await expect(page.locator("#shape-select")).toHaveValue("Hex");
  await page.selectOption("#shape-select", "Circle");
  await page.selectOption("#board-select", "LatticeSquare");
  await page.selectOption("#board-select", "LatticeHex");
  await expect(page.locator("#shape-select")).toHaveValue("Circle");
  await page.selectOption("#shape-select", "Hex");
  await page.waitForTimeout(500);
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

test("triangle board, refresh, collapse, pan, and wheel zoom stay wired", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await page.selectOption("#board-select", "LatticeTriangle");
  await expect(page.locator("#shape-select")).toHaveValue("Triangle");
  await expect(page.locator("#shape-option-square")).toHaveAttribute(
    "disabled",
    "disabled",
  );
  await expect(page.locator("#shape-option-hex")).toHaveAttribute(
    "disabled",
    "disabled",
  );

  await page.click("#step-button");
  await expect(page.locator("#placement-log")).toContainText("triangle(");
  await expect(page.locator("#placement-log")).toContainText("coord=triangle(0,0)");

  await page.fill("#radius-input", "12");
  await page.click("#refresh-button");
  await expect(page.locator("#board-select")).toHaveValue("LatticeTriangle");
  await expect(page.locator("#radius-input")).toHaveValue("12");
  await expect(page.locator("#status-line")).toContainText("Paused");

  await page.click("#panel-toggle-button");
  await expect(page.locator("#control-panel")).toHaveClass(/collapsed/);
  await page.click("#panel-toggle-button");
  await expect(page.locator("#control-panel")).not.toHaveClass(/collapsed/);

  await page.selectOption("#display-mode-select", "PixelOneToOne");
  await page.mouse.move(500, 360);
  await page.mouse.down();
  await page.mouse.move(560, 390);
  await page.mouse.up();
  await page.mouse.wheel(0, -120);
  await expect(page.locator("#zoom-output")).toHaveText("x5");
});

test("visual progress off is explicit and re-enabling pauses cleanly", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await page.selectOption("#board-select", "ContinuousArchimedean");
  await page.selectOption("#army-preset-select", "PrimeKnight");
  await page.uncheck("#visual-progress-toggle");
  await page.click("#start-button");
  await expect(page.locator("#status-line")).toContainText("Running silently");

  await page.check("#visual-progress-toggle");
  await expect(page.locator("#status-line")).toContainText("Paused", {
    timeout: 10_000,
  });
  await expect(page.locator("#start-button")).toBeEnabled();

  await page.selectOption("#board-select", "LatticeSquare");
  await page.click("#step-button");
  await expect(page.locator("#status-line")).toContainText("1 placements", {
    timeout: 15_000,
  });
});

test("refresh terminates a silent continuous run and keeps controls usable", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await page.selectOption("#board-select", "ContinuousArchimedean");
  await page.selectOption("#army-preset-select", "PrimeGap");
  await page.uncheck("#visual-progress-toggle");
  await page.click("#start-button");
  await expect(page.locator("#status-line")).toContainText("Running silently");

  await page.click("#refresh-button");
  await expect(page.locator("#status-line")).toContainText("Paused", {
    timeout: 10_000,
  });
  await expect(page.locator("#army-preset-select")).toHaveValue("PrimeGap");
  await expect(page.locator("#visual-progress-toggle")).not.toBeChecked();

  await page.selectOption("#board-select", "LatticeHex");
  await expect(page.locator("#shape-select")).toHaveValue("Hex");
  await page.click("#step-button");
  await expect(page.locator("#status-line")).toContainText("1 placements", {
    timeout: 15_000,
  });
});

test("large strict export reports a visible error instead of silently doing nothing", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await page.selectOption("#board-select", "LatticeHex");
  await page.fill("#radius-input", "3000");

  await page.click("#download-png-button");
  await expect(page.locator("#status-line")).toContainText("Export failed", {
    timeout: 5_000,
  });
});
