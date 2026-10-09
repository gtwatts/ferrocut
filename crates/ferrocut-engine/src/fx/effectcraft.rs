//! EffectCraft's pinned native CPU effect library, adapted to Ferrocut's stack.
//!
//! The implementation invokes the vendored upstream render function. Unsupported
//! host-dependent effects are catalogued but never registered as inert effects.
//! Input/output are premultiplied linear ACEScg; arbitrary bounds are normalized
//! to a full-canvas buffer, upstream padding/offset is retained, then the requested
//! region is extracted. GPU frames incur a readback and upload.
//!
//! Numeric parameters use Ferrocut clip-local/timeline animation. Upstream Point
//! controls are exposed as normalized canvas fractions; this keeps defaults exact
//! at every resolution. Popup labels are fixed choices, not animated numbers.
//! Upstream has no cooperative cancellation hook: cancellation is checked around
//! its bounded, monolithic render and during adapter copies.

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, OnceLock};

use effectcraft_effects as ec;
use effectcraft_keyframe::Value as EcValue;
use effectcraft_project::ParamUi;
use ferrocut_core::effect::{
    Canvas, EffectParams, EffectRequest, ParamValue, VideoEffect, WorkingSpace,
};
use ferrocut_core::param::{ParamKind, ParamSpec, TimeBase};
use ferrocut_core::{
    AlphaMode, ColorSpace, CpuFrame, CpuImage, Frame, NodeError, PixelRect, RationalTime, RenderCtx,
};
use half::f16;
use serde_json::{Value, json};

pub const UPSTREAM_REVISION: &str = "6943872cf65b3da1275f1e0f808b60b2e51d84dc";
pub const ADAPTER_VERSION: &str = "effectcraft.6943872.cpu.v1";
/// Canvas size supports UHD 3840x2160; internal padded images have a separate cap.
pub const MAX_CANVAS_PIXELS: usize = 8 * 1024 * 1024;
pub const MAX_BUFFER_PIXELS: usize = 16 * 1024 * 1024;
pub const MAX_AXIS: u32 = 8192;
pub const MAX_PADDING: u32 = 256;
/// Conservative per-worker estimate including adapter and upstream scratch.
pub const MAX_WORKING_BYTES: usize = 1024 * 1024 * 1024;
const MAX_WORK: f64 = 256_000_000.0;
const MAX_TEXT: usize = 4096;
const MAX_TIME_SECONDS: f64 = 100_000_000.0;

// An explicit, source-reviewed admission list. The registry additionally rejects
// effects whose parameters need layer references, structured paths/text/meshes,
// or whose reserved parameter names collide with the stack's own fields.
const ADMITTED: &[&str] = &[
    "ec.blur.gaussian",
    "ec.blur.fastbox",
    "ec.blur.directional",
    "ec.blur.radial",
    "ec.blur.sharpen",
    "ec.blur.unsharp",
    "ec.blur.smart",
    "ec.blur.bilateral",
    "ec.blur.cccross",
    "ec.blur.ccradial",
    "ec.blur.ccradialfast",
    "ec.blur.reduceflicker",
    "ec.blur.channel",
    "ec.channel.arithmetic",
    "ec.channel.combiner",
    "ec.channel.invert",
    "ec.channel.minimax",
    "ec.channel.removecolormatting",
    "ec.channel.shiftchannels",
    "ec.channel.solidcomposite",
    "ec.channel.cccomposite",
    "ec.color.brightnesscontrast",
    "ec.color.exposure",
    "ec.color.levels",
    "ec.color.huesaturation",
    "ec.color.tint",
    "ec.color.gammapedestalgain",
    "ec.color.colorbalance",
    "ec.color.photofilter",
    "ec.color.blackwhite",
    "ec.color.vibrance",
    "ec.color.leavecolor",
    "ec.color.tritone",
    "ec.color.levelsic",
    "ec.color.equalize",
    "ec.color.selectivecolor",
    "ec.color.colorbalancehls",
    "ec.color.broadcast",
    "ec.color.cctoner",
    "ec.color.cccoloroffset",
    "ec.color.cckernel",
    "ec.color.changecolor",
    "ec.color.changetocolor",
    "ec.color.videolimiter",
    "ec.color.cccolorneutralizer",
    "ec.color.psarbitrarymap",
    "ec.color.curves",
    "ec.color.channelmixer",
    "ec.distort.transform",
    "ec.distort.cornerpin",
    "ec.distort.twirl",
    "ec.distort.twirllegacy",
    "ec.distort.spherize",
    "ec.distort.mirror",
    "ec.distort.offset",
    "ec.distort.polar",
    "ec.distort.bulge",
    "ec.distort.opticscompensation",
    "ec.distort.ripple",
    "ec.distort.wavewarp",
    "ec.distort.turbulentdisplace",
    "ec.distort.magnify",
    "ec.distort.bezierwarp",
    "ec.distort.ccbendit",
    "ec.distort.ccflomotion",
    "ec.distort.ccgriddler",
    "ec.distort.cclens",
    "ec.distort.ccpageturn",
    "ec.distort.ccpowerpin",
    "ec.distort.ccripplepulse",
    "ec.distort.ccslant",
    "ec.distort.ccsmear",
    "ec.distort.ccsplit",
    "ec.distort.ccsplit2",
    "ec.distort.cctiler",
    "ec.distort.warp",
    "ec.distort.ccbender",
    "ec.generate.gradientramp",
    "ec.generate.checkerboard",
    "ec.generate.grid",
    "ec.generate.fourcolor",
    "ec.generate.circle",
    "ec.generate.ellipse",
    "ec.generate.lensflare",
    "ec.generate.beam",
    "ec.generate.cellpattern",
    "ec.generate.advancedlightning",
    "ec.generate.cclightrays",
    "ec.generate.cclightburst",
    "ec.generate.cclightsweep",
    "ec.generate.paintbucket",
    "ec.generate.eyedropperfill",
    "ec.generate.ccthreads",
    "ec.noise.noise",
    "ec.noise.fractal",
    "ec.noise.turbulent",
    "ec.noise.addgrain",
    "ec.noise.median",
    "ec.noise.medianlegacy",
    "ec.noise.dustscratches",
    "ec.noise.noisealpha",
    "ec.noise.noisehls",
    "ec.noise.noisehlsauto",
    "ec.noise.curlnoise",
    "ec.key.screen",
    "ec.key.colorkey",
    "ec.key.luma",
    "ec.key.spill",
    "ec.key.advancedspill",
    "ec.key.colorrange",
    "ec.key.linearcolor",
    "ec.key.extract",
    "ec.key.keycleaner",
    "ec.key.unmult",
    "ec.key.ccsimplewireremoval",
    "ec.key.colordifference",
    "ec.matte.simplechoker",
    "ec.matte.mattechoker",
    "ec.obsolete.basic3d",
    "ec.obsolete.gaussianlegacy",
    "ec.perspective.bevelalpha",
    "ec.perspective.beveledges",
    "ec.perspective.cccylinder",
    "ec.perspective.ccsphere",
    "ec.perspective.ccspotlight",
    "ec.perspective.dropshadow",
    "ec.perspective.radialshadow",
    "ec.stylize.posterize",
    "ec.stylize.threshold",
    "ec.stylize.findedges",
    "ec.stylize.mosaic",
    "ec.stylize.glow",
    "ec.stylize.emboss",
    "ec.stylize.cartoon",
    "ec.stylize.roughenedges",
    "ec.stylize.scatter",
    "ec.stylize.strobe",
    "ec.stylize.brushstrokes",
    "ec.stylize.cckaleida",
    "ec.stylize.coloremboss",
    "ec.stylize.ccvignette",
    "ec.stylize.ccthreshold",
    "ec.stylize.ccthresholdrgb",
    "ec.stylize.cchextile",
    "ec.stylize.ccburnfilm",
    "ec.stylize.ccblockload",
    "ec.transition.blockdissolve",
    "ec.transition.iriswipe",
    "ec.transition.linearwipe",
    "ec.transition.radialwipe",
    "ec.transition.venetian",
    "ec.transition.ccglasswipe",
    "ec.transition.ccgridwipe",
    "ec.transition.ccjaws",
    "ec.transition.cclightwipe",
    "ec.transition.cclinesweep",
    "ec.transition.ccradialscalewipe",
    "ec.transition.ccscalewipe",
    "ec.transition.cctwister",
    "ec.vr.blur",
    "ec.vr.chromaticaberrations",
    "ec.vr.colorgradients",
    "ec.vr.converter",
    "ec.vr.denoise",
    "ec.vr.digitalglitch",
    "ec.vr.fractalnoise",
    "ec.vr.glow",
    "ec.vr.planetosphere",
    "ec.vr.rotatesphere",
    "ec.vr.sharpen",
    "ec.vr.spheretoplane",
    "ec.utility.ccoverbrights",
    "ec.utility.cineon",
    "ec.utility.hdrcompander",
    "ec.utility.hdrcompression",
];

