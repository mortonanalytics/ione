# Requirements — Event View Schema (Domain-Agnostic Detail Rendering)

**Source:** `md/intake/tri-thrust-advance-2026-06-21.md` (TT-B01)
**Status:** code-complete on `feat/tri-thrust-federation-hardening` (Partial — pending founder walkthrough)

The thin UX shell must render event detail without assuming any domain schema. The map
detail panel previously hardcoded USGS earthquake field names (magnitude, depth, PAGER
alert, place, USGS url). This couples the substrate to one domain and violates the
"pluggable view types" layer (`.claude/rules/path-2-stream-p.md`).

## Contract

`GET /api/v1/workspaces/:id/event-layers` — each `EventLayer` carries:

| Field | Type | Meaning |
|---|---|---|
| `propertyFields` | `string[]` | Ordered, operator-declared property field names present on every feature's `properties`. Derived from `view_config.property_fields[].name`. |

Each feature's `properties` already contains exactly those named fields (resolved from the
configured JSON pointers) plus the internal `_event_id` / `_observed_at` keys.

## Rendering rules (frontend)

- The detail panel (`openEventPopup`, `static/app.js`) renders **only** the fields named in
  `layer.propertyFields`, in declared order, reading values from `feature.properties`.
- A value matching `^https?://` renders as a safe external link (`rel="noopener noreferrer"`);
  all other values render as escaped text. No domain-specific field handling (no magnitude,
  depth, or PAGER chip logic).
- Title comes from `eventFeatureLabel` (operator `style.labelField`, else first non-internal
  property, else `_event_id` / stream name).
- Fallback: when a layer carries no `propertyFields` manifest, render all non-internal
  (`_`-prefixed excluded) properties so older payloads still display.

## Out of scope

- Per-field display labels and format hints (`PropertyFormat`) — deferred; operators set a
  friendly `name` today. Revisit if a field needs a label distinct from its key.
- Dead `pager-chip` CSS in `static/style.css` is now unused; left in place (harmless) for a
  separate cleanup pass.
</content>
