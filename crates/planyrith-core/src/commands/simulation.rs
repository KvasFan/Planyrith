//! Implements docs/05-modeling-tools.md §8.16–§8.17 (simulation, milestone
//! 1.16): `sim-static`, `sim-frequency`, `sim-thermal` and
//! `motion-study`.
//!
//! A simulation meshes one closed body (the faces of the edit context, or
//! a group or component) with the kernel of docs/03 §12; fixtures, loads
//! and thermal conditions name faces of that body by queries evaluated
//! inside it. Results are shown like analyses (§8.15): a colored copy of
//! the mesh's boundary in a group "Analysis: `kind`", optionally deformed,
//! replaced on the next run and removed by `analysis-clear`.

use std::collections::BTreeMap;

use glam::{DAffine3, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::commands::analysis::{AnalysisClass, AnalysisKind, Measured, show};
use crate::commands::assembly::{MateKind, mates_of, solve};
use crate::commands::common::targets_of;
use crate::commands::params::{Target, VectorSchema, target_container};
use crate::commands::surfaces::face_sets;
use crate::commands::{
    Command, CommandCategory, CommandError, CommandMeta, CommonParams, Transaction,
};
use crate::document::{ContainerRef, Document, EditContext, EntityKey, Query, Rgba8};
use crate::geom::HalfEdgeMesh;
use crate::geom::boolean::{BooleanOp, boolean};
use crate::ids::{EntityId, FaceId, Pid};
use crate::sim::{
    HeatCondition, Load, SimMaterial, TetMesh, material, solve_modes, solve_static, solve_thermal,
    tet_mesh,
};
use crate::units::{Angle, Length};

/// The most nodes a simulation mesh may have.
const MAX_NODES: usize = 60_000;

fn meta(name: &str, label: &str, view_only: bool) -> CommandMeta {
    CommandMeta {
        name: name.into(),
        label: label.into(),
        category: CommandCategory::Solid,
        view_only,
        steps: Vec::new(),
    }
}

/// Eight colors from cool to hot, for results.
const RAMP: [Rgba8; 8] = [
    Rgba8::rgb(49, 54, 149),
    Rgba8::rgb(69, 117, 180),
    Rgba8::rgb(116, 173, 209),
    Rgba8::rgb(171, 217, 233),
    Rgba8::rgb(254, 224, 144),
    Rgba8::rgb(253, 174, 97),
    Rgba8::rgb(244, 109, 67),
    Rgba8::rgb(215, 48, 39),
];

fn ramp(names: [&'static str; 8]) -> [(&'static str, Rgba8); 8] {
    let mut out = [("", RAMP[0]); 8];
    for i in 0..8 {
        out[i] = (names[i], RAMP[i]);
    }
    out
}

const STRESS: [&str; 8] = [
    "stress 1", "stress 2", "stress 3", "stress 4", "stress 5", "stress 6", "stress 7", "stress 8",
];
const MOTION: [&str; 8] = [
    "motion 1", "motion 2", "motion 3", "motion 4", "motion 5", "motion 6", "motion 7", "motion 8",
];
const HEAT: [&str; 8] = [
    "temperature 1",
    "temperature 2",
    "temperature 3",
    "temperature 4",
    "temperature 5",
    "temperature 6",
    "temperature 7",
    "temperature 8",
];

fn steel() -> String {
    "steel".into()
}

/// The material of a simulation: a built-in one, with values overridden.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MaterialParams {
    /// A built-in material (`steel`, `stainless-steel`, `aluminum`,
    /// `titanium`, `copper`, `brass`, `cast-iron`, `abs`, `pla`, `nylon`,
    /// `oak`, `concrete`, `glass`).
    #[serde(default = "steel")]
    pub material: String,
    /// Young's modulus instead, MPa.
    #[serde(default)]
    pub youngs_modulus: Option<f64>,
    /// Poisson's ratio instead.
    #[serde(default)]
    pub poisson_ratio: Option<f64>,
    /// Density instead, kg/m³.
    #[serde(default)]
    pub density: Option<f64>,
    /// Thermal conductivity instead, W/(m·K).
    #[serde(default)]
    pub conductivity: Option<f64>,
    /// Yield strength instead, MPa.
    #[serde(default)]
    pub yield_strength: Option<f64>,
}

