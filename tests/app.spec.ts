import { expect, test } from "@playwright/test";

const appBasePath = process.env.APP_BASE_PATH ?? "/Spiral-Pattern-Generator/";
const placementPageVertices = 1_048_576;

test.beforeEach(async ({ page }) => {
  page.on("pageerror", (error) => {
    throw error;
  });
  page.on("console", (message) => {
    if (message.type() === "error") {
      throw new Error(message.text());
    }
  });

  await page.goto("./");
  await expect(page.locator("#status-line")).toContainText("placements", {
    timeout: 15_000,
  });
});

async function pauseIfRunning(page) {
  const requestedPause = await page.locator("#pause-button").evaluate((button) => {
    const pauseButton = button as HTMLButtonElement;
    if (!pauseButton.disabled) {
      pauseButton.click();
      return true;
    }
    return false;
  });
  if (requestedPause) {
    await expect(page.locator("#pause-button")).toBeDisabled({ timeout: 10_000 });
    await expect
      .poll(
        async () => {
          const before = await page.locator("#status-line").textContent();
          await page.waitForTimeout(150);
          const after = await page.locator("#status-line").textContent();
          return before === after;
        },
        { timeout: 10_000 },
      )
      .toBe(true);
  }
}

async function renderedPixelCount(page) {
  return page.locator("#sim-canvas").evaluate((canvas) => {
    const source = canvas as HTMLCanvasElement;
    const copy = document.createElement("canvas");
    copy.width = source.width;
    copy.height = source.height;
    const context = copy.getContext("2d", { willReadFrequently: true });
    if (!context) throw new Error("missing 2d context");
    context.drawImage(source, 0, 0);
    const data = context.getImageData(0, 0, copy.width, copy.height).data;
    let pixels = 0;
    for (let index = 0; index < data.length; index += 4) {
      const r = data[index];
      const g = data[index + 1];
      const b = data[index + 2];
      if (Math.abs(r - 8) > 3 || Math.abs(g - 9) > 3 || Math.abs(b - 10) > 3) {
        pixels += 1;
      }
    }
    return pixels;
  });
}

async function canvasViewportMetrics(page) {
  return page.locator("#sim-canvas").evaluate((canvas) => {
    const source = canvas as HTMLCanvasElement;
    const rect = source.getBoundingClientRect();
    return {
      cssWidth: rect.width,
      cssHeight: rect.height,
      backingWidth: source.width,
      backingHeight: source.height,
      inlineWidth: source.style.width,
      inlineHeight: source.style.height,
      dpr: window.devicePixelRatio,
    };
  });
}

async function expectCanvasViewport(page, width: number, height: number) {
  await expect
    .poll(async () => {
      const metrics = await canvasViewportMetrics(page);
      return (
        Math.abs(metrics.cssWidth - width) <= 1 &&
        Math.abs(metrics.cssHeight - height) <= 1 &&
        metrics.inlineWidth === "" &&
        metrics.inlineHeight === ""
      );
    })
    .toBe(true);

  const metrics = await canvasViewportMetrics(page);
  expect(metrics.backingWidth).toBeGreaterThanOrEqual(
    Math.floor(metrics.cssWidth * metrics.dpr) - 1,
  );
  expect(metrics.backingWidth).toBeLessThanOrEqual(
    Math.ceil(metrics.cssWidth * metrics.dpr) + 1,
  );
  expect(metrics.backingHeight).toBeGreaterThanOrEqual(
    Math.floor(metrics.cssHeight * metrics.dpr) - 1,
  );
  expect(metrics.backingHeight).toBeLessThanOrEqual(
    Math.ceil(metrics.cssHeight * metrics.dpr) + 1,
  );
}

async function statusPlacementCount(page) {
  const text = await page.locator("#status-line").textContent();
  const match = text?.match(/(\d+) placements/);
  return match ? Number(match[1]) : 0;
}

async function canvasPlacementPages(page) {
  return page.locator("#sim-canvas").evaluate((canvas) => {
    return Number(canvas.getAttribute("data-placement-pages") ?? "0");
  });
}

async function lightPixelCount(page) {
  return page.locator("#sim-canvas").evaluate((canvas) => {
    const source = canvas as HTMLCanvasElement;
    const copy = document.createElement("canvas");
    copy.width = source.width;
    copy.height = source.height;
    const context = copy.getContext("2d", { willReadFrequently: true });
    if (!context) throw new Error("missing 2d context");
    context.drawImage(source, 0, 0);
    const data = context.getImageData(0, 0, copy.width, copy.height).data;
    let pixels = 0;
    for (let index = 0; index < data.length; index += 4) {
      const r = data[index];
      const g = data[index + 1];
      const b = data[index + 2];
      if (r > 180 && g > 180 && b > 180) {
        pixels += 1;
      }
    }
    return pixels;
  });
}

async function continuousSpotsForSearch(page, search: string) {
  await pauseIfRunning(page);
  await page.selectOption("#board-select", "ContinuousArchimedean");
  await page.selectOption("#placement-search-select", search);
  await page.click("#refresh-button");
  await page.click("#start-button");
  await page.waitForFunction(
    () => {
      const text = document.querySelector("#placement-log")?.textContent ?? "";
      const match = text.match(/placements logged: (\d+)/);
      return match && Number(match[1]) >= 48;
    },
    null,
    { timeout: 15_000 },
  );
  await pauseIfRunning(page);
  const log = (await page.locator("#placement-log").textContent()) ?? "";
  return [...log.matchAll(/spot=(\d+)/g)]
    .slice(0, 24)
    .map((match) => Number(match[1]));
}