fn ui_only(spec: &ec::EffectSpec, p: &ec::ParamSpec) -> bool {
    // Hidden channel selection controls the upstream curve editor, not pixels.
    spec.id == "ec.color.curves" && p.id == "channel"
}

fn intrinsic_time(id: &str) -> bool {
    // Ripple Pulse reads time when its default zero pulse level is changed;
    // the pinned upstream intrinsic-time list omits that nondefault case.
    ec::is_time_dependent(id) || id == "ec.distort.ccripplepulse"
}

/// Why an upstream entry is not registered. Every upstream entry is classified.
pub fn unsupported_reason(spec: &ec::EffectSpec) -> Option<String> {
    if spec.category == "Audio" {
        return Some("Needs the audio DSP adapter; its video render function is intentionally a passthrough upstream.".into());
    }
    if spec.category == "Expression Controls" {
        return Some("Needs agent-addressable control properties, not a video-pixel effect; upstream video rendering is intentionally inert.".into());
    }
    if spec.category == "3D Channel" {
        return Some("Needs source auxiliary EXR depth/object/material/normal/Cryptomatte channels via EffectHost::aux.".into());
    }
    if spec.category == "Time" {
        return Some("Needs neighbouring source frames, temporal recursion/flow and EffectHost::self_at or layer_at.".into());
    }
    if spec.category == "Simulation" {
        return Some("Needs a separately bounded simulation/scene adapter, replay/state controls and optional layer/camera/light/particle host services.".into());
    }
    for p in &spec.params {
        if ui_only(spec, p) {
            continue;
        }
        if ["type", "id", "enabled", "clock_offset", "_seed"].contains(&p.id) {
            return Some(format!(
                "Parameter {:?} collides with a Ferrocut stack field; needs an explicit name mapping.",
                p.id
            ));
        }
        if matches!(
            p.ui,
            ParamUi::Layer | ParamUi::Mask | ParamUi::Path | ParamUi::Text | ParamUi::Gradient
        ) {
            // Text UI is also used for harmless bounded curve strings.
            if !matches!((&p.ui, &p.default), (ParamUi::Text, EcValue::Str(_))) {
                return Some(format!(
                    "Parameter {:?} requires layer/mask/path/rich-text/gradient host data ({:?}).",
                    p.id, p.ui
                ));
            }
        }
        if matches!(
            p.default,
            EcValue::Layer(_) | EcValue::Path(_) | EcValue::Text(_) | EcValue::Gradient(_)
        ) {
            return Some(format!(
                "Parameter {:?} needs structured project/host data unavailable to the single-image adapter.",
                p.id
            ));
        }
        if let ParamUi::Popup { options } = &p.ui {
            let unique: HashSet<_> = options.iter().collect();
            if options.is_empty() || options.len() > 256 || unique.len() != options.len() {
                return Some(format!(
                    "Parameter {:?} has ambiguous or unbounded popup labels; needs an explicit enum mapping.",
                    p.id
                ));
            }
        }
        if matches!(p.default, EcValue::Enum(_)) && !matches!(p.ui, ParamUi::Popup { .. }) {
            return Some(format!(
                "Parameter {:?} is an enum without popup labels; needs an explicit enum mapping.",
                p.id
            ));
        }
    }
    if !ADMITTED.contains(&spec.id) {
        let reason = match spec.id {
            "ec.utility.applylut" | "ec.utility.colorprofileconverter" => {
                "Needs permission-checked LUT/ICC files and explicit asset cache dependencies."
            }
            "ec.color.lumetri"
            | "ec.color.ociofile"
            | "ec.color.ociocolorspace"
            | "ec.color.ociodisplay"
            | "ec.color.ociolook"
            | "ec.color.ociocdl" => {
                "Needs permission-checked LUT/OCIO configuration assets or an independently verified colour-management mapping."
            }
            "ec.color.autocolor"
            | "ec.color.autocontrast"
            | "ec.color.autolevels"
            | "ec.color.shadowhighlight"
            | "ec.color.colorstabilizer"
            | "ec.noise.removegrain"
            | "ec.blur.camerashakedeblur"
            | "ec.distort.rollingshutterrepair"
            | "ec.matte.refinesoft"
            | "ec.matte.refinehard" => {
                "Needs neighbouring-frame host services for temporal smoothing, filtering, stabilization or motion-aware controls."
            }
            "ec.distort.warpstabilizer"
            | "ec.perspective.cameratracker"
            | "ec.utility.facetrackpoints"
            | "ec.utility.facemeasurements"
            | "ec.matte.rotobrush"
            | "ec.obsolete.mochashape" => {
                "Needs tracking/segmentation results, masks and host-managed analysis/state."
            }
            "ec.distort.puppet"
            | "ec.paint.paint"
            | "ec.distort.liquify"
            | "ec.distort.meshwarp" => {
                "Needs structured editable meshes/pins/brush strokes or a separately validated bounded mesh-data adapter."
            }
            "ec.text.numbers"
            | "ec.text.timecode"
            | "ec.obsolete.basictext"
            | "ec.obsolete.pathtext" => {
                "Needs explicit font/rich text/path assets and typography host integration."
            }
            "ec.stylize.motiontile"
            | "ec.stylize.ccrepetile"
            | "ec.distort.upscale"
            | "ec.utility.growbounds" => {
                "Needs explicit resized-output bounds and a separate allocation-budget mapping."
            }
            "ec.generate.fractal" | "ec.obsolete.lightning" => {
                "Needs additional recurrence/segment workload bounds before connecting the procedural renderer."
            }
            "ec.distort.reshape" | "ec.distort.smear" | "ec.key.innerouter"
            | "ec.keying.keylight" => {
                "Needs EffectEnv.masks geometry for mask popups/inside-outside garbage/holdout regions; missing masks must not silently become no-ops."
            }
            "ec.generate.writeon" | "ec.generate.ccgluegun" => {
                "Upstream only renders a current brush dab/bead; needs explicit stroke history and bounded native brush-history integration before claiming write-on/glue stroke capabilities."
            }
            _ => {
                "Not admitted: additional native render-contract/parameter/workload validation remains; no host dependency is assumed from the entry name."
            }
        };
        return Some(reason.into());
    }
    None
}