impl MaterialParams {
    fn resolve(&self) -> Result<SimMaterial, CommandError> {
        let mut m = material(&self.material).ok_or_else(|| {
            CommandError::invalid_param(
                "material",
                format!(
                    "no material `{}` (one of {})",
                    self.material,
                    crate::sim::MATERIALS.join(", ")
                ),
            )
        })?;
        if let Some(v) = self.youngs_modulus {
            m.youngs_mpa = v;
        }
        if let Some(v) = self.poisson_ratio {
            m.poisson = v;
        }
        if let Some(v) = self.density {
            m.density_kg_m3 = v;
        }
        if let Some(v) = self.conductivity {
            m.conductivity_w_mk = v;
        }
        if let Some(v) = self.yield_strength {
            m.yield_mpa = Some(v);
        }
        m.check()
            .map_err(|e| CommandError::invalid_param("material", e))?;
        Ok(m)
    }
}

/// The body being simulated.
struct Body {
    /// Queries inside it evaluate here.
    inner: Target,
    /// Body to edit-context coordinates.
    to_target: DAffine3,
    mesh: HalfEdgeMesh,
}

/// The one closed body `q` names.
fn body(tx: &Transaction<'_>, target: &Target, q: &Query) -> Result<Body, CommandError> {
    let sets = face_sets(tx, target, "body", q, true)?;
    let [set] = sets.as_slice() else {
        return Err(CommandError::invalid_param(
            "body",
            "name one body: the faces of the edit context, or one group or component",
        ));
    };
    let doc = tx.document();
    let mesh = doc
        .container(set.container)
        .ok_or_else(|| CommandError::Internal("a container vanished".into()))?
        .mesh()
        .clone();
    for (e, _) in mesh.edges() {
        let [a, b] = mesh.edge_faces(e)?;
        if a.is_none() || b.is_none() {
            return Err(CommandError::NotApplicable(
                "the body must be a closed solid (an edge borders only one face)".into(),
            ));
        }
    }
    let inner = match set.entity {
        None => Target {
            context: target.context.clone(),
            container: target.container,
            world: target.world,
            local: target.local,
        },
        Some(id) => {
            let world = target.world * set.to_target;
            Target {
                context: EditContext {
                    path: target.context.path.child(EntityKey::Id(id)),
                },
                container: set.container,
                world,
                local: world.inverse(),
            }
        }
    };
    Ok(Body {
        inner,
        to_target: set.to_target,
        mesh,
    })
}

/// The faces of the body a query names.
fn faces_of(
    tx: &Transaction<'_>,
    b: &Body,
    param: &str,
    q: &Query,
) -> Result<Vec<FaceId>, CommandError> {
    let faces: Vec<FaceId> = targets_of(tx, &b.inner, param, q)?
        .into_iter()
        .filter_map(|id| match id {
            EntityId::Face(f) => Some(f),
            _ => None,
        })
        .collect();
    if faces.is_empty() {
        return Err(CommandError::invalid_param(param, "name faces of the body"));
    }
    Ok(faces)
}

/// The mesh of the body at `size` (or a size giving about `cells` cells).
fn mesh_of(
    tx: &Transaction<'_>,
    b: &Body,
    size: Option<Length>,
    cells: f64,
) -> Result<TetMesh, CommandError> {
    let size = match size {
        Some(s) => s.as_mm(),
        None => {
            let v = b.mesh.volume()?.abs();
            libm::cbrt(v / cells).max(0.1)
        }
    };
    Ok(tet_mesh(&b.mesh, size, MAX_NODES, tx.tolerances())?)
}

