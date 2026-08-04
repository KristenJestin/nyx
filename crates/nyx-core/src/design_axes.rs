//! THE VARIANCE-AXES MODEL of a catalogue component (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`), in ONE
//! place: how a stored `design_components.states_json` is READ, and how a `data-nyx-if` condition is
//! PARSED.
//!
//! # Why it is its own module (finding `01KYSCZ822K4ZFAAYTG6GT6YWZ`)
//!
//! It used to live inside [`crate::mcp_tools_design`], where the WRITE-path diagnostics
//! ([`crate::mcp_tools_design::variance_warnings`]) are the only thing that read it. Then the
//! RENDERER needed it too: resolving a REFERENCED sub-component ([`crate::render::reference`]) means
//! resolving ITS axes to concrete classes in the sidecar, and a renderer that reached back into the
//! agent-tools module would make the two mutually dependent. A SECOND reader was the other option,
//! and this codebase has already paid for one (finding `01KYSNPWWG9DA0AXP6D22GV5BK`: two readers of
//! the map form disagreed about which axis was primary). So the model moved DOWN, to a leaf both
//! sides depend on, and there is still exactly ONE reading of a stored row.
//!
//! What it carries beyond the diagnostics' old view: each value's CONCRETE `classes` string (the
//! diagnostics only ever asked whether there were any) and the axis's declared `default`. Those are
//! what an expansion APPLIES, and they are facts of the same stored blob — so they are read here,
//! once, rather than re-derived by whoever needs them.

use serde_json::{json, Value};

/// ONE VALUE of a variance axis: its name and the CONCRETE classes the shim applies for it (the
/// resolved CVA string the synthesis inlined — `""` when the value deliberately carries none, which
/// is what a BASELINE looks like).
#[derive(Debug, Clone)]
pub(crate) struct AxisValueSpec {
    pub(crate) name: String,
    /// The value's resolved classes, space-joined (the `classes` / `class` key, string or array).
    pub(crate) classes: String,
}

impl AxisValueSpec {
    /// Did the synthesis give this value the CONCRETE classes the shim would apply for it? The
    /// question the inertia diagnostics ask — now derived from the classes themselves, so the two
    /// can never disagree.
    pub(crate) fn has_classes(&self) -> bool {
        !self.classes.trim().is_empty()
    }
}

/// ONE NAMED AXIS of variance (finding `01KYQ0R84KVF6QVQXQ9YVG9Z8T`) — `variant`, `size`, or a
/// two-valued `loading`. `legacy` marks the ANONYMOUS axis a flat `states_json` is read as.
#[derive(Debug, Clone)]
pub(crate) struct AxisSpec {
    pub(crate) name: String,
    pub(crate) values: Vec<AxisValueSpec>,
    /// The axis's declared `default` value, when it names one of its own values. `None` otherwise —
    /// including when the blob names a value the axis does not declare, which decides nothing.
    pub(crate) default: Option<String>,
    pub(crate) legacy: bool,
}

impl AxisSpec {
    /// The value this axis renders at when NOBODY picks one: its declared `default`, else its FIRST
    /// value — the same precedence the catalogue vignette applies ("each axis's `default`, else its
    /// first value"). An axis always has at least one value ([`build_axis`] refuses an empty one).
    pub(crate) fn default_value(&self) -> &str {
        self.default
            .as_deref()
            .unwrap_or_else(|| self.values[0].name.as_str())
    }

    /// This axis's value called `name`, if it declares one.
    pub(crate) fn value(&self, name: &str) -> Option<&AxisValueSpec> {
        self.values.iter().find(|v| v.name == name)
    }
}

/// Is this JSON entry an AXIS (it carries a `values` array) rather than a bare state NAME? That one
/// question is what tells the two array shapes apart without any flag.
fn is_axis_entry(entry: &Value) -> bool {
    entry.get("values").is_some_and(Value::is_array)
}

/// The `classes` (or its `class` alias) of one axis value, as a STRING — a bare string verbatim, an
/// array space-joined, anything else empty. One normalisation, so `has_classes` and the expansion
/// that APPLIES them read the very same characters.
fn value_classes(entry: &Value) -> String {
    match entry.get("classes").or_else(|| entry.get("class")) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn axis_value(entry: &Value) -> Option<AxisValueSpec> {
    match entry {
        Value::String(s) if !s.trim().is_empty() => Some(AxisValueSpec {
            name: s.clone(),
            classes: String::new(),
        }),
        other => {
            let name = other
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())?;
            Some(AxisValueSpec {
                name: name.to_string(),
                classes: value_classes(other),
            })
        }
    }
}