fn intern(s: String) -> &'static str {
    // Bounded once-only catalogue metadata; no frame/render path leaks.
    Box::leak(s.into_boxed_str())
}

fn number(v: f64) -> Value {
    // Rational strings avoid JSON decimals, which Animatable intentionally rejects.
    Value::String(v.to_string())
}

fn default_json(p: &ec::ParamSpec) -> Value {
    match &p.default {
        EcValue::Scalar(v) => number(*v),
        EcValue::Vec2(v) => json!([number(v[0]), number(v[1])]),
        EcValue::Vec3(v) => json!([number(v[0]), number(v[1]), number(v[2])]),
        EcValue::Color(v) => Value::Array(v.iter().map(|v| number(*v)).collect()),
        EcValue::Bool(v) => json!(v),
        EcValue::Enum(i) => match &p.ui {
            ParamUi::Popup { options } => options
                .get(*i as usize)
                .map(|s| json!(s))
                .unwrap_or(Value::Null),
            _ => number(*i as f64),
        },
        EcValue::Str(s) => json!(s),
        _ => Value::Null,
    }
}

fn metadata(effect_id: &str, p: &ec::ParamSpec) -> ParamSpec {
    let default = intern(default_json(p).to_string());
    let mut doc = p.name.to_owned();
    let mut kind = match &p.default {
        EcValue::Vec2(_) => ParamKind::Vec2,
        EcValue::Vec3(_) => ParamKind::Vec3,
        EcValue::Color(_) => ParamKind::Color,
        EcValue::Bool(_) => ParamKind::Bool,
        EcValue::Str(_) | EcValue::Enum(_) => ParamKind::Choice,
        _ => ParamKind::Scalar,
    };
    let (mut unit, mut min, mut max) = match &p.ui {
        ParamUi::Slider { min, max, .. } => ("upstream units", Some(*min), Some(*max)),
        ParamUi::Percent => ("%", Some(-100.0), Some(100.0)),
        ParamUi::Angle => ("deg", Some(-36_000.0), Some(36_000.0)),
        ParamUi::Pixels => ("px", Some(0.0), Some(8192.0)),
        ParamUi::Point => ("canvas fraction", Some(-16.0), Some(16.0)),
        ParamUi::Point3 => (
            "upstream coordinates",
            Some(-1_000_000.0),
            Some(1_000_000.0),
        ),
        ParamUi::Color => ("linear ACEScg", Some(0.0), Some(1.0)),
        _ => ("", Some(-1_000_000.0), Some(1_000_000.0)),
    };
    let low = p.id.to_ascii_lowercase();
    if unit == "upstream units" {
        if effect_id == "ec.color.exposure" && low.contains("exposure") {
            unit = "stops";
        } else if low == "timespan" {
            unit = "s";
        } else if effect_id == "ec.distort.twirllegacy" && low == "radius" {
            unit = "% of half the shorter canvas axis";
        } else if low.contains("blurriness")
            || low.contains("radius")
            || low == "softness"
            || low == "feather"
            || low == "distance"
            || low == "length"
            || low == "border"
        {
            unit = "px";
        } else if low.contains("opacity")
            || low.contains("blend")
            || low.contains("completion")
            || low.contains("contrast")
            || low.contains("saturation")
            || ["scaleheight", "scalewidth", "magnification", "perspective"].contains(&low.as_str())
        {
            unit = "%";
        }
    }
    // Source-specific exceptions: similarly named controls have different
    // dimensions in the upstream algorithms, so spelling alone is insufficient.
    unit = match (effect_id, p.id) {
        ("ec.distort.twirl", "radius") => "% of longer canvas axis",
        ("ec.distort.ripple", "radius") => "% of half the longer canvas axis",
        ("ec.distort.bulge", "taperRadius") => "%",
        ("ec.perspective.cccylinder", "radius") => "% of canvas width / tau",
        ("ec.vr.digitalglitch", "target/radius") => "deg",
        ("ec.color.changetocolor", "softness")
        | ("ec.key.linearcolor", "softness")
        | ("ec.key.unmult", "softness")
        | ("ec.generate.ellipse", "softness")
        | ("ec.generate.beam", "softness")
        | ("ec.generate.beam", "length")
        | ("ec.vr.planetosphere", "feather") => "%",
        ("ec.stylize.strobe", "strobeDuration") | ("ec.stylize.strobe", "strobePeriod") => "s",
        _ => unit,
    };
    if matches!(p.default, EcValue::Scalar(_)) {
        // Spatial kernels and counts have deliberately smaller CPU-safe domains.
        if low.contains("radius")
            || low.contains("blurriness")
            || low.contains("softness")
            || low == "length"
            || low == "distance"
            || low.contains("choke")
            || low.contains("feather")
            || low == "border"
        {
            min = Some(min.unwrap_or(-128.0).max(-128.0));
            max = Some(max.unwrap_or(128.0).min(128.0));
        }
        if low.contains("complexity") || low.contains("octaves") {
            max = Some(max.unwrap_or(8.0).min(8.0));
        }
        if low == "iterations" || low.contains("passes") {
            max = Some(max.unwrap_or(8.0).min(8.0));
        }
    }
    let choices: &'static [&'static str] = if let ParamUi::Popup { options } = &p.ui {
        kind = ParamKind::Choice;
        Box::leak(
            options
                .iter()
                .map(|s| intern(s.clone()))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        )
    } else {
        &[]
    };
    if matches!(p.ui, ParamUi::Point) {
        doc.push_str("; normalized [x,y] fractions of canvas width/height, expanded to upstream pixel coordinates; [0.5,0.5] is canvas centre");
    }
    if matches!(p.default, EcValue::Str(_)) {
        doc.push_str("; bounded UTF-8 control data, at most 4096 bytes; not animated");
    }
    if p.id == "shutterAngle" {
        min = Some(0.0);
        max = Some(0.0);
        doc.push_str("; must be zero: motion blur requires host parameter resampling");
    }
    if kind == ParamKind::Choice || kind == ParamKind::Bool {
        ParamSpec {
            name: p.id,
            kind,
            animatable: false,
            time: TimeBase::None,
            unit,
            min: None,
            max: None,
            default,
            doc: intern(doc),
            choices,
        }
    } else {
        if let Some(d) = p.default.components().first().copied() {
            // Defaults must remain admissible even if a spatially named scalar
            // (e.g. Twirl radius in percent) has an unusually large default.
            min = min.map(|v| v.min(d));
            max = max.map(|v| v.max(d));
        }
        ParamSpec {
            name: p.id,
            kind,
            animatable: true,
            time: TimeBase::ClipLocal,
            unit,
            min,
            max,
            default,
            doc: intern(doc),
            choices,
        }
    }
}