/// Shows `values` (one per node) on the mesh's boundary, each boundary
/// triangle drawn as four (through its edge middles) colored by the mean of
/// their corners, moved by `moved` (body coordinates).
#[allow(clippy::too_many_arguments)]
fn present(
    tx: &mut Transaction<'_>,
    target: &Target,
    b: &Body,
    mesh: &TetMesh,
    values: &[f64],
    moved: impl Fn(u32) -> DVec3,
    kind: AnalysisKind,
    names: [&'static str; 8],
) -> Result<(Option<Pid>, Vec<AnalysisClass>, f64, f64), CommandError> {
    let mut on_boundary: Vec<u32> = mesh.boundary.iter().flatten().copied().collect();
    on_boundary.sort_unstable();
    on_boundary.dedup();
    let lo = on_boundary
        .iter()
        .map(|i| values[*i as usize])
        .fold(f64::MAX, f64::min);
    let hi = on_boundary
        .iter()
        .map(|i| values[*i as usize])
        .fold(f64::MIN, f64::max);
    let span = (hi - lo).max(1e-300);
    let t = b.to_target;
    let normal_m = t.matrix3.inverse().transpose();
    let mut measured = Vec::with_capacity(mesh.boundary.len() * 4);
    for tri in &mesh.boundary {
        // Corners a b c, middles ab bc ca.
        let [a, bb, c, ab, bc, ca] = *tri;
        for sub in [[a, ab, ca], [ab, bb, bc], [ca, bc, c], [ab, bc, ca]] {
            let ring: Vec<DVec3> = sub
                .iter()
                .map(|i| t.transform_point3(mesh.nodes[*i as usize] + moved(*i)))
                .collect();
            let normal =
                (normal_m * (ring[1] - ring[0]).cross(ring[2] - ring[0])).normalize_or_zero();
            let v = sub.iter().map(|i| values[*i as usize]).sum::<f64>() / 3.0;
            measured.push(Measured {
                rings: vec![ring],
                normal,
                class: (((v - lo) / span * 8.0).floor() as usize).min(7),
            });
        }
    }
    let out = show(tx, target, kind, measured, &ramp(names), Some((lo, hi)))?;
    Ok((out.group, out.classes, lo, hi))
}

// ----- sim-static -----------------------------------------------------------------------

/// A load on faces of the body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LoadParams {
    /// Faces of the body.
    pub faces: Query,
    /// A total force spread over them, N.
    #[serde(default)]
    #[schemars(with = "Option<VectorSchema>")]
    pub force: Option<DVec3>,
    /// Or a pressure pushing on them, MPa.
    #[serde(default)]
    pub pressure: Option<f64>,
}

/// `sim-static` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SimStaticParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The body: the faces of the edit context, or a group or component.
    pub body: Query,
    /// Its material.
    #[serde(flatten)]
    pub material: MaterialParams,
    /// The mesh's cell size (about 2500 cells by default).
    #[serde(default)]
    pub mesh_size: Option<Length>,
    /// Faces of the body held fixed.
    pub fixed: Query,
    /// Loads.
    #[serde(default)]
    pub loads: Vec<LoadParams>,
    /// Add the body's own weight (along −Z).
    #[serde(default)]
    pub gravity: bool,
    /// Draw the result deformed by this factor (0: as it is).
    #[serde(default)]
    pub deformation_scale: f64,
}

/// What `sim-static` found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SimStaticOutput {
    /// The result's colored copy.
    pub group: Option<Pid>,
    /// Mesh nodes.
    pub nodes: usize,
    /// Mesh tetrahedra.
    pub elements: usize,
    /// The largest von Mises stress, MPa.
    pub max_von_mises_mpa: f64,
    /// The largest displacement, mm.
    pub max_displacement_mm: f64,
    /// Yield strength over the largest stress, when the material has one.
    pub safety_factor: Option<f64>,
    /// The fixtures' total reaction, N.
    pub reaction_n: [f64; 3],
    /// The color classes of the boundary's stress, MPa from `min` to `max`.
    pub classes: Vec<AnalysisClass>,
    /// The lowest stress shown, MPa.
    pub min: f64,
    /// The highest stress shown, MPa.
    pub max: f64,
}