fn build_axis(
    name: &str,
    values: &[Value],
    default: Option<&str>,
    legacy: bool,
) -> Option<AxisSpec> {
    if name.trim().is_empty() {
        return None;
    }
    let mut out: Vec<AxisValueSpec> = Vec::new();
    for entry in values {
        if let Some(v) = axis_value(entry) {
            if !out.iter().any(|existing| existing.name == v.name) {
                out.push(v);
            }
        }
    }
    // A `default` naming a value the axis does not declare decides NOTHING — the first value keeps
    // the role, exactly as if none had been written.
    let default = default
        .filter(|d| out.iter().any(|v| v.name == *d))
        .map(str::to_string);
    (!out.is_empty()).then_some(AxisSpec {
        name: name.to_string(),
        values: out,
        default,
        legacy,
    })
}

/// The entries of a JSON OBJECT in DOCUMENT ORDER — the order the text declares them in, not the
/// order a map happens to iterate in.
///
/// WHY IT EXISTS (finding `01KYSNPWWG9DA0AXP6D22GV5BK`). `serde_json::Map` is a `BTreeMap` here —
/// the crate is built WITHOUT `preserve_order`, and no `indexmap` is in `Cargo.lock` — so
/// `map.iter()` hands the keys back ALPHABETICALLY, while the renderer's `Object.entries` hands the
/// very same object back in INSERTION order. That difference is invisible everywhere except on the
/// tolerated MAP form of `states_json`, where the FIRST axis is SPECIAL on both sides (it is the one
/// `data-nyx-state-case` drives, the one the chip-bar pilots): on `{ "variant": …, "loading": … }`
/// this validator judged `loading` primary while the panel piloted `variant`, so the two twins
/// disagreed about the same stored row.
///
/// Re-reading the raw text with a visitor that KEEPS the streamed order is what makes them agree,
/// and document order is the order EVERY OTHER accepted shape already preserves (the canonical
/// `{ "axes": [...] }`, the bare array, the legacy flat list) — sorting instead would have made the
/// primary axis depend on an alphabet rather than on what the agent wrote.
///
/// A REPEATED key resolves exactly as a JS object literal resolves it — the LAST value wins, at the
/// FIRST position — so even that pathology reads the same on both sides.
struct DocumentOrder(Vec<(String, Value)>);

impl<'de> serde::Deserialize<'de> for DocumentOrder {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct OrderedVisitor;
        impl<'de> serde::de::Visitor<'de> for OrderedVisitor {
            type Value = DocumentOrder;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut out: Vec<(String, Value)> = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, Value>()? {
                    match out.iter_mut().find(|(k, _)| *k == key) {
                        Some(slot) => slot.1 = value,
                        None => out.push((key, value)),
                    }
                }
                Ok(DocumentOrder(out))
            }
        }
        deserializer.deserialize_map(OrderedVisitor)
    }
}

/// The tolerated MAP form's axes, in DOCUMENT ORDER ([`DocumentOrder`]). The re-read cannot fail —
/// the very same text already parsed as an object — and if it somehow did, a blob that declares
/// nothing is this module's standing answer to anything it cannot read.
fn map_form_axes(raw: &str) -> Vec<(String, Vec<Value>)> {
    serde_json::from_str::<DocumentOrder>(raw)
        .map(|DocumentOrder(entries)| entries)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(key, value)| match value {
            Value::Array(items) => Some((key, items)),
            _ => None,
        })
        .collect()
}

