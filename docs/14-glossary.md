# 14 — Glossary

Canonical vocabulary. Use these terms in code identifiers, docs and UI strings.

| Term | Definition |
|---|---|
| **Anchor** | A reference from a dimension/text to a vertex, edge midpoint or point; re-resolved on read. |
| **Attribute dictionary** | Named key/value store attached to any entity or the document; namespaced (`plyr.*`, `sheets.*`, `ext.<id>.*`). |
| **Auto-fold** | Automatic insertion of hidden soft edges that keep faces planar when vertices move out of plane. |
| **Axis lock** | Constraining a tool's input direction to a drawing axis or inferred line (arrow keys / Shift). |
| **Batch** | A text file of CLI command lines executed as one undo group and one save. |
| **Cap** | The face that closes a section cut on a solid; also the end face created by push/pull. |
| **Command** | A named, serde-typed, undoable document mutation; the only mutation path (`docs/01 §3`). |
| **Component** | A reusable definition instantiated one or more times; edits to the definition affect all instances. |
| **Component definition** | The entities, behavior and metadata of a component (has a UUID). |
| **Component instance** | A placement (transform) of a definition inside a container. |
| **Container** | An `Entities` collection: the model root or a definition's contents. |
| **Context (edit context)** | The container currently open for editing in a frontend; entities outside it are faded/not selectable. |
| **Curve descriptor** | Metadata grouping consecutive edges into an arc/circle/polygon/freehand curve. |
| **Delta** | A recorded elementary change with its inverse, grouped into transactions for undo. |
| **Dock** | A side area holding dockable panels (tiles). |
| **Edge** | A straight segment between two vertices; may border 0, 1 or 2 faces. |
| **Entity** | Any addressable thing in a container: edge, face, group, instance, guide, dimension, text, image, section plane. |
| **Face** | A planar polygon with one outer loop and zero or more holes; has a front and a back. |
| **Face finding** | Detecting closed coplanar edge loops and creating faces automatically (`docs/03 §4.3`). |
| **Front / back** | The two sides of a face; the front is along the normal and gets the front material. |
| **Glue** | Component behavior that attaches an instance to a face and slides along it. |
| **Group** | A single-instance, anonymous component used to isolate geometry. |
| **Guide** | A construction line or point used for inference; not geometry. |
| **Half-edge** | A directed edge record; each edge has two twins (`docs/03 §3`). |
| **Hidden geometry** | Entities flagged hidden; shown dashed when "show hidden geometry" is on. |
| **Inference** | A snapped point/direction derived from existing geometry or axes, with a kind and label. |
| **Instance path** | The chain of instances from the model root to an occurrence of an entity. |
| **Material** | Color/opacity/texture applied to face sides or as an instance override. |
| **Measure (value field)** | The status-bar numeric input that accepts lengths, angles, counts and coordinates during tools. |
| **Merge** | Combining two coplanar faces when their shared edge is erased, or two vertices within tolerance. |
| **Model** | The root container of a document. |
| **Pid** | Persistent id of an entity within a document, stable across save/load. |
| **Plane constraint** | The plane onto which a tool projects free cursor positions. |
| **Profile edge** | A silhouette edge for the current view, drawn thicker by styles. |
| **Push/pull** | Extruding a face along its normal, merging with coplanar neighbors and cutting through when landing on a coplanar face. |
| **Query** | A selection expression (`docs/08 §5`). |
| **Scene** | A saved set of view properties (camera, tag visibility, hidden entities, sections, style, shadows). |
| **Section plane** | A cutting plane that hides geometry on one side and draws cut lines/caps. |
| **Sheet** | A paper-space page in the 2D documentation module (`docs/15`). |
| **Smooth / soft edge** | Flags: smooth hides the edge line between visible faces; soft averages shading normals across it. |
| **Solid** | A container whose mesh is a closed, consistently oriented 2-manifold; required for solid tools. |
| **Style** | Named rendering settings (edges, faces, background, watermarks, modeling colors). |
| **Sweep** | Extruding a profile face along a path of edges. |
| **Tag** | A visibility/style category assigned to entities; not ownership. |
| **Tolerance** | Document-level thresholds for point merging, angle equality and planarity (`docs/03 §2`). |
| **Transaction** | A set of deltas applied atomically and undone as a unit. |
| **Viewport** | A tile rendering the model with its own camera and style. |
| **Wire edge** | An edge bordering no face. |