/// The `sim-static` command: linear static stress.
#[derive(Debug, Default, Clone, Copy)]
pub struct SimStatic;

impl Command for SimStatic {
    type Params = SimStaticParams;
    type Output = SimStaticOutput;

    fn name(&self) -> &'static str {
        "sim-static"
    }

    fn validate(&self, _: &Document, p: &SimStaticParams) -> Result<(), CommandError> {
        p.material.resolve()?;
        for l in &p.loads {
            if l.force.is_some() == l.pressure.is_some() {
                return Err(CommandError::invalid_param(
                    "loads",
                    "each load gives a force or a pressure",
                ));
            }
        }
        if p.loads.is_empty() && !p.gravity {
            return Err(CommandError::invalid_param(
                "loads",
                "give a load or gravity",
            ));
        }
        if !p.deformation_scale.is_finite() || p.deformation_scale < 0.0 {
            return Err(CommandError::OutOfRange {
                name: "deformation_scale".into(),
                reason: "must be zero or more".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SimStaticParams,
    ) -> Result<SimStaticOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let m = p.material.resolve()?;
        let b = body(tx, &target, &p.body)?;
        let fixed = faces_of(tx, &b, "fixed", &p.fixed)?;
        let inverse = b.to_target.matrix3.inverse();
        let mut loads = Vec::new();
        for l in &p.loads {
            let faces = faces_of(tx, &b, "loads", &l.faces)?;
            loads.push(match (l.force, l.pressure) {
                (Some(f), _) => Load::Force {
                    faces,
                    force: inverse * (target.local.matrix3 * f),
                },
                (_, Some(mpa)) => Load::Pressure { faces, mpa },
                _ => continue,
            });
        }
        let mesh = mesh_of(tx, &b, p.mesh_size, 2500.0)?;
        let r = solve_static(&mesh, &m, &fixed, &loads, p.gravity)?;
        let scale = p.deformation_scale;
        let disp = &r.displacement;
        let (group, classes, min, max) = present(
            tx,
            &target,
            &b,
            &mesh,
            &r.node_stress,
            |i| disp[i as usize] * scale,
            AnalysisKind::Stress,
            STRESS,
        )?;
        let reaction = target.world.matrix3 * (b.to_target.matrix3 * r.reaction);
        Ok(SimStaticOutput {
            group,
            nodes: mesh.nodes.len(),
            elements: mesh.tets.len(),
            max_von_mises_mpa: r.max_von_mises,
            max_displacement_mm: r.max_displacement,
            safety_factor: m
                .yield_mpa
                .filter(|_| r.max_von_mises > 0.0)
                .map(|y| y / r.max_von_mises),
            reaction_n: reaction.to_array(),
            classes,
            min,
            max,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("sim-static", "Stress Simulation", false)
    }
}

// ----- sim-frequency --------------------------------------------------------------------

fn four() -> u32 {
    4
}

fn one() -> u32 {
    1
}

/// `sim-frequency` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SimFrequencyParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The body.
    pub body: Query,
    /// Its material (it needs a density).
    #[serde(flatten)]
    pub material: MaterialParams,
    /// The mesh's cell size (about 600 cells by default).
    #[serde(default)]
    pub mesh_size: Option<Length>,
    /// Faces of the body held fixed.
    pub fixed: Query,
    /// How many of the lowest frequencies (4, at most 12).
    #[serde(default = "four")]
    pub modes: u32,
    /// The mode whose shape is shown (1 is the lowest).
    #[serde(default = "one")]
    pub show_mode: u32,
    /// How far the shown shape's largest motion goes (a tenth of the
    /// body's size by default).
    #[serde(default)]
    pub amplitude: Option<Length>,
}

/// What `sim-frequency` found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SimFrequencyOutput {
    /// The shown mode's colored copy.
    pub group: Option<Pid>,
    /// Mesh nodes.
    pub nodes: usize,
    /// Mesh tetrahedra.
    pub elements: usize,
    /// Natural frequencies, Hz, lowest first.
    pub frequencies_hz: Vec<f64>,
}

/// The `sim-frequency` command: the lowest natural frequencies and a mode
/// shape.
#[derive(Debug, Default, Clone, Copy)]
pub struct SimFrequency;

impl Command for SimFrequency {
    type Params = SimFrequencyParams;
    type Output = SimFrequencyOutput;

    fn name(&self) -> &'static str {
        "sim-frequency"
    }

    fn validate(&self, _: &Document, p: &SimFrequencyParams) -> Result<(), CommandError> {
        p.material.resolve()?;
        if !(1..=12).contains(&p.modes) || p.show_mode == 0 || p.show_mode > p.modes {
            return Err(CommandError::OutOfRange {
                name: "modes".into(),
                reason: "1 to 12 modes, showing one of them".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SimFrequencyParams,
    ) -> Result<SimFrequencyOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let m = p.material.resolve()?;
        let b = body(tx, &target, &p.body)?;
        let fixed = faces_of(tx, &b, "fixed", &p.fixed)?;
        let mesh = mesh_of(tx, &b, p.mesh_size, 600.0)?;
        let r = solve_modes(&mesh, &m, &fixed, p.modes as usize)?;
        let shape = &r.shapes[p.show_mode as usize - 1];
        let (mut lo, mut hi) = (DVec3::splat(f64::MAX), DVec3::splat(f64::MIN));
        for n in &mesh.nodes {
            lo = lo.min(*n);
            hi = hi.max(*n);
        }
        let amplitude = p.amplitude.map_or((hi - lo).length() * 0.1, |a| a.as_mm());
        let values: Vec<f64> = shape.iter().map(|d| d.length()).collect();
        let (group, _, _, _) = present(
            tx,
            &target,
            &b,
            &mesh,
            &values,
            |i| shape[i as usize] * amplitude,
            AnalysisKind::Mode,
            MOTION,
        )?;
        Ok(SimFrequencyOutput {
            group,
            nodes: mesh.nodes.len(),
            elements: mesh.tets.len(),
            frequencies_hz: r.frequencies_hz,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("sim-frequency", "Frequency Simulation", false)
    }
}

// ----- sim-thermal ----------------------------------------------------------------------

/// A thermal condition on faces of the body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HeatParams {
    /// Faces of the body.
    pub faces: Query,
    /// Held at this temperature, °C.
    #[serde(default)]
    pub temperature: Option<f64>,
    /// Or heated with this power in all, W.
    #[serde(default)]
    pub power: Option<f64>,
}

/// `sim-thermal` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SimThermalParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The body.
    pub body: Query,
    /// Its material.
    #[serde(flatten)]
    pub material: MaterialParams,
    /// The mesh's cell size (about 2500 cells by default).
    #[serde(default)]
    pub mesh_size: Option<Length>,
    /// Temperatures held and heat put in; at least one temperature.
    pub conditions: Vec<HeatParams>,
}

/// What `sim-thermal` found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SimThermalOutput {
    /// The result's colored copy.
    pub group: Option<Pid>,
    /// Mesh nodes.
    pub nodes: usize,
    /// Mesh tetrahedra.
    pub elements: usize,
    /// The lowest temperature, °C.
    pub min_c: f64,
    /// The highest temperature, °C.
    pub max_c: f64,
    /// The color classes.
    pub classes: Vec<AnalysisClass>,
}

/// The `sim-thermal` command: steady heat conduction.
#[derive(Debug, Default, Clone, Copy)]
pub struct SimThermal;

impl Command for SimThermal {
    type Params = SimThermalParams;
    type Output = SimThermalOutput;

    fn name(&self) -> &'static str {
        "sim-thermal"
    }

    fn validate(&self, _: &Document, p: &SimThermalParams) -> Result<(), CommandError> {
        p.material.resolve()?;
        if p.conditions
            .iter()
            .any(|c| c.temperature.is_some() == c.power.is_some())
        {
            return Err(CommandError::invalid_param(
                "conditions",
                "each condition gives a temperature or a power",
            ));
        }
        if !p.conditions.iter().any(|c| c.temperature.is_some()) {
            return Err(CommandError::invalid_param(
                "conditions",
                "hold at least one face at a temperature",
            ));
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: SimThermalParams,
    ) -> Result<SimThermalOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let m = p.material.resolve()?;
        let b = body(tx, &target, &p.body)?;
        let mut conditions = Vec::new();
        for c in &p.conditions {
            let faces = faces_of(tx, &b, "conditions", &c.faces)?;
            conditions.push(match (c.temperature, c.power) {
                (Some(celsius), _) => HeatCondition::Temperature { faces, celsius },
                (_, Some(watts)) => HeatCondition::Power { faces, watts },
                _ => continue,
            });
        }
        let mesh = mesh_of(tx, &b, p.mesh_size, 2500.0)?;
        let r = solve_thermal(&mesh, &m, &conditions)?;
        let (group, classes, _, _) = present(
            tx,
            &target,
            &b,
            &mesh,
            &r.temperature,
            |_| DVec3::ZERO,
            AnalysisKind::Temperature,
            HEAT,
        )?;
        Ok(SimThermalOutput {
            group,
            nodes: mesh.nodes.len(),
            elements: mesh.tets.len(),
            min_c: r.range.0,
            max_c: r.range.1,
            classes,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("sim-thermal", "Thermal Simulation", false)
    }
}

// ----- motion-study ---------------------------------------------------------------------

fn ten() -> u32 {
    10
}

/// `motion-study` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MotionStudyParams {
    /// Common fields.
    #[serde(flatten)]
    pub common: CommonParams,
    /// The driven mate's number (a distance or angle mate).
    pub mate: u32,
    /// The distance it goes to (distance mates).
    #[serde(default)]
    pub to_length: Option<Length>,
    /// The angle it goes to (angle mates).
    #[serde(default)]
    pub to_angle: Option<Angle>,
    /// Steps from its present value (10, at most 200).
    #[serde(default = "ten")]
    pub steps: u32,
    /// Check every step for parts passing into each other.
    #[serde(default)]
    pub interference: bool,
}

/// Where an occurrence is at one step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Placement {
    /// The occurrence.
    pub occurrence: Pid,
    /// Its origin, mm.
    pub origin: [f64; 3],
}

