(function(root, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) {
    module.exports = api;
  }
  root.IoneChartAdapter = api;
})(typeof globalThis !== "undefined" ? globalThis : this, function() {
  const TYPE_MAP = {
    line: "line",
    bar: "bar",
    scatter: "point",
    point: "point",
    histogram: "histogram",
    gauge: "gauge",
    qq: "qq",
    area: "line"
  };

  const NUMERIC_SERIES = new Set([
    "value",
    "avg",
    "min",
    "max",
    "sum",
    "percentile_value",
    "trailing_30d_avg",
    "event_count",
    "valid_count"
  ]);

  function ioneToMyio(spec, rows) {
    const normalizedSpec = normalizeSpec(spec);
    const normalizedRows = Array.isArray(rows) ? rows : [];
    const type = TYPE_MAP[normalizedSpec.chart_type] || normalizedSpec.chart_type || "line";

    if (type === "histogram") {
      return histogramConfig(normalizedSpec, normalizedRows);
    }
    if (type === "gauge") {
      return gaugeConfig(normalizedSpec, normalizedRows);
    }
    if (normalizedSpec.chart_type === "group_by" || hasGroupRows(normalizedRows)) {
      return groupBarConfig(normalizedSpec, normalizedRows);
    }

    return timeseriesConfig(type, normalizedSpec, normalizedRows);
  }

  function normalizeSpec(spec) {
    const src = spec || {};
    const series = Array.isArray(src.series) && src.series.length > 0
      ? src.series.map(String)
      : [String(src.y_axis || "value")];
    return {
      chart_type: String(src.chart_type || src.chartType || "line"),
      x_axis: String(src.x_axis || src.xAxis || "bucket_start_ms"),
      y_axis: String(src.y_axis || src.yAxis || series[0] || "value"),
      series
    };
  }

  function timeseriesConfig(type, spec, rows) {
    const xKey = rows.some((row) => typeof row.bucket_start_ms === "number")
      ? "bucket_start_ms"
      : (rows.some((row) => typeof row.bucketStartMs === "number") ? "bucketStartMs" : spec.x_axis);
    const series = spec.series;
    const data = [];

    rows.forEach((row) => {
      series.forEach((seriesName) => {
        if (row[seriesName] == null) return;
        const value = Number(row[seriesName]);
        const x = Number(row[xKey]);
        if (!Number.isFinite(value) || !Number.isFinite(x)) return;
        data.push({
          x: x,
          y: value,
          group: seriesName,
          bucket_start: row.bucket_start || row.bucketStart || null
        });
      });
    });

    return makeConfig([makeLayer(type, spec.y_axis || "Series", data, {
      x_var: "x",
      y_var: "y",
      group: "group"
    })], {
      x: "numeric",
      y: "numeric",
      group: "string"
    });
  }

  function groupBarConfig(spec, rows) {
    const data = rows
      .map((row) => ({
        group_key: String(row.group_key || row.groupKey || "Unknown"),
        event_count: Number(row.event_count ?? row.eventCount ?? 0)
      }))
      .filter((row) => Number.isFinite(row.event_count));

    return makeConfig([makeLayer("bar", spec.y_axis || "Count", data, {
      x_var: "group_key",
      y_var: "event_count"
    })], {
      group_key: "string",
      event_count: "numeric"
    }, {
      scales: {
        categoricalScale: { xAxis: true, yAxis: false }
      }
    });
  }

  function histogramConfig(spec, rows) {
    const valueKey = spec.series[0] || spec.y_axis || "value";
    const data = rows
      .map((row) => ({ value: Number(row[valueKey]) }))
      .filter((row) => Number.isFinite(row.value));
    return makeConfig([makeLayer("histogram", spec.y_axis || "Distribution", data, {
      value: "value"
    })], {
      value: "numeric"
    });
  }

  function gaugeConfig(spec, rows) {
    const valueKey = spec.series[0] || spec.y_axis || "value";
    const first = rows.find((row) => row[valueKey] != null) || {};
    const value = Number(first[valueKey]);
    return makeConfig([makeLayer("gauge", spec.y_axis || "Gauge", [{
      value: Number.isFinite(value) ? value : 0
    }], {
      value: "value"
    })], {
      value: "numeric"
    }, {
      layout: {
        suppressLegend: true,
        suppressAxis: { xAxis: true, yAxis: true }
      }
    });
  }

  function hasGroupRows(rows) {
    return rows.some((row) => row && (row.group_key != null || row.groupKey != null));
  }

  function makeLayer(type, label, data, mapping) {
    return {
      id: `layer_${type}_${Math.random().toString(36).slice(2, 8)}`,
      type,
      label,
      data,
      mapping,
      options: {},
      transform: "identity",
      transformMeta: {},
      encoding: {},
      sourceKey: "_source_key",
      derivedFrom: null,
      order: 1,
      visibility: true,
      color: "#4E79A7"
    };
  }

  function makeConfig(layers, columns, overrides) {
    const base = {
      specVersion: 1,
      layers,
      columns,
      layout: {
        margin: { top: 30, bottom: 60, left: 56, right: 16 },
        suppressLegend: false,
        suppressAxis: { xAxis: false, yAxis: false }
      },
      scales: {
        xlim: { min: null, max: null },
        ylim: { min: null, max: null },
        categoricalScale: { xAxis: false, yAxis: false },
        flipAxis: false,
        colorScheme: { colors: ["#4E79A7", "#F28E2B", "#59A14F", "#E15759"], domain: ["none"], enabled: false }
      },
      axes: {
        xAxisFormat: "s",
        yAxisFormat: "s",
        xAxisLabel: null,
        yAxisLabel: null,
        toolTipFormat: "s"
      },
      interactions: {
        dragPoints: false,
        toggleY: { variable: null, format: null },
        toolTipOptions: { suppressY: false }
      },
      theme: {},
      transitions: { speed: 0 },
      referenceLines: { x: null, y: null }
    };
    return deepMerge(base, overrides || {});
  }

  function deepMerge(target, source) {
    Object.keys(source).forEach((key) => {
      const value = source[key];
      if (value && typeof value === "object" && !Array.isArray(value)) {
        target[key] = deepMerge(target[key] || {}, value);
      } else {
        target[key] = value;
      }
    });
    return target;
  }

  function validationSpecs(config) {
    return (config.layers || []).map((layer) => ({
      type: layer.type,
      mapping: layer.mapping,
      transform: layer.transform || "identity",
      columns: config.columns || inferColumns(layer.data)
    }));
  }

  function inferColumns(rows) {
    const columns = {};
    (rows || []).forEach((row) => {
      Object.entries(row || {}).forEach(([key, value]) => {
        if (columns[key]) return;
        columns[key] = typeof value === "number" ? "numeric" : "string";
      });
    });
    NUMERIC_SERIES.forEach((key) => {
      if (columns[key]) columns[key] = "numeric";
    });
    return columns;
  }

  return {
    ioneToMyio,
    validationSpecs
  };
});