struct CraftEffect {
    spec: &'static ec::EffectSpec,
    params: Vec<ParamSpec>,
    doc: String,
}

fn adapters() -> &'static [Arc<CraftEffect>] {
    static ALL: OnceLock<Vec<Arc<CraftEffect>>> = OnceLock::new();
    ALL.get_or_init(|| {
        ec::registry().iter().filter(|s| unsupported_reason(s).is_none()).map(|spec| {
            let mut params: Vec<_> = spec.params.iter().filter(|p|!ui_only(spec,p)).map(|p|metadata(spec.id,p)).collect();
            params.push(ParamSpec::scalar("_seed",TimeBase::ClipLocal,"integer","0","deterministic EffectCraft instance seed; rounded to the nearest integer").range(0.0,1_000_000_000.0));
            Arc::new(CraftEffect { spec, params, doc: format!("EffectCraft native CPU {} ({}) in premultiplied linear ACEScg; normalized point controls; bounded full-frame readback/upload",spec.name,spec.category) })
        }).collect()
    })
}

pub fn all() -> Vec<Arc<dyn VideoEffect>> {
    adapters()
        .iter()
        .cloned()
        .map(|a| a as Arc<dyn VideoEffect>)
        .collect()
}

/// Complete upstream index, including explicit unsupported entries.
pub fn catalog() -> Value {
    let entries: Vec<_> = ec::registry().iter().map(|s| {
        let reason = unsupported_reason(s);
        let adapter = adapters().iter().find(|a| a.spec.id == s.id);
        json!({
            "id":s.id,"name":s.name,"category":s.category,
            "status":if reason.is_none() {"connected"} else {"unsupported"},
            "reason":reason,
            "intrinsic_time_dependence":intrinsic_time(s.id),
            "omitted_ui_parameters":s.params.iter().filter(|p|ui_only(s,p)).map(|p|p.id).collect::<Vec<_>>(),
            "limits":match s.id {
                "ec.distort.transform"=>Some("Sharp transform only: shutterAngle must be zero; no host parameter resampling/composition shutter service."),
                "ec.channel.cccomposite"=>Some("Upstream composites the image entering this effect with itself; it does not expose the original pre-stack source."),
                "ec.color.curves"=>Some("Five bounded fixed curve strings; hidden editor channel selector omitted. Curve points do not animate; numeric colour controls support animation."),
                _=>None,
            },
            "parameters":adapter.map(|a|&a.params),
            "upstream_parameters":s.params.iter().map(|p|json!({"id":p.id,"name":p.name,"default":p.default,"ui":p.ui})).collect::<Vec<_>>()
        })
    }).collect();
    json!({"upstream":"storytold/effectcraft","revision":UPSTREAM_REVISION,
        "adapter_version":ADAPTER_VERSION,"entry_count":entries.len(),
        "connected_count":adapters().len(),"unsupported_count":entries.len()-adapters().len(),
        "working_space":"ACEScg","premultiplied":true,"point_units":"canvas fraction",
        "max_canvas_pixels":MAX_CANVAS_PIXELS,"max_buffer_pixels":MAX_BUFFER_PIXELS,
        "max_axis":MAX_AXIS,"max_padding":MAX_PADDING,"max_estimated_working_bytes":MAX_WORKING_BYTES,"entries":entries})
}

fn converted(
    effect: &CraftEffect,
    params: &EffectParams,
    size: [f64; 2],
) -> Result<ec::Params, String> {
    let mut out = ec::Params::default();
    for ps in &effect.spec.params {
        let v = match (params.get(ps.id), &ps.default) {
            (None, _) => ec::default_value(ps, size),
            (Some(ParamValue::Scalar(v)), EcValue::Scalar(_)) => EcValue::Scalar(*v),
            (Some(ParamValue::Vec(v)), EcValue::Vec2(_)) if v.len() == 2 => {
                let mut p = [v[0], v[1]];
                if matches!(ps.ui, ParamUi::Point) {
                    p[0] *= size[0];
                    p[1] *= size[1];
                }
                EcValue::Vec2(p)
            }
            (Some(ParamValue::Vec(v)), EcValue::Vec3(_)) if v.len() == 3 => {
                EcValue::Vec3([v[0], v[1], v[2]])
            }
            (Some(ParamValue::Vec(v)), EcValue::Color(_)) if v.len() == 3 || v.len() == 4 => {
                EcValue::Color([v[0], v[1], v[2], v.get(3).copied().unwrap_or(1.0)])
            }
            (Some(ParamValue::Bool(v)), EcValue::Bool(_)) => EcValue::Bool(*v),
            (Some(ParamValue::Choice(v)), EcValue::Enum(_)) => {
                let ParamUi::Popup { options } = &ps.ui else {
                    return Err(format!("{}: enum has no popup metadata", ps.id));
                };
                let i = options
                    .iter()
                    .position(|s| s == v)
                    .ok_or_else(|| format!("{}: unknown popup choice {v:?}", ps.id))?;
                EcValue::Enum(i as u32)
            }
            (Some(ParamValue::Choice(v)), EcValue::Str(_)) if v.len() <= MAX_TEXT => {
                EcValue::Str(v.clone())
            }
            _ => {
                return Err(format!(
                    "{}: value does not match the upstream parameter type or exceeds its control-data limit",
                    ps.id
                ));
            }
        };
        out.values.insert(ps.id.into(), v);
    }
    Ok(out)
}