test("high-radius rendering grows past the previous single-page buffer cliff", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await pauseIfRunning(page);
  await page.evaluate(() => {
    const slider = document.querySelector<HTMLInputElement>(
      "#track-opacity-slider",
    );
    if (!slider) throw new Error("missing track opacity slider");
    slider.value = "0";
    slider.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await page.fill("#radius-input", "600");
  await page.selectOption("#placement-search-select", "SpiralPath");
  await page.click("#refresh-button");
  await page.click("#start-button");

  await expect
    .poll(() => statusPlacementCount(page), {
      timeout: 100_000,
      intervals: [1_000, 2_000, 5_000],
    })
    .toBeGreaterThan(placementPageVertices);
  await expect
    .poll(() => canvasPlacementPages(page), { timeout: 10_000 })
    .toBeGreaterThanOrEqual(2);
  await pauseIfRunning(page);
  await expect(page.locator("#start-button")).toBeEnabled();
  await expect(page.locator("#pause-button")).toBeDisabled();
  expect(await renderedPixelCount(page)).toBeGreaterThan(0);
});

test("default simulation auto-runs", async ({ page }) => {
  expect(
    await page.evaluate(() => new URL(document.baseURI).pathname),
  ).toBe(appBasePath);
  await expect(page.locator("#board-select option").first()).toHaveText(
    "Triangle Lattice",
  );
  await expect(page.locator("#board-select")).toHaveValue("LatticeSquare");
  await expect(page.locator("#enemy-mode-select option").nth(0)).toHaveText(
    "Attack-set",
  );
  await expect(page.locator("#enemy-mode-select option").nth(2)).toHaveText(
    "Color-Attack-set",
  );
  await expect(page.locator("#radius-input")).toHaveValue("200");
  await expect(page.locator("#track-opacity-output")).toHaveText("10%");
  await expect(page.locator("#attack-overlay-opacity-output")).toHaveText("Off");
  await expect(page.locator("#download-jpeg-button")).toHaveText("Half JPEG");
  await expect(page.locator("#sim-canvas")).toHaveAttribute(
    "data-webgl-context",
    "webgl2",
  );
  await page.waitForFunction(
    () => {
      const text = document.querySelector("#status-line")?.textContent ?? "";
      return !text.includes("0 placements") && text.includes("placements");
    },
    null,
    { timeout: 15_000 },
  );
  await expect
    .poll(() => renderedPixelCount(page), { timeout: 10_000 })
    .toBeGreaterThan(0);
  await pauseIfRunning(page);
});

test("WebGL1 fallback renders when WebGL2 is unavailable", async ({ page }) => {
  await page.addInitScript(() => {
    const originalGetContext = HTMLCanvasElement.prototype.getContext;
    (HTMLCanvasElement.prototype as unknown as { getContext: unknown }).getContext =
      function patchedGetContext(
        this: HTMLCanvasElement,
        contextId: string,
        ...args: unknown[]
      ) {
        if (contextId === "webgl2") {
          return null;
        }
        return (originalGetContext as unknown as Function).call(
          this,
          contextId,
          ...args,
        );
      };
  });

  await page.goto("./");
  await expect(page.locator("#status-line")).toContainText("placements", {
    timeout: 15_000,
  });
  await expect(page.locator("#sim-canvas")).toHaveAttribute(
    "data-webgl-context",
    "webgl1",
  );
  await expect
    .poll(() => renderedPixelCount(page), { timeout: 10_000 })
    .toBeGreaterThan(0);
  await pauseIfRunning(page);
});

test("mobile viewport rendering stays nonblank and uses device pixels", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.reload();
  await expect(page.locator("#status-line")).toContainText("placements", {
    timeout: 15_000,
  });
  await expect
    .poll(() => renderedPixelCount(page), { timeout: 10_000 })
    .toBeGreaterThan(0);

  await expectCanvasViewport(page, 390, 844);
  await pauseIfRunning(page);
});

test("canvas remains viewport-sized and nonblank after browser resize", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await expectCanvasViewport(page, 1280, 720);

  await page.setViewportSize({ width: 640, height: 540 });
  await expectCanvasViewport(page, 640, 540);
  await expect
    .poll(() => renderedPixelCount(page), { timeout: 10_000 })
    .toBeGreaterThan(0);

  await page.setViewportSize({ width: 1120, height: 680 });
  await expectCanvasViewport(page, 1120, 680);
  await expect
    .poll(() => renderedPixelCount(page), { timeout: 10_000 })
    .toBeGreaterThan(0);
});

test("custom finite panel layout and color swatch overrides work", async ({
  page,
}) => {
  await pauseIfRunning(page);

  const layout = await page.evaluate(() => {
    const rect = (id: string) => document.getElementById(id)?.getBoundingClientRect();
    return {
      panelWidth: document.querySelector("#control-panel")?.getBoundingClientRect()
        .width,
      radiusRightOfRadius:
        (rect("piece-radius-slider")?.left ?? 0) >
        (rect("radius-input")?.left ?? 0),
      displayModeLeftOfTrack:
        (rect("display-mode-select")?.left ?? 0) <
        (rect("track-opacity-slider")?.left ?? 0),
      visualBelowDisplay:
        (rect("visual-progress-toggle")?.top ?? 0) >
        (rect("display-mode-select")?.top ?? 0),
      searchRightOfEnemy:
        (rect("placement-search-select")?.left ?? 0) >
        (rect("enemy-mode-select")?.left ?? 0),
      offsetRightOfAttack:
        (rect("continuous-offset-input")?.left ?? 0) >
        (rect("attacking-toggle")?.left ?? 0),
      presetMoveOrder: [
        rect("piece-a-input")?.left ?? 0,
        rect("piece-b-input")?.left ?? 0,
        rect("army-preset-select")?.left ?? 0,
      ],
      checkboxAccent: getComputedStyle(
        document.querySelector("#visual-progress-toggle")!,
      ).accentColor,
      addRandomDividerWidth: document
        .querySelector(".piece-action-divider")
        ? getComputedStyle(document.querySelector(".piece-action-divider")!).width
        : "",
    };
  });

  expect(layout.panelWidth).toBeGreaterThanOrEqual(455);
  expect(layout.radiusRightOfRadius).toBe(true);
  expect(layout.displayModeLeftOfTrack).toBe(true);
  expect(layout.visualBelowDisplay).toBe(true);
  expect(layout.searchRightOfEnemy).toBe(true);
  expect(layout.offsetRightOfAttack).toBe(true);
  expect(layout.presetMoveOrder[0]).toBeLessThan(layout.presetMoveOrder[1]);
  expect(layout.presetMoveOrder[1]).toBeLessThan(layout.presetMoveOrder[2]);
  expect(layout.checkboxAccent).toBe("rgb(85, 167, 255)");
  expect(layout.addRandomDividerWidth).toBe("1px");
  await expect(page.locator('label[for="army-preset-select"] .label-row')).toContainText(
    "Army Preset",
  );
  await expect(page.locator("#piece-a-label")).toContainText("First Move");
  await expect(page.locator("#piece-b-label")).toContainText("Turn Move");

  await expect(page.locator(".army-row").first().locator(".army-piece-move")).toHaveText(
    "(2, 1)",
  );
  await expect(page.locator(".army-row").first().locator(".army-piece-name")).toHaveText(
    "Knight",
  );
  const armyLabelWeights = await page.locator(".army-row").first().evaluate((row) => ({
    move: getComputedStyle(row.querySelector(".army-piece-move")!).fontWeight,
    name: getComputedStyle(row.querySelector(".army-piece-name")!).fontWeight,
    swatchCursor: getComputedStyle(row.querySelector(".army-swatch")!).cursor,
  }));
  expect(Number(armyLabelWeights.move)).toBeGreaterThan(600);
  expect(Number(armyLabelWeights.name)).toBeLessThan(600);
  expect(armyLabelWeights.swatchCursor).toBe("grab");

  const rowBeforeHover = await page
    .locator(".army-row")
    .first()
    .evaluate((row) => getComputedStyle(row).backgroundColor);
  await page.locator(".army-row").first().hover();
  await page.waitForTimeout(180);
  const rowAfterHover = await page
    .locator(".army-row")
    .first()
    .evaluate((row) => getComputedStyle(row).backgroundColor);
  expect(rowAfterHover).not.toBe(rowBeforeHover);

  await page.locator(".army-swatch").first().hover();
  await page.waitForTimeout(180);
  const swatchHover = await page.locator(".army-row").first().evaluate((row) => {
    const swatch = row.querySelector(".army-swatch")!;
    return {
      rowBackground: getComputedStyle(row).backgroundColor,
      swatchOutline: getComputedStyle(swatch).outlineWidth,
      swatchOutlineColor: getComputedStyle(swatch).outlineColor,
    };
  });
  expect(swatchHover.rowBackground).toBe(rowBeforeHover);
  expect(swatchHover.swatchOutline).toBe("2px");
  expect(swatchHover.swatchOutlineColor).toBe("rgb(85, 167, 255)");

  const firstColor = await page
    .locator(".army-swatch")
    .first()
    .evaluate((swatch) => getComputedStyle(swatch).backgroundColor);
  await expect(page.locator("#sim-canvas")).toHaveAttribute(
    "data-color-saturation",
    "normal",
  );
  await page.locator(".army-swatch").first().click();
  await expect(page.locator("#sim-canvas")).toHaveAttribute(
    "data-color-saturation",
    "normal",
  );
  const clicked = await page.locator(".army-swatch").evaluateAll((swatches) =>
    swatches.map((swatch) => ({
      color: getComputedStyle(swatch).backgroundColor,
      override: swatch.classList.contains("custom-color-override"),
      borderWidth: getComputedStyle(swatch).borderTopWidth,
      markerWidth: getComputedStyle(swatch, "::after").width,
    })),
  );
  expect(clicked[0]).toEqual({
    color: firstColor,
    override: true,
    borderWidth: "2px",
    markerWidth: "9px",
  });
  await page.locator(".army-swatch").first().click();
  await expect(page.locator("#sim-canvas")).toHaveAttribute(
    "data-color-saturation",
    "normal",
  );
  const reset = await page.locator(".army-swatch").first().evaluate((swatch) => ({
    color: getComputedStyle(swatch).backgroundColor,
    override: swatch.classList.contains("custom-color-override"),
  }));
  expect(reset.override).toBe(false);
  expect(reset.color).toBe(firstColor);

  await page.locator(".army-swatch").first().dragTo(page.locator(".army-swatch").nth(1));
  const copied = await page.locator(".army-swatch").evaluateAll((swatches) =>
    swatches.map((swatch) => ({
      color: getComputedStyle(swatch).backgroundColor,
      override: swatch.classList.contains("custom-color-override"),
    })),
  );
  expect(copied[1]).toEqual({ color: copied[0].color, override: true });

  const tooltip = page.locator(".global-tooltip");
  await page.locator("#add-piece-button .info-icon").hover();
  await expect(tooltip).toBeHidden();
  await page.waitForTimeout(200);
  await expect(tooltip).toBeHidden();
  await page.waitForTimeout(250);
  await expect(tooltip).toBeVisible();
  await page.mouse.move(10, 10);
  await expect(tooltip).toBeHidden();

  await page.locator("#random-pool-toggle-button").click();
  await expect(page.locator(".random-pool-row").first()).toContainText(
    "(2, 1) Knight",
  );
  const poolLabelWeights = await page
    .locator(".random-pool-row")
    .first()
    .evaluate((row) => ({
      move: getComputedStyle(row.querySelector(".pool-piece-move")!).fontWeight,
      name: getComputedStyle(row.querySelector(".pool-piece-name")!).fontWeight,
    }));
  expect(Number(poolLabelWeights.move)).toBeGreaterThan(600);
  expect(Number(poolLabelWeights.name)).toBeLessThan(600);
});

