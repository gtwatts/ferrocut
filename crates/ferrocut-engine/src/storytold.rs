//! Agent discovery for pinned reused libraries. Presence is distinct from wiring.

use serde_json::{Value, json};

const MANIFEST: &str = include_str!("../../../vendor/storytold/manifest.json");

/// Source provenance and the actual adapter entry points in this build. This
/// describes connectivity; visual fidelity and measured quality need evidence
/// from demanding projects, not registry counts.
pub fn capabilities() -> Value {
    let manifest: Value = match serde_json::from_str(MANIFEST) {
        Ok(v) => v,
        Err(e) => return json!({"error":format!("invalid embedded source manifest: {e}")}),
    };
    let build_dependencies = [
        "filmcraft-cfb",
        "filmcraft-color",
        "filmcraft-frame",
        "filmcraft-geom",
        "filmcraft-interchange",
        "filmcraft-media",
        "filmcraft-project",
        "filmcraft-scopes",
        "filmcraft-time",
        "effectcraft-color",
        "effectcraft-effects",
        "effectcraft-geom",
        "effectcraft-keyframe",
        "effectcraft-path",
        "effectcraft-project",
        "effectcraft-raster",
        "effectcraft-segment",
        "effectcraft-time",
        "effectcraft-track",
    ];
    let repositories: Vec<Value> = ["filmcraft","effectcraft"].iter().map(|name| {
        let repo = &manifest["repositories"][name];
        let packages: Vec<Value> = repo["packages"].as_array().into_iter().flatten().map(|p| json!({
            "package":p,
            "source_present":true,
            "build_dependency":p.as_str().is_some_and(|s|build_dependencies.contains(&s)),
        })).collect();
        json!({"name":name,"url":repo["url"],"revision":repo["revision"],"version":repo["version"],
            "source_package_count":packages.len(),"packages":packages,"omitted":repo["omitted"],"modifications":repo["modifications"],"local_patches":repo["local_patches"]})
    }).collect();
    let ec = crate::fx::effectcraft::catalog();
    json!({
        "schema":"ferrocut.capabilities/1",
        "repositories":repositories,
        "connected":{
            "media_placement":{"timeline_path":"clips.fit / output.fit","modes":["contain","cover","none","stretch"],
                "default":"contain","anchor_units":"native source pixels","position_units":"output pixels",
                "scale":"relative to fit","diagnostics":"plan and render: base placement factors and stretch warnings",
                "limitations":["square source pixels","no punch_in operation","transformed-source tracking unsupported","no preview badges","native anchor expressions need an explicit value"]},
            "effectcraft_effects":{"count":ec["connected_count"],"catalog":"effects_catalog",
                "timeline_path":"clips/tracks.effects","execution":"upstream CPU kernels, GPU readback/upload when needed",
                "working_space":"premultiplied linear ACEScg","time":"clip-local for clips, timeline for tracks"},
            "effectcraft_paths":{"geometry":["polygon","star"],
                "operators":["trim","round_corners","offset","pucker_bloat","zigzag","twist","wiggle","reverse","merge"],
                "timeline_path":"generator.shape","time":"source"},
            "native_vector_groups":{"timeline_path":"generator.group","generator_type":"vector_group",
                "time":"source","algorithms":"actual EffectCraft Mat3/path transforms with Ferrocut copy/opacity adapter",
                "features":["nested_groups","repeater","fractional_copies","per_copy_opacity","skew"],
                "max_depth":crate::vector_instances::MAX_GROUP_DEPTH,"max_expanded_draws":crate::vector_instances::MAX_VECTOR_INSTANCES,
                "group_opacity":"per-descendant paint; no isolated intermediate group"},
            "native_masks":{"timeline_path":"clips.masks","time":"source before clip effects and transform",
                "algorithms":"FilmCraft MaskPath flatten/combine and adapted bounded signed-distance rasterizer",
                "modes":["none","add","subtract","intersect","lighten","darken","difference"],
                "features":["animated_bezier_geometry","opacity","inversion","isotropic_feather","signed_expansion"]},
            "effectcraft_tracking":{"tools":["tracking_analyze","tracking_keyframes"],
                "algorithms":"actual CPU NCC/LK tracker and Gaussian/no-motion translation corrections",
                "features":["point_translation","confidence","explicit_loss","motion_attachment","translation_stabilization"],
                "source_binding":"full-file Blake3 checked before/after analysis and before planning edits",
                "unsupported":["planar_perspective","3d_camera_solve","nonlinear_retime_key_conversion","automatic_crop_or_border_fill"]},
            "filmcraft_scopes":{"tool":"scopes_read","input":"decoded display-encoded RGBA8",
                "outputs":["waveform","parade","histogram","vectorscope_yuv","vectorscope_hls","channel_statistics"]},
            "filmcraft_interchange":{"tools":["timeline_import","timeline_export"],"formats":["otio","fcp7"],
                "loss_reporting":true,"lossy_writes_require_allow_loss":true}
        },
        "registered_video_effect_count":crate::fx::type_names().len(),
        "unsupported_effectcraft_effect_count":ec["unsupported_count"],
        "status_meaning":"source_present = retained source; build_dependency = compiled dependency; connected = adapter exposed. None implies Adobe parity, real-time speed, or independently verified visual fidelity.",
        "evidence":"docs/integrations/STORYTOLD.md",
        "licenses":"vendor/storytold/{filmcraft,effectcraft}/{LICENSE-MIT,LICENSE-APACHE,NOTICE}"
    })
}

/// Search the complete upstream catalog while exposing every usable native
/// Ferrocut effect with its typed controls. Unsupported upstream entries have
/// a reason and are absent from the executable registry.
pub fn effects_catalog(
    query: Option<&str>,
    offset: usize,
    limit: usize,
    details: bool,
) -> anyhow::Result<Value> {
    anyhow::ensure!((1..=100).contains(&limit), "catalog limit must be 1..=100");
    let needle = query.unwrap_or("").to_lowercase();
    let matches = |v: &Value| needle.is_empty() || v.to_string().to_lowercase().contains(&needle);
    let mut upstream = crate::fx::effectcraft::catalog();
    if let Some(entries) = upstream["entries"].as_array_mut() {
        entries.retain(&matches);
        let total = entries.len();
        *entries = entries
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .map(|mut v| {
                if !details {
                    v["parameter_count"] = json!(v["parameters"].as_array().map_or(0, Vec::len));
                    if let Some(o) = v.as_object_mut() {
                        o.remove("parameters");
                        o.remove("upstream_parameters");
                    }
                }
                v
            })
            .collect();
        upstream["matching_count"] = json!(total);
        upstream["next_offset"] = if offset.saturating_add(limit) < total {
            json!(offset.saturating_add(limit))
        } else {
            Value::Null
        };
    }
    let native: Vec<Value> = crate::fx::registered().iter().filter(|e| !e.type_name().starts_with("ec.")).map(|e| json!({
        "id":e.type_name(),"doc":e.doc(),"status":"connected","parameters":e.params(),"intrinsic_time_dependence":e.time_dependent()
    })).filter(matches).map(|mut v| {
        if !details { v["parameter_count"] = json!(v["parameters"].as_array().map_or(0,Vec::len)); if let Some(o)=v.as_object_mut(){o.remove("parameters");} }
        v
    }).collect();
    Ok(
        json!({"query":query,"offset":offset,"limit":limit,"details":details,"native":native,"effectcraft":upstream}),
    )
}
