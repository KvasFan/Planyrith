# Planyrith specification

These documents are the source of truth for building Planyrith. They are written for AI agents
and engineers who will implement the software; read them in numeric order the first time, then
by milestone (`13-roadmap.md`). Working rules for implementers are in the repository root
[`AGENTS.md`](../AGENTS.md).

| # | Document | One-line summary |
|---|---|---|
| 00 | [Overview](00-overview.md) | Product definition, goals, non-goals, personas, feature map, quality targets |
| 01 | [Architecture](01-architecture.md) | Core / CLI / GUI tiers, command system, data flow, concurrency, errors |
| 02 | [Technology stack](02-technology-stack.md) | Approved crates with licenses, alternatives considered, licensing policy |
| 03 | [Geometry kernel](03-geometry-kernel.md) | Half-edge mesh, tolerances, face finding, push/pull, booleans, inference engine |
| 04 | [Document model](04-document-model.md) | Entities, groups/components, materials, tags, scenes, styles, attributes, ids |
| 05 | [Modeling tools](05-modeling-tools.md) | Complete command catalogue: params, interaction, behavior, tests |
| 06 | [Rendering and viewport](06-rendering-and-viewport.md) | wgpu passes, styles, edges, shadows, sections, picking, headless render |
| 07 | [Interaction and UI](07-interaction-and-ui.md) | egui shell, docks, input model, tool lifecycle, panels, menus, web UI |
| 08 | [CLI specification](08-cli-specification.md) | Subcommands, parameter syntax, selection queries, JSON output, JSON-RPC server |
| 09 | [File formats](09-file-formats.md) | `.plyr` container, schema, migrations, importers/exporters |
| 10 | [Extensions API](10-extensions-api.md) | Scripting binding, plugin manifests, forms, sandboxing |
| 11 | [Persistence and web](11-persistence-and-web.md) | Storage trait, autosave/recovery, IndexedDB, wasm build |
| 12 | [Testing and quality](12-testing-and-quality.md) | Test pyramid, fixtures, CI matrix, gates |
| 13 | [Roadmap](13-roadmap.md) | Milestones 0.1 → 1.0 with acceptance criteria |
| 14 | [Glossary](14-glossary.md) | Canonical vocabulary |
| 15 | [Sheets](15-sheets-2d-documentation.md) | 2D documentation module |

Conventions: RFC 2119 keywords; kebab-case command names; millimeters and `f64` internally;
every document ends with a dated **Decisions** list that records resolved ambiguities.