/// The VARIANCE AXES a `states_json` blob declares, in every accepted shape (the Rust twin of the
/// renderer's `axes.ts` — the two must read a stored row the same way):
///
///   * `{ "axes": [{ name, default?, values: [{ name, classes }] }] }` — the canonical form;
///   * a bare ARRAY of axis objects (an entry with a `values` array IS an axis);
///   * the object MAP `{ "variant": ["primary", "ghost"] }`;
///   * and the LEGACY FLAT LIST `["default", …]` / `[{ "name": … }]`, read as ONE anonymous axis
///     named `state` whose values carry no classes — the retro-compatible reading of the rows every
///     existing catalogue is written in.
///
/// THE ORDER IS PART OF THE READING, in every shape: the FIRST axis is the one `data-nyx-state-case`
/// drives, so which axis comes first is a VERDICT, not a presentation detail. All four shapes are
/// read in DOCUMENT ORDER — for the map form that takes [`map_form_axes`], because a
/// `serde_json::Map` is sorted and the renderer's `Object.entries` is not
/// (finding `01KYSNPWWG9DA0AXP6D22GV5BK`).
///
/// A blob that is absent or malformed declares NOTHING: this is a diagnostic helper, never a
/// validator.
pub(crate) fn parse_axes(states_json: Option<&str>) -> Vec<AxisSpec> {
    let Some(raw) = states_json else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    // (name, values, declared default) — the default only exists on the two OBJECT-entry shapes.
    let raw_axes: Vec<(String, Vec<Value>, Option<String>)> = match &parsed {
        Value::Array(items) => {
            if !items.iter().any(is_axis_entry) {
                // The LEGACY flat list — one anonymous axis, class-less.
                return build_axis("state", items, None, true).into_iter().collect();
            }
            items.iter().filter_map(axis_entry).collect()
        }
        Value::Object(map) => match map.get("axes").and_then(Value::as_array) {
            Some(items) => items.iter().filter_map(axis_entry).collect(),
            // The tolerated MAP form `{ "variant": ["primary", "ghost"] }` — read from the RAW text,
            // because `map` is sorted and the renderer's `Object.entries` is not.
            None => map_form_axes(raw)
                .into_iter()
                .map(|(name, values)| (name, values, None))
                .collect(),
        },
        _ => return Vec::new(),
    };
    let mut out: Vec<AxisSpec> = Vec::new();
    for (name, values, default) in raw_axes {
        if let Some(axis) = build_axis(&name, &values, default.as_deref(), false) {
            if !out.iter().any(|a| a.name == axis.name) {
                out.push(axis);
            }
        }
    }
    out
}

/// One `{ name, values, default? }` axis entry of the canonical / bare-array shapes.
fn axis_entry(entry: &Value) -> Option<(String, Vec<Value>, Option<String>)> {
    let name = entry.get("name").and_then(Value::as_str)?;
    let values = entry.get("values").and_then(Value::as_array)?;
    let default = entry
        .get("default")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some((name.to_string(), values.clone(), default))
}

/// The MAP form of `states_json` rewritten to the CANONICAL `{ "axes": [...] }` form. Every other
/// shape — the canonical one, the bare array, the legacy flat list, a malformed blob, `None` — is
/// returned VERBATIM.
///
/// WHY AT THE WRITE (finding `01KYSNPWWG9DA0AXP6D22GV5BK`). The map form is the one accepted shape
/// whose MEANING depends on how a JSON object is iterated: it carries the axes as KEYS, and the
/// first axis is the one `data-nyx-state-case` drives. [`parse_axes`] and the renderer now agree on
/// document order, so the rows already stored are read the same way on both sides — but the ORDER OF
/// AN OBJECT'S KEYS IS NOT A GUARANTEE JSON MAKES, and a single re-serialization through a sorted
/// map (any `serde_json::Value` round-trip in the stack) would silently re-elect the primary axis.
/// Canonicalizing at the write moves that order into an ARRAY, where it is structural: no NEW row
/// can be exposed to it again, and only the legacy rows depend on the reader's care.
///
/// LOSSLESS for everything either reader looks at: each key becomes an axis NAME and its array
/// becomes that axis's `values` VERBATIM (classes included). A key whose value is not an array
/// declares no axis on either side, and is dropped with them.
pub(crate) fn canonical_states_json(states_json: Option<&str>) -> Option<String> {
    let raw = states_json?;
    let verbatim = || Some(raw.to_string());
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(raw) else {
        return verbatim();
    };
    // The same test `parse_axes` makes: an `axes` ARRAY is the canonical form, anything else is the
    // map form.
    if map.get("axes").and_then(Value::as_array).is_some() {
        return verbatim();
    }
    let axes: Vec<Value> = map_form_axes(raw)
        .into_iter()
        .map(|(name, values)| json!({ "name": name, "values": values }))
        .collect();
    if axes.is_empty() {
        return verbatim();
    }
    Some(json!({ "axes": axes }).to_string())
}