/// One step of a motion study.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MotionFrame {
    /// The driven value (mm, or degrees for an angle).
    pub value: f64,
    /// The occurrences that moved.
    pub placements: Vec<Placement>,
    /// Pairs of occurrences overlapping, with the shared volume, mm³.
    pub collisions: Vec<(Pid, Pid, f64)>,
}

/// What `motion-study` found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MotionStudyOutput {
    /// The steps, from the present value to the end.
    pub frames: Vec<MotionFrame>,
    /// How far each moving occurrence's origin travelled in all, mm.
    pub travel_mm: Vec<(Pid, f64)>,
    /// The first step with a collision.
    pub first_collision: Option<usize>,
}

/// The `motion-study` command: drives one mate of the edit context from
/// its value to another in steps, solving the assembly at each, and
/// reports where the parts go and whether they collide. The document is
/// not changed.
#[derive(Debug, Default, Clone, Copy)]
pub struct MotionStudy;

impl Command for MotionStudy {
    type Params = MotionStudyParams;
    type Output = MotionStudyOutput;

    fn name(&self) -> &'static str {
        "motion-study"
    }

    fn validate(&self, _: &Document, p: &MotionStudyParams) -> Result<(), CommandError> {
        if p.to_length.is_some() == p.to_angle.is_some() {
            return Err(CommandError::invalid_param(
                "to_length",
                "give to_length or to_angle",
            ));
        }
        if !(1..=200).contains(&p.steps) {
            return Err(CommandError::OutOfRange {
                name: "steps".into(),
                reason: "must be 1 to 200".into(),
            });
        }
        Ok(())
    }

    fn apply(
        &self,
        tx: &mut Transaction<'_>,
        p: MotionStudyParams,
    ) -> Result<MotionStudyOutput, CommandError> {
        let target = target_container(tx, &p.common)?;
        let doc = tx.document();
        let tol = *tx.tolerances();
        let mates = mates_of(doc, target.container);
        let index = mates
            .iter()
            .position(|m| m.id == p.mate)
            .ok_or_else(|| CommandError::invalid_param("mate", format!("no mate {}", p.mate)))?;
        let angle = match (mates[index].kind, p.to_length, p.to_angle) {
            (MateKind::Distance, Some(_), _) => false,
            (MateKind::Angle, _, Some(_)) => true,
            _ => {
                return Err(CommandError::invalid_param(
                    "mate",
                    "drive a distance mate with to_length or an angle mate with to_angle",
                ));
            }
        };
        let (from, to) = if angle {
            (
                mates[index].angle_rad.unwrap_or(0.0),
                p.to_angle.map_or(0.0, |a| a.as_radians()),
            )
        } else {
            (
                mates[index].length_mm.unwrap_or(0.0),
                p.to_length.map_or(0.0, |l| l.as_mm()),
            )
        };
        let mut entities = doc
            .container(target.container)
            .ok_or_else(|| CommandError::Internal("edit context vanished".into()))?
            .clone();
        let start: BTreeMap<EntityId, DVec3> = entities
            .iter()
            .filter_map(|id| entities.transform_of(id).map(|t| (id, t.translation)))
            .collect();
        let mut last = start.clone();
        let mut travel: BTreeMap<EntityId, f64> = BTreeMap::new();
        let mut frames = Vec::new();
        let mut first_collision = None;
        let mut list = mates.clone();
        for step in 0..=p.steps {
            let value = from + (to - from) * f64::from(step) / f64::from(p.steps);
            if angle {
                list[index].angle_rad = Some(value);
            } else {
                list[index].length_mm = Some(value);
            }
            let s = solve(doc, &entities, &list).map_err(CommandError::NotApplicable)?;
            for (&id, &t) in &s.placements {
                entities
                    .set_transform(id, t)
                    .map_err(|e| CommandError::Internal(e.to_string()))?;
            }
            let mut placements = Vec::new();
            for &id in s.placements.keys() {
                let (Some(t), Some(pid)) = (entities.transform_of(id), entities.pid_of(id)) else {
                    continue;
                };
                placements.push(Placement {
                    occurrence: pid,
                    origin: t.translation.to_array(),
                });
                let before = last.get(&id).copied().unwrap_or(t.translation);
                *travel.entry(id).or_default() += before.distance(t.translation);
                last.insert(id, t.translation);
            }
            let mut collisions = Vec::new();
            if p.interference {
                let solids: Vec<(EntityId, &HalfEdgeMesh, DAffine3)> = entities
                    .iter()
                    .filter_map(|id| {
                        let d = entities.definition_of(id)?;
                        let t = entities.transform_of(id)?;
                        Some((id, doc.container(ContainerRef::Definition(d))?.mesh(), t))
                    })
                    .collect();
                for i in 0..solids.len() {
                    for j in i + 1..solids.len() {
                        let (a, ma, ta) = solids[i];
                        let (b, mb, tb) = solids[j];
                        if !s.placements.contains_key(&a) && !s.placements.contains_key(&b) {
                            continue;
                        }
                        if let Ok(r) = boolean(BooleanOp::Intersect, ma, &ta, mb, &tb, &tol) {
                            let v: f64 = r
                                .solids
                                .iter()
                                .map(|m| m.volume().unwrap_or(0.0).abs())
                                .sum();
                            if v > 1e-3
                                && let (Some(pa), Some(pb)) =
                                    (entities.pid_of(a), entities.pid_of(b))
                            {
                                collisions.push((pa, pb, v));
                            }
                        }
                    }
                }
                if !collisions.is_empty() && first_collision.is_none() {
                    first_collision = Some(step as usize);
                }
            }
            frames.push(MotionFrame {
                value: if angle { value.to_degrees() } else { value },
                placements,
                collisions,
            });
        }
        let travel_mm = travel
            .into_iter()
            .filter_map(|(id, d)| entities.pid_of(id).map(|pid| (pid, d)))
            .collect();
        Ok(MotionStudyOutput {
            frames,
            travel_mm,
            first_collision,
        })
    }

    fn meta(&self) -> CommandMeta {
        meta("motion-study", "Motion Study", true)
    }
}