fn padding(id: &str, p: &ec::Params, size: [f64; 2], normal: PixelRect) -> f64 {
    let f = |n: &str| p.f(n).max(0.0);
    match id {
        "ec.blur.gaussian" | "ec.obsolete.gaussianlegacy" => {
            if p.b("repeatEdge") {
                0.0
            } else {
                (f("blurriness") * 1.5).ceil()
            }
        }
        "ec.blur.fastbox" => {
            if p.b("repeatEdge") {
                0.0
            } else {
                f("radius").round() * f("iterations").round().clamp(1.0, 50.0) + 1.0
            }
        }
        "ec.blur.directional" => {
            if f("length") < 0.5 {
                0.0
            } else {
                f("length").ceil() + 1.0
            }
        }
        "ec.stylize.glow" => (f("radius") * 1.5).ceil() + 2.0,
        "ec.perspective.dropshadow" => (f("distance") + f("softness") * 1.5).ceil() + 2.0,
        "ec.distort.wavewarp" => p.f("height").abs().ceil() + 1.0,
        "ec.channel.minimax" => f("radius").ceil(),
        "ec.blur.cccross" => {
            let r = f("radiusX").round().max(f("radiusY").round());
            if r == 0.0 { 0.0 } else { r * 3.0 + 1.0 }
        }
        "ec.blur.channel" => {
            if p.b("repeatEdge") {
                0.0
            } else {
                [
                    "redBlurriness",
                    "greenBlurriness",
                    "blueBlurriness",
                    "alphaBlurriness",
                ]
                .into_iter()
                .map(f)
                .fold(0.0f64, f64::max)
                .mul_add(1.5, 0.0)
                .ceil()
                    + 1.0
            }
        }
        "ec.perspective.radialshadow" if p.b("resizeLayer") => {
            let c = p.v2("lightSource");
            let factor = f("projectionDistance") / 100.0;
            let grow = [
                [normal.x as f64, normal.y as f64],
                [normal.right() as f64, normal.y as f64],
                [normal.x as f64, normal.bottom() as f64],
                [normal.right() as f64, normal.bottom() as f64],
            ]
            .into_iter()
            .map(|q| (q[0] - c[0]).hypot(q[1] - c[1]) * factor)
            .fold(0.0f64, f64::max);
            (grow + f("softness") * 1.5).ceil() + 1.0
        }
        "ec.distort.turbulentdisplace" if p.b("resizeLayer") && p.f("amount").abs() >= 1e-9 => {
            p.f("amount").abs().ceil() + 1.0
        }
        "ec.distort.ccpowerpin" => {
            let need = ["topLeft", "topRight", "bottomRight", "bottomLeft"]
                .into_iter()
                .fold(0.0f64, |need, key| {
                    let c = p.v2(key);
                    need.max(normal.x as f64 - c[0])
                        .max(normal.y as f64 - c[1])
                        .max(c[0] - normal.right() as f64)
                        .max(c[1] - normal.bottom() as f64)
                });
            if need > 0.0 {
                need.ceil().min(4096.0) + 1.0
            } else {
                0.0
            }
        }
        "ec.distort.ccslant" => {
            let k = p.f("slant") / 100.0;
            let h = p.f("height") / 100.0;
            ((k * size[1] * h.max(1.0))
                .abs()
                .max((h - 1.0).max(0.0) * size[1]))
            .ceil()
            .min(4096.0)
        }
        "ec.distort.warp" => {
            let k = (p.f("bend").abs()
                + p.f("horizontalDistortion").abs()
                + p.f("verticalDistortion").abs())
                / 100.0;
            if k == 0.0 {
                0.0
            } else {
                (k * 0.3 * size[0].max(size[1])).ceil().min(4096.0) + 1.0
            }
        }
        "ec.distort.opticscompensation"
            if p.b("reverseLensDistortion") && !p.b("optimalPixels") && p.e("resize") > 0 =>
        {
            let half = p.f("fieldOfView").clamp(0.0, 179.0).to_radians() * 0.5;
            if half < 5e-7 {
                return 0.0;
            }
            let r = match p.e("fovOrientation") {
                1 => size[1] * 0.5,
                2 => size[0].hypot(size[1]) * 0.5,
                _ => size[0] * 0.5,
            }
            .max(1.0);
            let c = p.v2("viewCenter");
            let rc = [[0.0, 0.0], [size[0], 0.0], [0.0, size[1]], size]
                .into_iter()
                .map(|q| (q[0] - c[0]).hypot(q[1] - c[1]))
                .fold(0.0f64, f64::max);
            let th = rc / r * half;
            let cap = size[0].max(size[1]) * [0.0, 0.5, 1.5, 3.5][p.e("resize").min(3) as usize];
            let grow = if th < std::f64::consts::FRAC_PI_2 - 1e-3 {
                (r / half.tan() * th.tan() - rc).max(0.0)
            } else {
                cap
            };
            grow.min(cap).min(4096.0).ceil()
        }
        "ec.distort.magnify" if p.b("resizeLayer") && p.e("link") == 0 => {
            let c = p.v2("center");
            let r = p.f("size").max(0.0);
            [r - c[0], r - c[1], c[0] + r - size[0], c[1] + r - size[1]]
                .into_iter()
                .fold(0.0f64, f64::max)
                .min(4096.0)
                .ceil()
        }
        _ => 0.0,
    }
}

