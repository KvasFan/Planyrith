//! `planyrith render` (docs/08 §2, docs/06 §11): headless images of a
//! document, one view or every scene.

use std::path::{Path, PathBuf};
use std::time::Instant;

use glam::DVec3;
use planyrith_core::commands::params::{CliType, parse_cli_value};
use planyrith_core::document::{Camera, Document, Projection};
use planyrith_core::units::LengthUnit;
use planyrith_render::{Gpu, HeadlessOptions, render_image};
use serde_json::{Value, json};

use crate::session::Session;
use crate::{CliError, Global, out};

/// `render` options (docs/08 §2).
#[derive(Debug, Clone, clap::Args)]
pub struct RenderArgs {
    /// Output image (`.png`, `.jpg` or `.webp`); not with `--all-scenes`.
    pub output: Option<PathBuf>,
    /// Width in pixels.
    #[arg(long, default_value_t = 1920)]
    pub width: u32,
    /// Height in pixels.
    #[arg(long, default_value_t = 1080)]
    pub height: u32,
    /// Render from a scene's camera.
    #[arg(long, conflicts_with = "camera")]
    pub scene: Option<String>,
    /// Camera as `eye=x,y,z;target=x,y,z[;fov=35|height=2m][;projection=perspective|parallel|two-point]`.
    #[arg(long)]
    pub camera: Option<String>,
    /// Style name (default: the active style).
    #[arg(long)]
    pub style: Option<String>,
    /// Sun shadows (not rendered yet: a warning is printed).
    #[arg(long)]
    pub shadows: bool,
    /// Transparent background (PNG and WebP).
    #[arg(long)]
    pub transparent: bool,
    /// Multisampling: 1 or 4.
    #[arg(long, default_value_t = 4)]
    pub msaa: u32,
    /// Image format (default: from the extension).
    #[arg(long)]
    pub format: Option<String>,
    /// Render every scene into this directory as `<scene>.png`.
    #[arg(long, value_name = "DIR", conflicts_with_all = ["scene", "camera"])]
    pub all_scenes: Option<PathBuf>,
    /// Draw hidden geometry.
    #[arg(long)]
    pub show_hidden: bool,
}

/// Runs the verb.
pub fn render(global: &Global, args: &RenderArgs) -> Result<(), CliError> {
    let started = Instant::now();
    let session = Session::open(global, false)?;
    let doc = session.engine.document();
    let unit = session.unit(global)?;
    let style = match &args.style {
        Some(name) => Some(
            doc.styles
                .by_name(name)
                .and_then(|id| doc.styles.get(id))
                .cloned()
                .ok_or_else(|| CliError::invalid("style", format!("no style named `{name}`")))?,
        ),
        None => None,
    };
    if args.shadows {
        out::diagnostic("warning: shadows are not rendered yet; rendering without them");
    }
    let gpu = Gpu::headless().map_err(render_error)?;
    let mut outputs = Vec::new();
    let mut views: Vec<(PathBuf, Camera)> = Vec::new();
    if let Some(dir) = &args.all_scenes {
        std::fs::create_dir_all(dir).map_err(|e| io_error(dir, &e))?;
        for (_, scene) in doc.scenes.iter() {
            let file = dir.join(format!("{}.png", file_name(&scene.name)));
            views.push((file, scene.camera.unwrap_or(doc.camera)));
        }
    } else {
        let output = args
            .output
            .clone()
            .ok_or_else(|| CliError::usage("give the output image path (or --all-scenes <dir>)"))?;
        let camera = match (&args.scene, &args.camera) {
            (Some(name), _) => {
                let id = doc.scenes.by_name(name).ok_or_else(|| {
                    CliError::invalid("scene", format!("no scene named `{name}`"))
                })?;
                doc.scenes
                    .get(id)
                    .and_then(|s| s.camera)
                    .unwrap_or(doc.camera)
            }
            (None, Some(text)) => parse_camera(doc, text, unit)?,
            (None, None) => doc.camera,
        };
        views.push((output, camera));
    }
    for (path, camera) in views {
        let image = render_image(
            doc,
            &HeadlessOptions {
                width: args.width,
                height: args.height,
                msaa: args.msaa,
                camera: Some(camera),
                style: style.clone(),
                show_hidden: args.show_hidden,
                transparent_background: args.transparent,
            },
            Some(&gpu),
        )
        .map_err(render_error)?;
        save(&image, &path, args.format.as_deref())?;
        outputs.push(json!(path.display().to_string()));
    }
    let output = json!({
        "images": outputs,
        "width": args.width,
        "height": args.height,
        "adapter": gpu.adapter.name,
    });
    if global.json || global.jsonl {
        out::line(json!({
            "ok": true,
            "command": "render",
            "output": output,
            "warnings": Value::Array(Vec::new()),
            "timing_ms": started.elapsed().as_secs_f64() * 1000.0,
        }))
    } else {
        for path in &outputs {
            out::diagnostic(format!("rendered {}", path.as_str().unwrap_or_default()));
        }
        Ok(())
    }
}