/// Registers the simulation commands.
pub fn register(registry: &mut crate::commands::CommandRegistry) {
    registry.register(SimStatic);
    registry.register(SimFrequency);
    registry.register(SimThermal);
    registry.register(MotionStudy);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::commands::{CommandRegistry, Engine};
    use crate::document::Document;
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn run(e: &mut Engine, name: &str, p: Value) -> Value {
        e.execute_erased(name, p).unwrap()
    }

    fn beam() -> Engine {
        let mut e = Engine::new(Document::new(), Arc::new(CommandRegistry::with_builtins()));
        run(
            &mut e,
            "rectangle",
            json!({ "corner_a": [0, 0, 0], "corner_b": [200, 20, 0] }),
        );
        run(
            &mut e,
            "push-pull",
            json!({"face": {"Query": "face"}, "distance": 20}),
        );
        run(
            &mut e,
            "make-group",
            json!({"targets": "all", "name": "Beam"}),
        );
        e
    }

    #[test]
    fn a_beam_group_held_at_one_end() {
        let mut e = beam();
        let s = run(
            &mut e,
            "sim-static",
            json!({
                "body": "name:Beam",
                "fixed": "face & plane:yz@0",
                "loads": [{ "faces": "face & plane:yz@200", "force": [0, 0, -1000] }],
                "mesh_size": 10,
            }),
        );
        let d = s["max_displacement_mm"].as_f64().unwrap();
        assert!((d - 0.95).abs() < 0.06, "{d}");
        assert!((s["reaction_n"][2].as_f64().unwrap() - 1000.0).abs() < 1e-3);
        assert!(s["safety_factor"].as_f64().unwrap() > 0.5);
        let t = run(
            &mut e,
            "sim-thermal",
            json!({
                "body": "name:Beam",
                "material": "aluminum",
                "conditions": [
                    { "faces": "face & plane:yz@0", "temperature": 20 },
                    { "faces": "face & plane:yz@200", "temperature": 80 }
                ],
                "mesh_size": 10,
            }),
        );
        assert!((t["min_c"].as_f64().unwrap() - 20.0).abs() < 1e-9);
        assert!((t["max_c"].as_f64().unwrap() - 80.0).abs() < 1e-9);
        // Both results are shown; clearing removes them.
        let c = run(&mut e, "analysis-clear", json!({}));
        assert_eq!(c["removed"], 2);
        assert!(
            e.execute_erased(
                "sim-static",
                json!({ "body": "name:Beam", "fixed": "face & plane:yz@999", "gravity": true })
            )
            .is_err()
        );
    }
}