fn validate_params(effect: &CraftEffect, p: &EffectParams) -> Result<(), String> {
    for (name, v) in p.iter() {
        let meta = effect
            .params
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| format!("unknown parameter {name:?}"))?;
        let type_ok = matches!(
            (meta.kind, v),
            (ParamKind::Scalar, ParamValue::Scalar(_))
                | (ParamKind::Vec2, ParamValue::Vec(_))
                | (ParamKind::Vec3, ParamValue::Vec(_))
                | (ParamKind::Color, ParamValue::Vec(_))
                | (ParamKind::Bool, ParamValue::Bool(_))
                | (ParamKind::Choice, ParamValue::Choice(_))
        );
        if !type_ok {
            return Err(format!("{name}: value does not match {:?}", meta.kind));
        }
        let values: Vec<f64> = match v {
            ParamValue::Scalar(v) => vec![*v],
            ParamValue::Vec(v) => v.clone(),
            ParamValue::Bool(_) => continue,
            ParamValue::Choice(s) => {
                if s.len() > MAX_TEXT {
                    return Err(format!("{name}: control data exceeds {MAX_TEXT} bytes"));
                }
                if !meta.choices.is_empty() && !meta.choices.contains(&s.as_str()) {
                    return Err(format!("{name}: unsupported choice {s:?}"));
                }
                if effect.spec.id == "ec.color.psarbitrarymap"
                    && name == "map"
                    && !s.trim().is_empty()
                {
                    let samples = s
                        .split(|c: char| c == ',' || c.is_whitespace())
                        .filter(|s| !s.is_empty())
                        .map(|v| {
                            v.parse::<f32>()
                                .ok()
                                .filter(|v| v.is_finite() && (0.0..=255.0).contains(v))
                        })
                        .collect::<Option<Vec<_>>>()
                        .ok_or("map: expected finite comma/space-separated samples in [0,255]")?;
                    if !(2..=1024).contains(&samples.len()) {
                        return Err("map: expected 2..1024 samples".into());
                    }
                }
                if (effect.spec.id == "ec.color.curves"
                    || (effect.spec.id == "ec.stylize.glow" && name == "arbitraryMap"))
                    && !s.trim().is_empty()
                {
                    let curves: Vec<_> = s.split('|').collect();
                    if curves.len()
                        > if effect.spec.id == "ec.color.curves" {
                            1
                        } else {
                            3
                        }
                    {
                        return Err(format!("{name}: too many curve channels"));
                    }
                    for curve in curves {
                        if curve.trim().is_empty() {
                            continue;
                        }
                        let mut xs = Vec::new();
                        for point in curve
                            .split(|c: char| c.is_whitespace() || c == ';')
                            .filter(|s| !s.is_empty())
                        {
                            let values = point
                                .split(',')
                                .map(|v| {
                                    v.parse::<f32>()
                                        .ok()
                                        .filter(|v| v.is_finite() && (-16.0..=16.0).contains(v))
                                })
                                .collect::<Option<Vec<_>>>()
                                .ok_or_else(|| {
                                    format!("{name}: expected finite x,y curve points in [-16,16]")
                                })?;
                            if values.len() != 2 {
                                return Err(format!("{name}: expected x,y curve points"));
                            }
                            xs.push(values[0]);
                        }
                        if !(2..=128).contains(&xs.len()) {
                            return Err(format!("{name}: each nonempty curve needs 2..128 points"));
                        }
                        xs.sort_by(f32::total_cmp);
                        if xs.windows(2).any(|pair| pair[1] - pair[0] < 1e-6) {
                            return Err(format!(
                                "{name}: curve x coordinates must be distinct (at least 1e-6 apart)"
                            ));
                        }
                    }
                }
                continue;
            }
        };
        for v in values {
            if !v.is_finite() || meta.min.is_some_and(|m| v < m) || meta.max.is_some_and(|m| v > m)
            {
                return Err(format!(
                    "{name}: value {v} is outside the bounded adapter domain {:?}..{:?}",
                    meta.min, meta.max
                ));
            }
        }
    }
    let ec = converted(effect, p, [1.0, 1.0])?;
    let pad = padding(effect.spec.id, &ec, [1.0, 1.0], PixelRect::full(1, 1));
    if !pad.is_finite() || pad > MAX_PADDING as f64 {
        return Err(format!(
            "padding {pad} exceeds the {MAX_PADDING}px adapter budget"
        ));
    }
    Ok(())
}

fn checked_area(w: u32, h: u32, limit: usize, what: &str) -> Result<usize, String> {
    if w == 0 || h == 0 || w > MAX_AXIS + 2 * MAX_PADDING || h > MAX_AXIS + 2 * MAX_PADDING {
        return Err(format!(
            "{what}: nonzero dimensions must fit {} pixels per axis",
            MAX_AXIS + 2 * MAX_PADDING
        ));
    }
    (w as usize)
        .checked_mul(h as usize)
        .filter(|n| *n <= limit)
        .ok_or_else(|| format!("{what}: exceeds {limit} pixels"))
}

fn padded_window(rect: PixelRect, pad: u32) -> PixelRect {
    PixelRect::new(
        rect.x.saturating_sub(pad as i32),
        rect.y.saturating_sub(pad as i32),
        rect.width.saturating_add(pad.saturating_mul(2)),
        rect.height.saturating_add(pad.saturating_mul(2)),
    )
}

fn memory_budget(
    id: &str,
    scratch_pixels: usize,
    input_pixels: usize,
    output_pixels: usize,
) -> Result<(), String> {
    // Conservative float-image equivalents cover planes, parallel channel
    // scratch and recurrent clones in the admitted upstream implementations.
    // This is a preflight estimate, not an allocator-enforced process RSS limit.
    let images = match id {
        "ec.vr.denoise" => 16usize,
        "ec.stylize.glow"
        | "ec.stylize.cartoon"
        | "ec.generate.advancedlightning"
        | "ec.generate.paintbucket"
        | "ec.noise.median"
        | "ec.noise.medianlegacy"
        | "ec.noise.dustscratches"
        | "ec.matte.mattechoker" => 8,
        _ if id.starts_with("ec.blur.") || id.starts_with("ec.perspective.") => 6,
        _ => 4,
    };
    let bytes = scratch_pixels
        .checked_mul(16 * images)
        .and_then(|n| {
            input_pixels
                .checked_add(output_pixels)
                .and_then(|p| p.checked_mul(8))
                .and_then(|p| n.checked_add(p))
        })
        .ok_or("estimated working storage overflow")?;
    if bytes > MAX_WORKING_BYTES {
        Err(format!(
            "estimated CPU working storage {bytes} exceeds {MAX_WORKING_BYTES} bytes; reduce resolution or overscan"
        ))
    } else {
        Ok(())
    }
}

fn workload(id: &str, p: &ec::Params, pixels: usize) -> Result<(), String> {
    let f = |n: &str| p.f(n).abs();
    let samples = match id {
        "ec.blur.directional" => (f("length").ceil() * 2.0 + 1.0).clamp(1.0, 257.0),
        "ec.blur.radial" | "ec.blur.ccradial" | "ec.blur.ccradialfast" => 32.0,
        "ec.blur.bilateral" | "ec.blur.smart" => {
            let r = f("radius");
            let stride = (r / 6.0).ceil().max(1.0);
            ((r.ceil() * 2.0 / stride).floor() + 1.0).powi(2).max(1.0)
        }
        "ec.noise.median" | "ec.noise.medianlegacy" | "ec.noise.dustscratches" => {
            (f("radius").ceil() * 2.0 + 1.0).powi(2).max(1.0)
        }
        "ec.generate.cellpattern" => {
            if (6..=10).contains(&p.e("cellPattern")) {
                400.0
            } else {
                9.0
            }
        }
        "ec.noise.fractal" | "ec.noise.turbulent" => {
            let lattice = match p.e("noiseType") {
                0 => 2.0,
                1 | 2 => 8.0,
                _ => 32.0,
            };
            let domain = if (4..=6).contains(&p.e("fractalType")) {
                3.0
            } else {
                1.0
            };
            let cycle = if p.b("evolutionOptions/cycleEvolution") {
                2.0
            } else {
                1.0
            };
            f("complexity").ceil().max(1.0) * lattice * domain * cycle
        }
        "ec.vr.fractalnoise" => f("complexity").ceil().max(1.0) * 16.0,
        "ec.distort.turbulentdisplace" | "ec.stylize.roughenedges" => {
            f("complexity").ceil().max(1.0) * 32.0
        }
        "ec.generate.cclightrays" | "ec.generate.cclightburst" => 32.0,
        "ec.vr.denoise" if p.e("noiseType") == 1 => {
            ((p.f("noiseLevel").max(0.0) / 100.0 * 3.0).round().max(1.0) * 2.0 + 1.0).powi(2)
        }
        _ => 1.0,
    };
    let cost = pixels as f64 * samples;
    if !cost.is_finite() || cost > MAX_WORK {
        Err(format!(
            "estimated CPU sampling work {cost} exceeds the {MAX_WORK} sample budget; reduce resolution or kernel complexity"
        ))
    } else {
        Ok(())
    }
}