/// The ACTIVE combination of `axes` once `fixed` has had its say: ONE value per axis, in the axes'
/// own declaration order — the value the caller FIXED when the axis really declares it, else the
/// axis's own [`AxisSpec::default_value`].
///
/// It lives HERE, and not in whichever caller needed it first, because it is a READING of the stored
/// blob and this module is where a stored blob is read exactly once. Two callers apply it: the
/// expansion of a REFERENCED sub-component ([`crate::render::reference`]) and the RESOLUTION of the
/// component an agent just synthesized ([`crate::render::resolve_component`], finding
/// `01KYT0Z3RYGF3A881SDQN0X9D7`) — which is the one that REPORTS the combination back to the agent.
/// A second copy of the precedence would let the markup be resolved at one combination and described
/// as another, which is the exact class of defect this crate keeps paying for.
///
/// A `fixed` entry naming an axis that does not exist, or a value the axis does not declare, decides
/// NOTHING — it is silently ignored here (the caller is the one that can name it).
pub(crate) fn active_combination<'a>(
    axes: &'a [AxisSpec],
    fixed: &'a [(String, String)],
) -> Vec<(&'a str, &'a str)> {
    axes.iter()
        .map(|axis| {
            let want = fixed
                .iter()
                .find(|(name, _)| name == &axis.name)
                .map(|(_, value)| value.as_str())
                .filter(|value| axis.value(value).is_some());
            (
                axis.name.as_str(),
                want.unwrap_or_else(|| axis.default_value()),
            )
        })
        .collect()
}

/// The PRIMARY axis's value names — what `data-nyx-state-case` and the legacy chain speak.
pub(crate) fn primary_axis_states(axes: &[AxisSpec]) -> Vec<String> {
    axes.first()
        .map(|a| a.values.iter().map(|v| v.name.clone()).collect())
        .unwrap_or_default()
}

/// One clause of a `data-nyx-if` / `data-nyx-if-not` condition: an axis at one of N values.
pub(crate) struct TemplateCondition {
    pub(crate) axis: String,
    pub(crate) values: Vec<String>,
    pub(crate) negated: bool,
}

/// Read one attribute body — `"loading=on"`, `"variant=primary|ghost, size=lg"`.
pub(crate) fn parse_condition_body(body: &str, negated: bool) -> Vec<TemplateCondition> {
    body.split([',', ';'])
        .filter_map(|clause| {
            let (axis, values) = clause.split_once('=')?;
            let axis = axis.trim();
            let values: Vec<String> = values
                .split('|')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .collect();
            (!axis.is_empty() && !values.is_empty()).then(|| TemplateCondition {
                axis: axis.to_string(),
                values,
                negated,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two fields the RENDERER needs and the diagnostics never did: a value's CONCRETE classes
    /// (string OR array) and the axis's declared `default`. A `default` naming a value the axis does
    /// not declare is ignored — the first value keeps the role.
    #[test]
    fn axes_carry_their_resolved_classes_and_their_default() {
        let axes = parse_axes(Some(
            r#"{"axes":[
                 {"name":"variant","default":"ghost","values":[
                    {"name":"primary","classes":"bg-primary text-primary-foreground"},
                    {"name":"ghost","class":["border","border-input"]}]},
                 {"name":"size","default":"nope","values":[{"name":"md"},{"name":"lg","classes":"h-11"}]}]}"#,
        ));
        assert_eq!(axes.len(), 2);
        assert_eq!(axes[0].default_value(), "ghost");
        assert_eq!(
            axes[0].value("primary").map(AxisValueSpec::has_classes),
            Some(true)
        );
        assert_eq!(
            axes[0].value("ghost").map(|v| v.classes.as_str()),
            Some("border border-input"),
            "an array of classes is space-joined, once, here"
        );
        // A `default` the axis does not declare decides nothing: the FIRST value keeps the role.
        assert_eq!(axes[1].default_value(), "md");
        assert!(!axes[1].values[0].has_classes());
    }
}