/// A scene name as a file name: path separators and control characters
/// become `_`.
fn file_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// `eye=…;target=…;fov=…|height=…;projection=…` (docs/08 §2).
fn parse_camera(doc: &Document, text: &str, unit: LengthUnit) -> Result<Camera, CliError> {
    let mut camera = doc.camera;
    let mut fov = None;
    let mut height = None;
    let mut projection = None;
    for part in text.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let (key, value) = part
            .split_once('=')
            .ok_or_else(|| CliError::invalid("camera", format!("`{part}` is not key=value")))?;
        let point = |v: &str| -> Result<DVec3, CliError> {
            let json = parse_cli_value(CliType::Point, "camera", v, unit)
                .map_err(CliError::from_command)?;
            serde_json::from_value::<[f64; 3]>(json)
                .map(DVec3::from)
                .map_err(|e| CliError::invalid("camera", e.to_string()))
        };
        match key.trim() {
            "eye" => camera.eye = point(value)?,
            "target" => camera.target = point(value)?,
            "up" => camera.up = point(value)?,
            "fov" => {
                let v = parse_cli_value(CliType::Angle, "camera", value, unit)
                    .map_err(CliError::from_command)?;
                fov = v.as_f64().map(f64::to_degrees);
            }
            "height" => {
                let v = parse_cli_value(CliType::Length, "camera", value, unit)
                    .map_err(CliError::from_command)?;
                height = v.as_f64();
            }
            "projection" => projection = Some(value.trim().to_string()),
            other => {
                return Err(CliError::invalid(
                    "camera",
                    format!("unknown key `{other}` (eye, target, up, fov, height, projection)"),
                ));
            }
        }
    }
    let current_fov = match camera.projection {
        Projection::Perspective { fov_y_deg } | Projection::TwoPoint { fov_y_deg } => fov_y_deg,
        Projection::Parallel { .. } => 35.0,
    };
    camera.projection = match projection.as_deref() {
        Some("parallel") => Projection::Parallel {
            height_mm: height.unwrap_or(match camera.projection {
                Projection::Parallel { height_mm } => height_mm,
                _ => 2.0 * camera.distance() * tan_half(current_fov),
            }),
        },
        Some("two-point") => Projection::TwoPoint {
            fov_y_deg: fov.unwrap_or(current_fov),
        },
        Some("perspective") | None => match (camera.projection, projection.is_none(), fov) {
            (Projection::Parallel { height_mm }, true, None) => Projection::Parallel {
                height_mm: height.unwrap_or(height_mm),
            },
            (Projection::TwoPoint { .. }, true, f) => Projection::TwoPoint {
                fov_y_deg: f.unwrap_or(current_fov),
            },
            (_, _, f) => Projection::Perspective {
                fov_y_deg: f.unwrap_or(current_fov),
            },
        },
        Some(other) => {
            return Err(CliError::invalid(
                "camera",
                format!("unknown projection `{other}` (perspective, parallel, two-point)"),
            ));
        }
    };
    if camera.eye.distance(camera.target) < doc.tolerances().length {
        return Err(CliError::invalid("camera", "eye and target coincide"));
    }
    Ok(camera)
}

fn tan_half(fov_deg: f64) -> f64 {
    (fov_deg.to_radians() * 0.5).tan()
}

fn save(image: &image::RgbaImage, path: &Path, format: Option<&str>) -> Result<(), CliError> {
    let format = match format {
        Some(f) => image::ImageFormat::from_extension(f),
        None => image::ImageFormat::from_path(path).ok(),
    }
    .ok_or_else(|| CliError::invalid("format", "use png, jpg or webp"))?;
    let result = match format {
        image::ImageFormat::Jpeg => image::DynamicImage::ImageRgba8(image.clone())
            .to_rgb8()
            .save_with_format(path, format),
        image::ImageFormat::Png | image::ImageFormat::WebP => image.save_with_format(path, format),
        _ => return Err(CliError::invalid("format", "use png, jpg or webp")),
    };
    result.map_err(|e| {
        CliError::io(planyrith_core::io::IoError::storage(
            path.display().to_string(),
            e.to_string(),
        ))
    })
}

fn io_error(path: &Path, e: &std::io::Error) -> CliError {
    CliError::io(planyrith_core::io::IoError::storage(
        path.display().to_string(),
        e.to_string(),
    ))
}

/// Rendering failures are I/O-class errors (exit 4): the output could not be
/// produced on this machine.
fn render_error(e: planyrith_render::RenderError) -> CliError {
    CliError::io(planyrith_core::io::IoError::storage(
        "render",
        e.to_string(),
    ))
}