fn clock_domain(
    time: RationalTime,
    param_time: RationalTime,
    effect_time: RationalTime,
) -> Result<(), String> {
    if [time, param_time, effect_time].iter().any(|time| {
        let seconds = time.0.to_f64();
        !seconds.is_finite() || seconds.abs() > MAX_TIME_SECONDS
    }) {
        Err("time exceeds the bounded CPU adapter domain".into())
    } else {
        Ok(())
    }
}

/// Run the actual upstream effect without a GPU, for workers and reference tests.
/// The input must have ACEScg premultiplied finite pixels and valid storage.
/// Cancellation is checked around upstream execution and every adapter row.
pub fn render_cpu(
    kind: &str,
    input: &CpuFrame,
    params: &EffectParams,
    req: &EffectRequest,
    check: impl Fn() -> Result<(), NodeError>,
) -> Result<CpuFrame, NodeError> {
    check()?;
    let fail = |s: String| NodeError::permanent(format!("EffectCraft {kind}: {s}"));
    let effect = adapters()
        .iter()
        .find(|e| e.spec.id == kind)
        .ok_or_else(|| {
            fail("unsupported effect; consult the complete EffectCraft catalogue".into())
        })?;
    validate_params(effect, params).map_err(&fail)?;
    if input.color_space.name() != WorkingSpace::ACESCG.name()
        || input.alpha != AlphaMode::Premultiplied
    {
        return Err(fail("requires premultiplied linear ACEScg input".into()));
    }
    if req.canvas.width != input.width || req.canvas.height != input.height {
        return Err(fail(
            "input display window disagrees with the requested canvas".into(),
        ));
    }
    if req.canvas.width > MAX_AXIS || req.canvas.height > MAX_AXIS {
        return Err(fail(format!("canvas axes exceed {MAX_AXIS}")));
    }
    checked_area(input.width, input.height, MAX_CANVAS_PIXELS, "canvas").map_err(&fail)?;
    let input_count = checked_area(
        input.data_window.width,
        input.data_window.height,
        MAX_BUFFER_PIXELS,
        "input",
    )
    .map_err(&fail)?;
    for (what, window) in [("input", input.data_window), ("output", req.region)] {
        if window.x.unsigned_abs() > MAX_AXIS + MAX_PADDING
            || window.y.unsigned_abs() > MAX_AXIS + MAX_PADDING
            || window.right().abs() > (MAX_AXIS * 2) as i64
            || window.bottom().abs() > (MAX_AXIS * 2) as i64
        {
            return Err(fail(format!(
                "{what}: pixel coordinates exceed the bounded adapter domain"
            )));
        }
    }
    if input.image.pixels.len() != input_count * 4 {
        return Err(fail("input storage does not match its data window".into()));
    }
    if !req.canvas.frame_rate.is_finite()
        || req.canvas.frame_rate <= 0.0
        || req.canvas.frame_rate > 1000.0
    {
        return Err(fail("frame rate must be finite and in (0,1000]".into()));
    }
    clock_domain(req.time, req.param_time, req.effect_time).map_err(&fail)?;
    let size = [input.width as f64, input.height as f64];
    let upstream_params = converted(effect, params, size).map_err(&fail)?;
    let normal = input.data_window.union(&req.canvas.display_window());
    let pad = padding(kind, &upstream_params, size, normal);
    if !pad.is_finite() || pad > MAX_PADDING as f64 {
        return Err(fail(format!(
            "padding {pad} exceeds {MAX_PADDING}px; reduce the spatial controls"
        )));
    }
    let pad = pad.ceil() as u32;
    let count = checked_area(
        normal.width,
        normal.height,
        MAX_BUFFER_PIXELS,
        "normalized input",
    )
    .map_err(&fail)?;
    let padded = padded_window(normal, pad);
    let mut scratch_count = checked_area(
        padded.width,
        padded.height,
        MAX_BUFFER_PIXELS,
        "padded output",
    )
    .map_err(&fail)?;
    if kind == "ec.vr.denoise" && upstream_params.e("noiseType") != 1 {
        let internal_pad =
            ((1.0 + upstream_params.f("noiseLevel").max(0.0) / 100.0 * 4.0).max(1.0) as u32) * 2
                + 1;
        if internal_pad > MAX_PADDING {
            return Err(fail(
                "VR Denoise internal padding exceeds the adapter budget".into(),
            ));
        }
        let internal = padded_window(normal, internal_pad);
        scratch_count = scratch_count.max(
            checked_area(
                internal.width,
                internal.height,
                MAX_BUFFER_PIXELS,
                "VR Denoise internal image",
            )
            .map_err(&fail)?,
        );
    }
    let out_count = checked_area(
        req.region.width,
        req.region.height,
        MAX_BUFFER_PIXELS,
        "requested output",
    )
    .map_err(&fail)?;
    memory_budget(kind, scratch_count, input_count, out_count).map_err(&fail)?;
    workload(kind, &upstream_params, count).map_err(&fail)?;
    let mut data = Vec::new();
    data.try_reserve_exact(count)
        .map_err(|e| fail(format!("input allocation: {e}")))?;
    data.resize(count, [0.0; 4]);
    for y in 0..input.data_window.height as usize {
        check()?;
        for x in 0..input.data_window.width as usize {
            let i = (y * input.data_window.width as usize + x) * 4;
            let p = [0, 1, 2, 3].map(|c| input.image.pixels[i + c].to_f32());
            if p.iter().any(|v| !v.is_finite()) || !(0.0..=1.0).contains(&p[3]) {
                return Err(fail(
                    "input contains nonfinite colour or alpha outside [0,1]".into(),
                ));
            }
            let dx = (input.data_window.x as i64 - normal.x as i64 + x as i64) as usize;
            let dy = (input.data_window.y as i64 - normal.y as i64 + y as i64) as usize;
            data[dy * normal.width as usize + dx] = if p[3] == 0.0 { [0.0; 4] } else { p };
        }
    }
    let buf = ec::Buf {
        img: ec::Image {
            width: normal.width,
            height: normal.height,
            data,
        },
        offset: [-(normal.x as f64), -(normal.y as f64)],
        scale: 1.0,
    };
    let seed = params.scalar_opt("_seed").unwrap_or(0.0).round() as u32;
    let env = ec::EffectEnv {
        comp_time: req.time.0.to_f64(),
        frame_rate: req.canvas.frame_rate,
        working_space: Some(effectcraft_color::ColorSpace::AcesCg),
        working_linear: true,
        ..Default::default()
    };
    let context = ec::EffectCtx {
        params: &upstream_params,
        time: req.effect_time.0.to_f64(),
        layer_size: size,
        seed,
        adjustment: false,
        env,
    };
    check()?;
    if kind == "ec.generate.advancedlightning" {
        let segments = catch_unwind(AssertUnwindSafe(|| ec::lightning_segments(&context, &buf)))
            .map_err(|_| fail("upstream lightning planner panicked; frame was discarded".into()))?;
        if segments.len() as f64 * count as f64 > MAX_WORK {
            return Err(fail("lightning segment sampling exceeds the CPU work budget; reduce resolution, complexity or forking".into()));
        }
        check()?;
    }
    let output = catch_unwind(AssertUnwindSafe(|| ec::apply(effect.spec, &context, buf)))
        .map_err(|_| fail("upstream renderer panicked; frame was discarded".into()))?;
    check()?;
    let actual_count = checked_area(
        output.img.width,
        output.img.height,
        MAX_BUFFER_PIXELS,
        "upstream output",
    )
    .map_err(&fail)?;
    if output.img.data.len() != actual_count
        || output.scale != 1.0
        || output
            .offset
            .iter()
            .any(|v| !v.is_finite() || v.fract() != 0.0 || v.abs() > 100_000.0)
    {
        return Err(fail(
            "upstream returned invalid storage/scale/offset".into(),
        ));
    }
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(out_count * 4)
        .map_err(|e| fail(format!("output allocation: {e}")))?;
    let origin = [-output.offset[0] as i64, -output.offset[1] as i64];
    for y in 0..req.region.height as i64 {
        check()?;
        for x in 0..req.region.width as i64 {
            let sx = req.region.x as i64 + x - origin[0];
            let sy = req.region.y as i64 + y - origin[1];
            let p = if sx >= 0
                && sy >= 0
                && sx < output.img.width as i64
                && sy < output.img.height as i64
            {
                output.img.data[sy as usize * output.img.width as usize + sx as usize]
            } else {
                [0.0; 4]
            };
            if p.iter().any(|v| !v.is_finite()) {
                return Err(fail("upstream produced nonfinite pixels".into()));
            }
            let a = p[3].clamp(0.0, 1.0);
            let clean = if p[3] <= 0.0 {
                [0.0; 4]
            } else {
                [p[0] * a / p[3], p[1] * a / p[3], p[2] * a / p[3], a]
            };
            pixels.extend(clean.map(|v| f16::from_f32(v.clamp(-65504.0, 65504.0))));
        }
    }
    Ok(CpuFrame {
        width: input.width,
        height: input.height,
        data_window: req.region,
        pixel_aspect: input.pixel_aspect,
        color_space: ColorSpace::acescg(),
        alpha: AlphaMode::Premultiplied,
        image: Arc::new(CpuImage { pixels }),
    })
}