test("radius border renders with track off and subpath-loaded app is nonblank", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await page.fill("#radius-input", "12");
  await page.evaluate(() => {
    const slider = document.querySelector<HTMLInputElement>(
      "#track-opacity-slider",
    );
    if (!slider) throw new Error("missing track opacity slider");
    slider.value = "0";
    slider.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await page.click("#refresh-button");
  await expect(page.locator("#track-opacity-output")).toHaveText("Off");
  await expect
    .poll(() => renderedPixelCount(page), { timeout: 10_000 })
    .toBeGreaterThan(0);

  await page.evaluate(() => {
    const slider = document.querySelector<HTMLInputElement>(
      "#track-opacity-slider",
    );
    if (!slider) throw new Error("missing track opacity slider");
    slider.value = "50";
    slider.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await expect(page.locator("#track-opacity-output")).toHaveText("50%");
  await expect
    .poll(() => renderedPixelCount(page), { timeout: 10_000 })
    .toBeGreaterThan(0);
});

test("continuous center distance uses the configured unit-chord spiral spots", async ({
  page,
}) => {
  const spiralPath = await continuousSpotsForSearch(page, "SpiralPath");
  const centerDistance = await continuousSpotsForSearch(page, "CenterDistance");

  expect(spiralPath.length).toBeGreaterThan(8);
  expect(centerDistance.length).toBeGreaterThan(8);
  expect(centerDistance).toEqual(spiralPath);
});

test("custom move names are symmetric except on triangle lattice", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await page.fill("#piece-a-input", "0");
  await page.fill("#piece-b-input", "3");
  await page.click("#add-piece-button");
  await expect(page.locator(".army-row").last()).toContainText("(0, 3)");
  await expect(page.locator(".army-row").last()).toContainText("Spehbed");

  await page.selectOption("#board-select", "LatticeTriangle");
  await expect(page.locator(".army-row").last()).toContainText("(0, 3)");
  await expect(page.locator(".army-row").last().locator(".army-piece-name")).toHaveCount(0);
});

test("continuous prime presets show early progress in fastest mode", async ({
  page,
}) => {
  for (const preset of ["PrimeKnight", "PrimeGapper"]) {
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

test("1/s speed remains slow after faster slider changes", async ({ page }) => {
  await pauseIfRunning(page);
  await page.locator("#fastest-toggle").uncheck();
  await page.evaluate(() => {
    const slider = document.querySelector<HTMLInputElement>("#speed-slider");
    if (!slider) throw new Error("missing speed slider");
    slider.value = "1";
    slider.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await expect(page.locator("#speed-output")).toHaveText("1/s");
  await page.click("#refresh-button");
  await page.click("#start-button");

  await page.waitForTimeout(750);
  const slowInitial = await statusPlacementCount(page);
  expect(slowInitial).toBeLessThanOrEqual(1);
  await page.waitForTimeout(700);
  const first = await statusPlacementCount(page);
  expect(first).toBeLessThanOrEqual(2);

  await page.evaluate(() => {
    const slider = document.querySelector<HTMLInputElement>("#speed-slider");
    if (!slider) throw new Error("missing speed slider");
    slider.value = "200";
    slider.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await expect
    .poll(() => statusPlacementCount(page), { timeout: 5_000 })
    .toBeGreaterThan(first + 2);

  await page.evaluate(() => {
    const slider = document.querySelector<HTMLInputElement>("#speed-slider");
    if (!slider) throw new Error("missing speed slider");
    slider.value = "1";
    slider.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await expect(page.locator("#speed-output")).toHaveText("1/s");
  const slowed = await statusPlacementCount(page);
  await page.waitForTimeout(650);
  expect(await statusPlacementCount(page)).toBeLessThanOrEqual(slowed + 1);
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
  await expect(page.locator('.army-row button[title="Move up"]').nth(0)).toHaveText("▲");
  await expect(page.locator('.army-row button[title="Move down"]').nth(0)).toHaveText("▼");
  await expect(page.locator("#army-list")).not.toContainText("gap");

  await page.locator('.army-row button[title="Delete"]').nth(1).click();
  await page.locator('.army-row button[title="Delete"]').nth(0).click();

  await expect(page.locator(".army-empty-row")).toBeVisible();
});

test("panel header remains visible while the control body scrolls", async ({
  page,
}) => {
  const before = await page.locator("#panel-toggle-button").boundingBox();
  expect(before).not.toBeNull();

  await page.locator(".panel-body").evaluate((body) => {
    body.scrollTop = body.scrollHeight;
    body.dispatchEvent(new Event("scroll"));
  });

  await expect(page.locator("#panel-toggle-button")).toBeVisible();
  const after = await page.locator("#panel-toggle-button").boundingBox();
  expect(after).not.toBeNull();
  expect(Math.abs((after?.y ?? 0) - (before?.y ?? 0))).toBeLessThanOrEqual(1);
  const bodyMetrics = await page.locator(".panel-body").evaluate((body) => {
    const element = body as HTMLElement;
    const style = getComputedStyle(element);
    return {
      gutter: style.scrollbarGutter,
      scrollWidth: element.scrollWidth,
      clientWidth: element.clientWidth,
    };
  });
  expect(bodyMetrics.gutter).toContain("stable");
  expect(bodyMetrics.scrollWidth).toBeLessThanOrEqual(bodyMetrics.clientWidth);
});

test("quick second click closes an already focused select", async ({ page }) => {
  await page.click("#enemy-mode-select");
  await expect
    .poll(() =>
      page.evaluate(() => {
        return document.activeElement?.id ?? "";
      }),
    )
    .toBe("enemy-mode-select");

  await page.dispatchEvent("#enemy-mode-select", "pointerdown", {
    bubbles: true,
    button: 0,
  });

  await expect
    .poll(() =>
      page.evaluate(() => {
        return document.activeElement?.id ?? "";
      }),
    )
    .not.toBe("enemy-mode-select");
});

test("random custom army controls populate and edit the random pool", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await expect(page.locator("#random-count-input")).toHaveValue("3");
  await expect(page.locator("#random-piece-button")).toBeEnabled();
  await expect(page.locator(".piece-action-divider")).toBeVisible();
  await expect
    .poll(() =>
      page
        .locator("#random-pool-toggle-button .mirrored-icon")
        .evaluate((icon) => getComputedStyle(icon).transform),
    )
    .toContain("-1");
  await expect(page.locator("#prime-divisor-label")).toBeHidden();

  await page.click("#random-pool-toggle-button");
  await expect(page.locator("#random-pool-toggle-button")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(page.locator(".random-pool-row")).toHaveCount(11);
  await expect(page.locator("#army-list")).toContainText("Knight");
  await expect(page.locator("#army-list")).toContainText("(2, 1)");
  await expect(page.locator(".random-pool-row .army-swatch")).toHaveCount(0);

  await page.fill("#piece-a-input", "7");
  await page.fill("#piece-b-input", "4");
  await page.click("#add-piece-button");
  await expect(page.locator(".random-pool-row")).toHaveCount(12);
  await expect(page.locator("#army-list")).toContainText("(7, 4) Custom");

  await page.fill("#random-count-input", "4");
  await page.click("#random-piece-button");
  await expect(page.locator("#random-pool-toggle-button")).toHaveAttribute(
    "aria-pressed",
    "false",
  );
  await expect(page.locator(".random-pool-row")).toHaveCount(0);
  await expect(page.locator(".army-row")).toHaveCount(4);
  await expect
    .poll(() => statusPlacementCount(page), { timeout: 15_000 })
    .toBeGreaterThan(0);

  await page.selectOption("#army-preset-select", "PrimeKnight");
  await expect(page.locator("#prime-divisor-label")).toBeVisible();
  await expect(page.locator("#piece-a-label")).toBeVisible();
  await expect(page.locator("#piece-b-label")).toBeVisible();
  await expect(page.locator("#piece-a-input")).toBeDisabled();
  await expect(page.locator("#piece-b-input")).toBeDisabled();
  await expect(page.locator("#random-piece-button")).toBeDisabled();
  await expect(page.locator("#random-count-input")).toBeDisabled();
  await expect(page.locator("#random-pool-toggle-button")).toBeDisabled();
  await page.selectOption("#army-preset-select", "PrimeGapper");
  await expect(page.locator("#prime-divisor-label")).toBeHidden();
});

test("random custom army safely restarts an active run", async ({ page }) => {
  await pauseIfRunning(page);
  await page.fill("#radius-input", "400");
  await page.click("#refresh-button");
  await page.click("#start-button");
  await expect(page.locator("#pause-button")).toBeEnabled({ timeout: 5_000 });

  await page.click("#random-piece-button");

  await expect(page.locator(".army-row")).toHaveCount(3);
  await expect
    .poll(() => statusPlacementCount(page), { timeout: 15_000 })
    .toBeGreaterThan(0);
  await expect(page.locator("#placement-log")).toContainText(
    "placements logged:",
    { timeout: 15_000 },
  );
  await pauseIfRunning(page);
  await expect(page.locator("#start-button")).toBeEnabled();
});

test("custom row edits stage without clearing the visible snapshot", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await expect(page.locator("#placement-log")).toContainText("placements logged:");
  const beforePixels = await renderedPixelCount(page);
  expect(beforePixels).toBeGreaterThan(0);

  await page.locator('.army-row button[title="Move down"]').nth(0).click();
  await expect(page.locator("#placement-log")).toContainText("placements logged:");
  await expect(page.locator("#placement-log")).not.toContainText("No placements yet.");
  await expect.poll(() => renderedPixelCount(page)).toBeGreaterThan(0);

  await page.fill("#piece-a-input", "3");
  await page.fill("#piece-b-input", "1");
  await page.click("#add-piece-button");
  await expect(page.locator("#placement-log")).toContainText("placements logged:");
  await expect(page.locator(".army-row")).toHaveCount(3);

  await page.locator('.army-row button[title="Move up"]').nth(2).click();
  await expect(page.locator("#placement-log")).toContainText("placements logged:");
  await expect(page.locator("#placement-log")).not.toContainText("No placements yet.");
  await expect.poll(() => renderedPixelCount(page)).toBeGreaterThan(0);
});

test("dragging identical custom rows is a visual no-op and preserves the snapshot", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await expect(page.locator("#placement-log")).toContainText("placements logged:");
  const beforePixels = await renderedPixelCount(page);
  expect(beforePixels).toBeGreaterThan(0);
  const beforeStatus = await page.locator("#status-line").textContent();

  const rows = page.locator(".army-row");
  await rows.nth(0).dragTo(rows.nth(1));

  await expect(page.locator("#placement-log")).toContainText("placements logged:");
  await expect(page.locator("#placement-log")).not.toContainText("No placements yet.");
  await expect.poll(() => renderedPixelCount(page)).toBeGreaterThan(0);
  expect(await page.locator("#status-line").textContent()).toBe(beforeStatus);
});

test("complete runs hide the radius border until settings change", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await page.fill("#radius-input", "1");
  await page.click("#refresh-button");
  await expect(page.locator("#sim-canvas")).toHaveAttribute(
    "data-generation-border",
    "visible",
  );

  await page.click("#start-button");
  await expect(page.locator("#status-line")).toContainText("Complete", {
    timeout: 15_000,
  });
  await expect(page.locator("#sim-canvas")).toHaveAttribute(
    "data-generation-border",
    "hidden",
  );

  await page.fill("#radius-input", "2");
  await expect(page.locator("#sim-canvas")).toHaveAttribute(
    "data-generation-border",
    "visible",
  );
});

test("canvas cursor and wheel activate Free Camera panning", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await expect
    .poll(() =>
      page.locator("#sim-canvas").evaluate((canvas) => {
        return getComputedStyle(canvas as HTMLCanvasElement).cursor;
      }),
    )
    .not.toBe("grab");
  await expect(page.locator("#zoom-row")).toHaveCount(0);
  await expect(page.locator("#zoom-slider")).toHaveCount(0);

  await page.mouse.move(500, 360);
  await page.mouse.wheel(0, -120);
  await expect(page.locator("#display-mode-select")).toHaveValue("PixelOneToOne");
  await expect(page.locator("#display-mode-select option[value='PixelOneToOne']")).toHaveText(
    "Free Camera",
  );
  await expect(page.locator("#sim-canvas")).toHaveAttribute(
    "data-camera-zoom",
    "2.000",
  );
  await expect(page.locator("#zoom-row")).toHaveCount(0);
  await page.selectOption("#display-mode-select", "PixelOneToOne");
  await expect
    .poll(() =>
      page.locator("#sim-canvas").evaluate((canvas) => {
        return getComputedStyle(canvas as HTMLCanvasElement).cursor;
      }),
    )
    .toBe("grab");

  await expect
    .poll(() =>
      page.locator("#sim-canvas").evaluate((canvas) => {
        return getComputedStyle(canvas as HTMLCanvasElement).cursor;
      }),
    )
    .toBe("grab");
});

test("mobile pinch activates Free Camera and zooms without a slider", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await expect(page.locator("#zoom-slider")).toHaveCount(0);

  await page.locator("#sim-canvas").evaluate((canvas) => {
    if (!("Touch" in window) || !("TouchEvent" in window)) {
      throw new Error("TouchEvent unavailable");
    }
    const target = canvas as HTMLCanvasElement;
    const makeTouch = (identifier: number, clientX: number, clientY: number) =>
      new Touch({
        identifier,
        target,
        clientX,
        clientY,
      });
    target.dispatchEvent(
      new TouchEvent("touchstart", {
        bubbles: true,
        cancelable: true,
        touches: [makeTouch(1, 520, 360), makeTouch(2, 620, 360)],
      }),
    );
    target.dispatchEvent(
      new TouchEvent("touchmove", {
        bubbles: true,
        cancelable: true,
        touches: [makeTouch(1, 500, 360), makeTouch(2, 640, 360)],
      }),
    );
    target.dispatchEvent(
      new TouchEvent("touchend", {
        bubbles: true,
        cancelable: true,
        touches: [],
      }),
    );
  });

  await expect(page.locator("#display-mode-select")).toHaveValue("PixelOneToOne");
  await expect
    .poll(() =>
      page.locator("#sim-canvas").evaluate((canvas) =>
        Number((canvas as HTMLCanvasElement).dataset.cameraZoom),
      ),
    )
    .toBeGreaterThan(1);
  await expect(page.locator("#zoom-slider")).toHaveCount(0);
});

test("display mode stack is left and top-aligned against render sliders", async ({
  page,
}) => {
  const layout = await page.locator(".display-settings-row").evaluate((row) => {
    const displayStack = row.querySelector(".stacked-display-controls");
    const renderStack = row.querySelector(".stacked-render-sliders");
    const displaySelect = row.querySelector("#display-mode-select");
    const visualToggle = row.querySelector("#visual-progress-toggle");
    const trackSlider = row.querySelector("#track-opacity-slider");
    if (
      !displayStack ||
      !renderStack ||
      !displaySelect ||
      !visualToggle ||
      !trackSlider
    ) {
      throw new Error("missing display settings controls");
    }
    const displayRect = displayStack.getBoundingClientRect();
    const renderRect = renderStack.getBoundingClientRect();
    const selectRect = displaySelect.getBoundingClientRect();
    const visualRect = visualToggle.getBoundingClientRect();
    const trackRect = trackSlider.getBoundingClientRect();
    return {
      displayLeftOfRender: displayRect.left < renderRect.left,
      topDelta: Math.abs(displayRect.top - renderRect.top),
      visualBelowDisplay: visualRect.top > selectRect.top,
      trackRightOfDisplay: trackRect.left > selectRect.left,
    };
  });

  expect(layout.displayLeftOfRender).toBe(true);
  expect(layout.topDelta).toBeLessThanOrEqual(2);
  expect(layout.visualBelowDisplay).toBe(true);
  expect(layout.trackRightOfDisplay).toBe(true);
});

test("color labels do not open broad clickable color targets", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await page.locator("#anchor-a-label").click();
  await expect
    .poll(() =>
      page.evaluate(() => {
        return document.activeElement?.id ?? "";
      }),
    )
    .not.toBe("anchor-a-input");

  await page.locator("#anchor-a-input").click({ force: true });
  await expect(page.locator("#anchor-a-input")).toHaveAttribute(
    "aria-labelledby",
    "anchor-a-label",
  );
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

  await page.fill("#radius-input", "10");
  await page.click("#refresh-button");
  await expect(page.locator("#status-line")).toContainText("Paused");
  await page.click("#step-button");
  await expect(page.locator("#status-line")).toContainText("1 placements");
  const statusBeforeOverlay = await page.locator("#status-line").textContent();
  await page.evaluate(() => {
    const slider = document.querySelector<HTMLInputElement>(
      "#attack-overlay-opacity-slider",
    );
    if (!slider) throw new Error("missing attack overlay slider");
    slider.value = "80";
    slider.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await expect(page.locator("#attack-overlay-opacity-output")).toHaveText("80%");
  await expect(page.locator("#status-line")).not.toContainText("0 placements");
  expect(await page.locator("#status-line").textContent()).toBe(statusBeforeOverlay);
  await expect
    .poll(() =>
      page.locator("#sim-canvas").evaluate((canvas) => {
        return Number(canvas.getAttribute("data-attack-spots") ?? "0");
      }),
    )
    .toBeGreaterThan(0);
  const attackSpotCount = await page.locator("#sim-canvas").evaluate((canvas) => {
    return Number(canvas.getAttribute("data-attack-spots") ?? "0");
  });
  await page.evaluate(() => {
    const slider = document.querySelector<HTMLInputElement>(
      "#attack-overlay-opacity-slider",
    );
    if (!slider) throw new Error("missing attack overlay slider");
    slider.value = "0";
    slider.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await expect(page.locator("#attack-overlay-opacity-output")).toHaveText("Off");
  expect(
    await page.locator("#sim-canvas").evaluate((canvas) => {
      return Number(canvas.getAttribute("data-attack-spots") ?? "0");
    }),
  ).toBe(attackSpotCount);

  const download = page.waitForEvent("download");
  await page.click("#download-jpeg-button");
  expect((await download).suggestedFilename()).toContain("image-half");
});

test("proactive attack spots initialize while a prime run is active", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await page.fill("#radius-input", "80");
  await page.selectOption("#army-preset-select", "PrimeKnight");
  await page.check("#attacking-toggle");
  await page.click("#refresh-button");
  await expect(page.locator("#status-line")).toContainText("Paused");
  await page.click("#start-button");
  await expect
    .poll(() => statusPlacementCount(page), { timeout: 15_000 })
    .toBeGreaterThan(8);

  const before = await statusPlacementCount(page);
  await page.evaluate(() => {
    const slider = document.querySelector<HTMLInputElement>(
      "#attack-overlay-opacity-slider",
    );
    if (!slider) throw new Error("missing attack overlay slider");
    slider.value = "75";
    slider.dispatchEvent(new Event("input", { bubbles: true }));
  });

  await expect(page.locator("#attack-overlay-opacity-output")).toHaveText("75%");
  await expect(page.locator("#status-line")).not.toContainText("0 placements");
  await expect
    .poll(() =>
      page.locator("#sim-canvas").evaluate((canvas) => {
        return Number(canvas.getAttribute("data-attack-spots") ?? "0");
      }),
      { timeout: 30_000 },
    )
    .toBeGreaterThan(0);
  expect(await statusPlacementCount(page)).toBeGreaterThanOrEqual(before);
});

test("attack spots clear when board or attack settings change", async ({ page }) => {
  await pauseIfRunning(page);
  await page.fill("#radius-input", "12");
  await page.click("#refresh-button");
  await page.click("#step-button");
  await expect(page.locator("#status-line")).toContainText("1 placements");
  await page.evaluate(() => {
    const slider = document.querySelector<HTMLInputElement>(
      "#attack-overlay-opacity-slider",
    );
    if (!slider) throw new Error("missing attack overlay slider");
    slider.value = "70";
    slider.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await expect
    .poll(() =>
      page.locator("#sim-canvas").evaluate((canvas) => {
        return Number(canvas.getAttribute("data-attack-spots") ?? "0");
      }),
    )
    .toBeGreaterThan(0);

  await page.selectOption("#board-select", "LatticeHex");
  await expect
    .poll(() =>
      page.locator("#sim-canvas").evaluate((canvas) => {
        return Number(canvas.getAttribute("data-attack-spots") ?? "0");
      }),
    )
    .toBe(0);

  await page.click("#step-button");
  await expect(page.locator("#status-line")).toContainText("1 placements");
  await expect
    .poll(() =>
      page.locator("#sim-canvas").evaluate((canvas) => {
        return Number(canvas.getAttribute("data-attack-spots") ?? "0");
      }),
    )
    .toBeGreaterThan(0);
  await page.selectOption("#army-preset-select", "PrimeKnight");
  await expect
    .poll(() =>
      page.locator("#sim-canvas").evaluate((canvas) => {
        return Number(canvas.getAttribute("data-attack-spots") ?? "0");
      }),
    )
    .toBe(0);
});

test("full png, regular png, and jpeg export buttons produce downloads", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await page.click("#refresh-button");
  await expect(page.locator("#status-line")).toContainText("Paused");
  await page.click("#step-button");
  await expect(page.locator("#status-line")).toContainText("1 placements", {
    timeout: 15_000,
  });

  const fullPng = page.waitForEvent("download");
  await page.click("#download-full-png-button");
  expect((await fullPng).suggestedFilename()).toContain("image-full");

  const png = page.waitForEvent("download");
  await page.click("#download-png-button");
  const pngName = (await png).suggestedFilename();
  expect(pngName).toContain("image-");
  expect(pngName).toMatch(/\.png$/);

  const jpeg = page.waitForEvent("download");
  await page.click("#download-jpeg-button");
  expect((await jpeg).suggestedFilename()).toContain("image-half");
});

test("failed image export restores the button and later reduced-radius exports work", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await page.click("#refresh-button");
  await expect(page.locator("#status-line")).toContainText("Paused");
  await page.click("#step-button");
  await expect(page.locator("#status-line")).toContainText("1 placements", {
    timeout: 15_000,
  });

  await page.evaluate(() => {
    const original = HTMLCanvasElement.prototype.toBlob;
    (window as typeof window & { __originalToBlob?: typeof original }).__originalToBlob =
      original;
    HTMLCanvasElement.prototype.toBlob = function (callback) {
      window.setTimeout(() => callback(null), 0);
    };
  });
  await page.click("#download-png-button");
  await expect(page.locator("#status-line")).toContainText("Export failed", {
    timeout: 10_000,
  });
  await expect(page.locator("#download-png-button")).toHaveText("PNG");
  await expect(page.locator("#download-png-button")).toBeEnabled();

  await page.fill("#radius-input", "20");
  await page.evaluate(() => {
    const original = (window as typeof window & {
      __originalToBlob?: typeof HTMLCanvasElement.prototype.toBlob;
    }).__originalToBlob;
    if (!original) throw new Error("missing original toBlob");
    HTMLCanvasElement.prototype.toBlob = original;
  });
  const download = page.waitForEvent("download");
  await page.click("#download-png-button");
  expect((await download).suggestedFilename()).toContain("image");
});

test("triangle board, refresh, collapse, pan, and wheel zoom stay wired", async ({
  page,
}) => {
  await pauseIfRunning(page);
  if (!((await page.locator("#placement-log").textContent()) ?? "").includes("coord=square(")) {
    await page.click("#step-button");
    await expect(page.locator("#placement-log")).toContainText("coord=square(", {
      timeout: 15_000,
    });
  }
  await expect(page.locator("#sim-canvas")).toHaveAttribute(
    "data-piece-shape",
    "Square",
  );

  await page.selectOption("#board-select", "LatticeTriangle");
  await expect(page.locator("#shape-select")).toHaveValue("Triangle");
  await expect(page.locator("#sim-canvas")).toHaveAttribute(
    "data-piece-shape",
    "Square",
  );
  await expect(page.locator("#placement-search-select")).toHaveValue("SpiralPath");
  await expect(page.locator("#placement-log")).toContainText("board=LatticeSquare");
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
  await expect(page.locator("#placement-log")).not.toContainText("coord=square(");
  await expect(page.locator("#sim-canvas")).toHaveAttribute(
    "data-piece-shape",
    "Triangle",
  );

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
  await expect(page.locator("#display-mode-select")).toHaveValue("PixelOneToOne");
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
  await page.selectOption("#army-preset-select", "PrimeGapper");
  await page.uncheck("#visual-progress-toggle");
  await page.click("#start-button");
  await expect(page.locator("#status-line")).toContainText("Running silently");

  await page.click("#refresh-button");
  await expect(page.locator("#status-line")).toContainText("Paused", {
    timeout: 10_000,
  });
  await expect(page.locator("#army-preset-select")).toHaveValue("PrimeGapper");
  await expect(page.locator("#visual-progress-toggle")).not.toBeChecked();

  await page.selectOption("#board-select", "LatticeHex");
  await expect(page.locator("#shape-select")).toHaveValue("Hex");
  await page.click("#step-button");
  await expect(page.locator("#status-line")).toContainText("1 placements", {
    timeout: 15_000,
  });
});

test("refresh then immediate start uses the selected board worker", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await page.selectOption("#board-select", "LatticeTriangle");
  await page.click("#refresh-button");
  await page.click("#start-button");

  await expect(page.locator("#placement-log")).toContainText(
    "board=LatticeTriangle",
    { timeout: 15_000 },
  );
  await expect(page.locator("#placement-log")).toContainText("coord=triangle(");
  await expect(page.locator("#placement-log")).not.toContainText("coord=square(");
});

test("track-on board switch under load refreshes the selected board worker", async ({
  page,
}) => {
  await page.evaluate(() => {
    const slider = document.querySelector<HTMLInputElement>(
      "#track-opacity-slider",
    );
    if (!slider) throw new Error("missing track opacity slider");
    slider.value = "70";
    slider.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await expect(page.locator("#track-opacity-output")).toHaveText("70%");
  await expect(page.locator("#status-line")).toContainText("placements", {
    timeout: 15_000,
  });

  await page.selectOption("#board-select", "ContinuousArchimedean");
  await page.fill("#radius-input", "1000");
  await page.click("#refresh-button");
  await page.click("#start-button");

  await expect(page.locator("#board-select")).toHaveValue(
    "ContinuousArchimedean",
  );
  await expect(page.locator("#radius-input")).toHaveValue("1000");
  await expect(page.locator("#track-opacity-output")).toHaveText("70%");
  await expect(page.locator("#placement-log")).toContainText(
    "board=ContinuousArchimedean",
    { timeout: 15_000 },
  );
  await expect(page.locator("#placement-log")).toContainText("radius=1000.00");
  await expect(page.locator("#placement-log")).toContainText(
    "coord=continuous(",
  );
  await expect(page.locator("#placement-log")).not.toContainText(
    "coord=square(",
  );
  await pauseIfRunning(page);
});

test("starting a staged board switch clears stale square data", async ({
  page,
}) => {
  await pauseIfRunning(page);
  if (!((await page.locator("#placement-log").textContent()) ?? "").includes("coord=square(")) {
    await page.click("#step-button");
    await expect(page.locator("#placement-log")).toContainText("coord=square(", {
      timeout: 15_000,
    });
  }

  await page.selectOption("#board-select", "LatticeTriangle");
  await page.click("#start-button");

  await expect(page.locator("#placement-log")).toContainText(
    "board=LatticeTriangle",
    { timeout: 15_000 },
  );
  await expect(page.locator("#placement-log")).toContainText("coord=triangle(");
  await expect(page.locator("#placement-log")).not.toContainText("coord=square(");
});

test("continuous offset edits dim and preserve the current snapshot until restart", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await page.selectOption("#board-select", "ContinuousArchimedean");
  await page.fill("#radius-input", "12");
  await page.click("#refresh-button");
  await page.click("#step-button");
  await expect(page.locator("#placement-log")).toContainText(
    "board=ContinuousArchimedean",
    { timeout: 15_000 },
  );
  await expect(page.locator("#placement-log")).toContainText("coord=continuous(");
  await expect(page.locator("#sim-canvas")).toHaveAttribute(
    "data-color-saturation",
    "normal",
  );
  const pixelsBefore = await renderedPixelCount(page);
  expect(pixelsBefore).toBeGreaterThan(0);

  await page.fill("#continuous-offset-input", "bad");
  await expect(page.locator("#continuous-offset-input")).toHaveClass(
    /invalid-input/,
  );
  await expect(page.locator("#sim-canvas")).toHaveAttribute(
    "data-color-saturation",
    "normal",
  );
  await expect(page.locator("#placement-log")).toContainText("coord=continuous(");
  expect(await renderedPixelCount(page)).toBeGreaterThan(0);

  await page.fill("#continuous-offset-input", "0.25");
  await expect(page.locator("#sim-canvas")).toHaveAttribute(
    "data-color-saturation",
    "dimmed",
  );
  await expect(page.locator("#placement-log")).toContainText("coord=continuous(");
  expect(await renderedPixelCount(page)).toBeGreaterThan(0);

  await page.click("#start-button");
  await expect(page.locator("#sim-canvas")).toHaveAttribute(
    "data-color-saturation",
    "normal",
    { timeout: 15_000 },
  );
  await expect(page.locator("#placement-log")).toContainText("offset=0.250");
  await pauseIfRunning(page);
});

test("reselecting the current board refreshes without changing board", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await expect(page.locator("#board-select")).toHaveValue("LatticeSquare");
  await expect(page.locator("#placement-log")).toContainText("placements logged:");

  await page.locator("#board-select").evaluate((element) => {
    element.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    element.dispatchEvent(new FocusEvent("blur", { bubbles: false }));
  });

  await expect(page.locator("#board-select")).toHaveValue("LatticeSquare");
  await expect(page.locator("#status-line")).toContainText("Paused");
  await expect(page.locator("#placement-log")).toContainText("No placements yet.");
});

test("board-select blur into another control does not refresh the board", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await page.selectOption("#board-select", "LatticeTriangle");
  await page.click("#step-button");
  await expect(page.locator("#placement-log")).toContainText("coord=triangle(", {
    timeout: 15_000,
  });

  await page.locator("#board-select").evaluate((element) => {
    const relatedTarget = document.querySelector("#enemy-mode-select");
    element.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    element.dispatchEvent(
      new FocusEvent("blur", { bubbles: false, relatedTarget }),
    );
  });
  await page.waitForTimeout(100);

  await expect(page.locator("#board-select")).toHaveValue("LatticeTriangle");
  await expect(page.locator("#placement-log")).toContainText("coord=triangle(");
  await expect(page.locator("#placement-log")).not.toContainText(
    "No placements yet.",
  );

  await page.locator("#board-select").evaluate((element) => {
    const enemy = document.querySelector<HTMLElement>("#enemy-mode-select");
    if (!enemy) throw new Error("missing enemy-mode-select");
    element.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    enemy.focus();
    element.dispatchEvent(new FocusEvent("blur", { bubbles: false }));
  });
  await page.waitForTimeout(100);

  await expect(page.locator("#board-select")).toHaveValue("LatticeTriangle");
  await expect(page.locator("#placement-log")).toContainText("coord=triangle(");
  await expect(page.locator("#placement-log")).not.toContainText(
    "No placements yet.",
  );
});

test("refresh then start uses visible board and radius controls for every board", async ({
  page,
}) => {
  for (const scenario of [
    {
      board: "LatticeHex",
      radius: "60",
      shape: "Hex",
      coord: "coord=hex(",
    },
    {
      board: "LatticeTriangle",
      radius: "60",
      shape: "Triangle",
      coord: "coord=triangle(",
    },
    {
      board: "ContinuousArchimedean",
      radius: "1000",
      shape: "Circle",
      coord: "coord=continuous(",
    },
  ]) {
    await page.reload();
    await expect(page.locator("#status-line")).toContainText("placements", {
      timeout: 15_000,
    });
    await pauseIfRunning(page);

    await page.evaluate(({ board, radius }) => {
      const boardSelect =
        document.querySelector<HTMLSelectElement>("#board-select");
      const radiusInput =
        document.querySelector<HTMLInputElement>("#radius-input");
      const trackSlider = document.querySelector<HTMLInputElement>(
        "#track-opacity-slider",
      );
      if (!boardSelect || !radiusInput || !trackSlider) {
        throw new Error("missing controls");
      }
      boardSelect.value = board;
      radiusInput.value = radius;
      trackSlider.value = "65";
    }, scenario);

    await page.click("#refresh-button");
    await expect(page.locator("#board-select")).toHaveValue(scenario.board);
    await expect(page.locator("#radius-input")).toHaveValue(scenario.radius);
    await expect(page.locator("#shape-select")).toHaveValue(scenario.shape);
    await expect(page.locator("#track-opacity-output")).toHaveText("65%");
    await expect(page.locator("#status-line")).toContainText("Paused");

    await page.click("#start-button");
    await expect(page.locator("#placement-log")).toContainText(
      `board=${scenario.board}`,
      { timeout: 15_000 },
    );
    await expect(page.locator("#placement-log")).toContainText(
      `radius=${Number(scenario.radius).toFixed(2)}`,
    );
    await expect(page.locator("#placement-log")).toContainText(scenario.coord);
    if (scenario.board !== "LatticeSquare") {
      await expect(page.locator("#placement-log")).not.toContainText(
        "coord=square(",
      );
    }
    await pauseIfRunning(page);
  }
});

test("placement search and continuous offset validation stay wired", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await page.selectOption("#army-preset-select", "PrimeKnight");
  await page.fill("#prime-divisor-input", "10");
  await expect(page.locator("#prime-divisor-input")).toHaveValue("10");
  await page.locator("#prime-divisor-input").blur();
  await expect(page.locator("#prime-divisor-input")).toHaveValue("12");

  await page.fill("#prime-divisor-input", "1");
  await expect(page.locator("#prime-divisor-input")).toHaveValue("1");
  await page.locator("#prime-divisor-input").blur();
  await expect(page.locator("#prime-divisor-input")).toHaveValue("6");

  await page.fill("#prime-divisor-input", "10");
  await expect(page.locator("#prime-divisor-input")).toHaveValue("10");
  await page.press("#prime-divisor-input", "Enter");
  await expect(page.locator("#prime-divisor-input")).toHaveValue("12");

  await page.selectOption("#placement-search-select", "CenterDistance");
  await page.click("#step-button");
  await expect(page.locator("#placement-log")).toContainText(
    "search=CenterDistance",
  );

  await page.selectOption("#board-select", "ContinuousArchimedean");
  await page.fill("#continuous-offset-input", "1.0000000000001");
  await expect(page.locator("#continuous-offset-input")).toHaveClass(
    /invalid-input/,
  );
  await expect(
    page.locator("#continuous-offset-highlight .invalid-char"),
  ).not.toHaveCount(0);
  await page.fill("#continuous-offset-input", "0.1234567890123");
  await expect(page.locator("#continuous-offset-input")).toHaveClass(
    /invalid-input/,
  );
  await expect(
    page.locator("#continuous-offset-highlight .invalid-char"),
  ).toHaveCount(1);
  await expect(page.locator("#continuous-offset-highlight .valid-char")).not.toHaveCount(
    0,
  );
  await page.fill("#continuous-offset-input", "0.123456789012");
  await expect(page.locator("#continuous-offset-input")).not.toHaveClass(
    /invalid-input/,
  );

  await page.fill("#continuous-offset-input", "");
  await expect(page.locator("#continuous-offset-input")).toHaveClass(
    /invalid-input/,
  );
  await page.locator("#status-line").click();
  await expect(page.locator("#continuous-offset-input")).toHaveValue("0");
  await expect(page.locator("#continuous-offset-input")).not.toHaveClass(
    /invalid-input/,
  );
});

test("high-radius center distance starts without a full-radius prebuild", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await page.evaluate(() => {
    const slider = document.querySelector<HTMLInputElement>(
      "#track-opacity-slider",
    );
    if (!slider) throw new Error("missing track opacity slider");
    slider.value = "0";
    slider.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await page.fill("#radius-input", "1500");
  await page.selectOption("#placement-search-select", "CenterDistance");
  await page.click("#step-button");

  await expect(page.locator("#status-line")).toContainText("1 placements", {
    timeout: 10_000,
  });
  await expect(page.locator("#placement-log")).toContainText(
    "search=CenterDistance",
  );
  await expect(page.locator("#placement-log")).toContainText("coord=square(0,0)");
});

test("higher radius commits without clearing the visible generation", async ({
  page,
}) => {
  await expect
    .poll(() => statusPlacementCount(page), { timeout: 15_000 })
    .toBeGreaterThan(0);

  const beforeLog = await page.locator("#placement-log").textContent();
  expect(beforeLog).toContain("placements logged:");

  await page.fill("#radius-input", "240");
  await expect(page.locator("#placement-log")).toContainText("placements logged:");
  await page.waitForTimeout(2_300);
  await expect(page.locator("#radius-input")).toHaveValue("240");
  await expect(page.locator("#placement-log")).toContainText("placements logged:");
  await expect(page.locator("#status-line")).not.toContainText("0 placements");
});

test("image export button enters cancel state and can cancel", async ({
  page,
}) => {
  await pauseIfRunning(page);
  await page.fill("#radius-input", "1000");

  await page.click("#download-full-png-button");
  await expect(page.locator("#download-full-png-button")).toHaveText("Cancel", {
    timeout: 5_000,
  });
  await page.click("#download-full-png-button");
  await expect(page.locator("#status-line")).toContainText("Export canceled", {
    timeout: 10_000,
  });
  await expect(page.locator("#download-full-png-button")).toHaveText("Full PNG");
});
