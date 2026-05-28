import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";

import { validateSpec } from "../../../myIO/mcp/lib/validate.mjs";

const require = createRequire(import.meta.url);
const { ioneToMyio, validationSpecs } = require("../js/chart_adapter.js");

test("line chart pivots wide series to long rows with numeric x", () => {
  const config = ioneToMyio(
    {
      chart_type: "line",
      x_axis: "bucket_start",
      y_axis: "Magnitude",
      series: ["mean", "p95"]
    },
    [
      { bucket_start: "2026-05-27T00:00:00Z", bucket_start_ms: 1780012800000, mean: 4.2, p95: 6.1 },
      { bucket_start: "2026-05-28T00:00:00Z", bucket_start_ms: 1780099200000, mean: 4.4, p95: 6.3 }
    ]
  );

  assert.equal(config.layers.length, 1);
  assert.deepEqual(config.layers[0].mapping, { x_var: "x", y_var: "y", group: "group" });
  assert.equal(config.layers[0].data.length, 4);
  assert.deepEqual([...new Set(config.layers[0].data.map((row) => row.group))].sort(), ["mean", "p95"]);
  assert.equal(typeof config.layers[0].data[0].x, "number");

  for (const spec of validationSpecs(config)) {
    assert.deepEqual(validateSpec(spec), { valid: true, errors: [] });
  }
});

test("histogram chart maps single value column", () => {
  const config = ioneToMyio(
    {
      chart_type: "histogram",
      x_axis: "mag",
      y_axis: "Magnitude",
      series: ["mag"]
    },
    [{ mag: 4.1 }, { mag: 4.8 }, { mag: 5.0 }]
  );

  assert.equal(config.layers.length, 1);
  assert.equal(config.layers[0].type, "histogram");
  assert.deepEqual(config.layers[0].mapping, { value: "value" });

  for (const spec of validationSpecs(config)) {
    assert.deepEqual(validateSpec(spec), { valid: true, errors: [] });
  }
});