impl VideoEffect for CraftEffect {
    fn type_name(&self) -> &str {
        self.spec.id
    }
    fn doc(&self) -> &str {
        &self.doc
    }
    fn params(&self) -> &[ParamSpec] {
        &self.params
    }
    fn version(&self) -> &str {
        ADAPTER_VERSION
    }
    fn working_space(&self, _: &EffectParams) -> WorkingSpace {
        WorkingSpace::ACESCG
    }
    fn validate(&self, p: &EffectParams) -> Result<(), String> {
        validate_params(self, p)
    }
    fn time_dependent(&self) -> bool {
        intrinsic_time(self.spec.id)
    }
    fn validate_clocks(
        &self,
        time: RationalTime,
        param_time: RationalTime,
        effect_time: RationalTime,
    ) -> Result<(), String> {
        clock_domain(time, param_time, effect_time)
    }
    fn batches_gpu_work(&self) -> bool {
        false
    }
    fn output_window(&self, input: PixelRect, p: &EffectParams, canvas: Canvas) -> PixelRect {
        let pad = converted(self, p, [canvas.width as f64, canvas.height as f64])
            .map(|p| {
                padding(
                    self.spec.id,
                    &p,
                    [canvas.width as f64, canvas.height as f64],
                    input.union(&canvas.display_window()),
                )
                .ceil()
                .clamp(0.0, MAX_PADDING as f64) as u32
            })
            .unwrap_or(0);
        padded_window(input.union(&canvas.display_window()), pad)
    }
    fn input_region(
        &self,
        _: PixelRect,
        _: &EffectParams,
        _: ferrocut_core::RationalTime,
    ) -> PixelRect {
        // Upstream algorithms may inspect histograms, distant warp coordinates
        // or all pixels. Ask for the entire bounded input, not the output ROI.
        PixelRect::new(
            -(MAX_AXIS as i32),
            -(MAX_AXIS as i32),
            MAX_AXIS * 3,
            MAX_AXIS * 3,
        )
    }
    fn render(
        &self,
        ctx: &mut RenderCtx<'_>,
        input: &Frame,
        p: &EffectParams,
        req: &EffectRequest,
    ) -> Result<Frame, NodeError> {
        ctx.check()?;
        clock_domain(req.time, req.param_time, req.effect_time)
            .map_err(|s| NodeError::permanent(format!("EffectCraft {}: {s}", self.spec.id)))?;
        // Reject hostile display/storage sizes before the GPU readback allocates
        // a CPU copy. Full semantic validation follows in render_cpu.
        let fail = |m: String| NodeError::permanent(format!("EffectCraft {}: {m}", self.spec.id));
        checked_area(input.width, input.height, MAX_CANVAS_PIXELS, "canvas").map_err(&fail)?;
        checked_area(
            input.data_window.width,
            input.data_window.height,
            MAX_BUFFER_PIXELS,
            "input",
        )
        .map_err(&fail)?;
        let limit = ctx.gpu.device.limits().max_texture_dimension_2d;
        if req.region.width > limit || req.region.height > limit {
            return Err(fail(format!(
                "requested output exceeds the worker texture axis limit {limit}"
            )));
        }
        ctx.flush();
        let input = input
            .to_cpu_frame(ctx.gpu)
            .map_err(|e| NodeError::retryable(format!("EffectCraft readback: {e}")))?;
        let out = render_cpu(self.spec.id, &input, p, req, || ctx.check())?;
        ctx.check()?;
        Ok(Frame::from_cpu(&out).to_gpu(ctx.gpu))
    }
}
