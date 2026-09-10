//! TOON (Token-Oriented Object Notation) encoder — spec v4.1.
//!
//! <https://github.com/toon-format/spec>
//!
//! Encodes the JSON data model (serde_json::Value) in the four TOON forms:
//! inline primitive arrays, tabular arrays (with nested field groups),
//! keyed tabular objects, and list form — plus indentation-based objects.
//! One document delimiter is chosen up front (comma by default) by
//! scanning string values for the delimiter that minimizes quoting.
//!
//! Decoder-side rules are out of scope: this module is an encoder only.

use serde_json::{Map, Number, Value};

const INDENT: &str = "  ";

/// Encode a JSON value as a TOON document (no trailing newline).
pub fn encode(value: &Value) -> String {
    let delim = choose_delim(value);
    let mut out = String::new();
    write_root(value, delim, &mut out);
    // Spec §12: no trailing newline at the end of the document.
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

// ---------------------------------------------------------------------------
// Delimiter selection
// ---------------------------------------------------------------------------

fn choose_delim(value: &Value) -> char {
    // Count strings containing each candidate and total occurrences; pick
    // the delimiter that quotes the fewest strings (tie: fewer total
    // occurrences, then the spec default `,`).
    let mut hits = [0usize; 3]; // strings containing the delim
    let mut total = [0usize; 3]; // total occurrences
    scan_strings(value, &mut |s: &str| {
        // Candidate order is also the tie-break preference: comma (spec
        // default), then pipe, then tab.
        for (i, d) in [',', '|', '\t'].iter().enumerate() {
            let n = s.matches(*d).count();
            if n > 0 {
                hits[i] += 1;
                total[i] += n;
            }
        }
    });
    let best = (0..3).min_by(|a, b| (hits[*a], total[*a], *b).cmp(&(hits[*b], total[*b], *b)));
    [',', '|', '\t'][best.unwrap_or(0)]
}

fn scan_strings(value: &Value, f: &mut impl FnMut(&str)) {
    match value {
        Value::String(s) => f(s),
        Value::Array(a) => a.iter().for_each(|v| scan_strings(v, f)),
        Value::Object(o) => o.iter().for_each(|(_, v)| scan_strings(v, f)),
        _ => {}
    }
}

fn delim_str(d: char) -> &'static str {
    match d {
        '\t' => "\t",
        '|' => "|",
        _ => ",",
    }
}

/// Bracket-segment delimiter symbol: empty for comma (the default).
fn delim_sym(d: char) -> &'static str {
    match d {
        '\t' => "\t",
        '|' => "|",
        _ => "",
    }
}

// ---------------------------------------------------------------------------
// Column shapes (tabular detection, spec §9.3)
// ---------------------------------------------------------------------------

enum FieldShape {
    Leaf,
    Group(Vec<FieldEntry>),
}

struct FieldEntry {
    name: String,
    shape: FieldShape,
}

/// Field list for a set of objects with identical key sets where every
/// column is uniform-primitive or nested-uniform. `None` disqualifies
/// tabular/keyed-tabular form for this set.
fn fields_for_objects(objs: &[&Map<String, Value>]) -> Option<Vec<FieldEntry>> {
    let first = objs.first()?;
    if first.is_empty() {
        return None;
    }
    let keys: Vec<&String> = first.keys().collect();
    let mut entries = Vec::with_capacity(keys.len());
    for k in keys {
        // Column values across all objects (same key set is guaranteed by
        // the caller via same_key_set).
        let col: Option<Vec<&Value>> = objs.iter().map(|o| o.get(k)).collect();
        let col = col?;
        match &col[0] {
            Value::Object(m) if !m.is_empty() => {
                // nested-uniform: all non-empty objects, same key set,
                // recursively valid columns.
                let sub: Option<Vec<&Map<String, Value>>> = col
                    .iter()
                    .map(|v| v.as_object().filter(|m| !m.is_empty()))
                    .collect();
                let sub = sub?;
                if !same_key_set(&sub) {
                    return None;
                }
                entries.push(FieldEntry {
                    name: k.clone(),
                    shape: FieldShape::Group(fields_for_objects(&sub)?),
                });
            }
            // Empty object or array column: neither shape — disqualify.
            Value::Object(_) | Value::Array(_) => return None,
            _ => {
                // uniform-primitive: every value must be a primitive.
                if col.iter().any(|v| matches!(v, Value::Object(_) | Value::Array(_))) {
                    return None;
                }
                entries.push(FieldEntry { name: k.clone(), shape: FieldShape::Leaf });
            }
        }
    }
    Some(entries)
}

fn same_key_set(objs: &[&Map<String, Value>]) -> bool {
    let first: std::collections::BTreeSet<&String> = objs[0].keys().collect();
    objs[1..].iter().all(|o| {
        let ks: std::collections::BTreeSet<&String> = o.keys().collect();
        ks == first
    })
}

/// Depth-first pre-order leaf count of a field list.
fn leaf_count(fields: &[FieldEntry]) -> usize {
    fields
        .iter()
        .map(|f| match &f.shape {
            FieldShape::Leaf => 1,
            FieldShape::Group(sub) => leaf_count(sub),
        })
        .sum()
}

/// Serialize a field list into `{a,b{c,d}}` form.
fn fields_str(fields: &[FieldEntry], delim: char) -> String {
    let d = delim_str(delim);
    fields
        .iter()
        .map(|f| match &f.shape {
            FieldShape::Leaf => encode_key(&f.name),
            FieldShape::Group(sub) => {
                format!("{}{{{}}}", encode_key(&f.name), fields_str(sub, delim))
            }
        })
        .collect::<Vec<_>>()
        .join(d)
}

/// Leaf values of an object in depth-first pre-order of `fields`.
fn leaf_cells(obj: &Map<String, Value>, fields: &[FieldEntry], delim: char) -> Vec<String> {
    let mut cells = Vec::with_capacity(leaf_count(fields));
    push_leaves(obj, fields, delim, &mut cells);
    cells
}

fn push_leaves(obj: &Map<String, Value>, fields: &[FieldEntry], delim: char, cells: &mut Vec<String>) {
    for f in fields {
        match &f.shape {
            FieldShape::Leaf => {
                cells.push(match obj.get(&f.name) {
                    // Cells are delimiter-aware quoted per §11.1.
                    Some(v) => encode_primitive(v, delim),
                    None => String::new(),
                });
            }
            FieldShape::Group(sub) => {
                if let Some(m) = obj.get(&f.name).and_then(|v| v.as_object()) {
                    push_leaves(m, sub, delim, cells);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Primitives, strings, keys (spec §2, §7)
// ---------------------------------------------------------------------------

fn encode_cell(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => encode_number(n),
        Value::String(s) => s.clone(),
        // Objects/arrays never appear as tabular leaves (detection excludes
        // them); this arm is defensive only.
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn encode_number(n: &Number) -> String {
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    let f = n.as_f64().unwrap_or(0.0);
    if f == 0.0 {
        return "0".to_string(); // -0 normalizes to 0
    }
    // Rust's shortest-round-trip Display: plain decimal, no trailing zeros,
    // integer-valued floats render without ".0" — the canonical §2 form.
    format!("{f}")
}

/// §7.2: does this string value need quotes in a `delim`-active context?
fn needs_quote(s: &str, delim: char) -> bool {
    s.is_empty()
        || s.starts_with(' ') || s.ends_with(' ')
        || s.starts_with('\t') || s.ends_with('\t')
        || s == "true" || s == "false" || s == "null"
        || is_numeric_like(s)
        || s.contains(':') || s.contains('"') || s.contains('\\')
        || s.contains('[') || s.contains(']') || s.contains('{') || s.contains('}')
        || s.chars().any(|c| (c as u32) < 0x20)
        || s.contains(delim)
        || s.starts_with('-')
        || s.starts_with('#')
}

/// `/^[+-]?[0-9]+(?:\.[0-9]+)?(?:e[+-]?[0-9]+)?$/i` — full match, ASCII digits.
fn is_numeric_like(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let d0 = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == d0 {
        return false;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let f0 = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == f0 {
            return false;
        }
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let e0 = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == e0 {
            return false;
        }
    }
    i == b.len()
}

/// §7.1: escape a quoted string.
fn escape_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn encode_value_string(s: &str, delim: char) -> String {
    if needs_quote(s, delim) {
        format!("\"{}\"", escape_quoted(s))
    } else {
        s.to_string()
    }
}

/// §7.3: unquoted iff `^[A-Za-z_][A-Za-z0-9_.]*$`.
fn is_plain_key(k: &str) -> bool {
    let mut chars = k.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

fn encode_key(k: &str) -> String {
    if is_plain_key(k) {
        k.to_string()
    } else {
        format!("\"{}\"", escape_quoted(k))
    }
}

// ---------------------------------------------------------------------------
// Document encoding (spec §5, §8–§10)
// ---------------------------------------------------------------------------

fn write_root(value: &Value, delim: char, out: &mut String) {
    match value {
        // §8: an empty object at the root yields an empty document.
        Value::Object(o) if o.is_empty() => {}
        Value::Object(o) => {
            if let Some(fields) = keyed_fields(o) {
                // Root keyed tabular: `[N:]{fields}:` (§9.5).
                out.push_str(&format!(
                    "[{}:{}]{{{}}}:\n",
                    o.len(),
                    delim_sym(delim),
                    fields_str(&fields, delim)
                ));
                for (k, v) in o {
                    let cells = leaf_cells(v.as_object().unwrap(), &fields, delim);
                    out.push_str(&format!("{}: {}\n", encode_key(k), cells.join(delim_str(delim))));
                }
            } else {
                write_object_fields(o, 0, delim, out);
            }
        }
        Value::Array(a) if a.is_empty() => out.push_str("[]\n"),
        Value::Array(a) => write_array(a, &Pos::Root, 0, delim, out),
        // Root primitive: a single line (§5).
        Value::String(s) => out.push_str(&format!("{}\n", encode_value_string(s, delim))),
        other => out.push_str(&format!("{}\n", encode_cell(other))),
    }
}

/// Where a value sits in the document: the root, an object field
/// (`key[N]:`), a list item after a `- ` marker (§9.2, §9.4), or the first
/// field of a list-item object carried on its hyphen line (`- key[N]:`, §10).
enum Pos {
    Root,
    Field(String),
    ListItem,
    HyphenField(String),
}

/// §8: object fields at `depth` (object already failed keyed-tabular
/// detection, or is nested inside a keyed structure's non-column value).
fn write_object_fields(o: &Map<String, Value>, depth: usize, delim: char, out: &mut String) {
    let pad = INDENT.repeat(depth);
    for (k, v) in o {
        let key = encode_key(k);
        match v {
            Value::Object(m) => {
                if let Some(fields) = keyed_fields(m) {
                    write_keyed_tabular(m, &key, &fields, depth, delim, out);
                } else {
                    out.push_str(&format!("{pad}{key}:\n"));
                    if !m.is_empty() {
                        write_object_fields(m, depth + 1, delim, out);
                    }
                }
            }
            Value::Array(a) => write_array(a, &Pos::Field(key.clone()), depth, delim, out),
            primitive => {
                out.push_str(&format!("{pad}{key}: {}\n", encode_primitive(primitive, delim)))
            }
        }
    }
}

fn encode_primitive(v: &Value, delim: char) -> String {
    match v {
        Value::String(s) => encode_value_string(s, delim),
        other => encode_cell(other),
    }
}

/// §9.5 keyed tabular detection: ≥2 entries, all non-empty uniform objects.
fn keyed_fields(o: &Map<String, Value>) -> Option<Vec<FieldEntry>> {
    if o.len() < 2 {
        return None;
    }
    let vals: Option<Vec<&Map<String, Value>>> = o
        .values()
        .map(|v| v.as_object().filter(|m| !m.is_empty()))
        .collect();
    let vals = vals?;
    if !same_key_set(&vals) {
        return None;
    }
    fields_for_objects(&vals)
}

fn write_keyed_tabular(
    m: &Map<String, Value>,
    key: &str,
    fields: &[FieldEntry],
    depth: usize,
    delim: char,
    out: &mut String,
) {
    let pad = INDENT.repeat(depth);
    out.push_str(&format!(
        "{pad}{key}[{}:{}]{{{}}}:\n",
        m.len(),
        delim_sym(delim),
        fields_str(fields, delim)
    ));
    let row_pad = INDENT.repeat(depth + 1);
    for (k, v) in m {
        let cells = leaf_cells(v.as_object().unwrap(), fields, delim);
        out.push_str(&format!("{row_pad}{}: {}\n", encode_key(k), cells.join(delim_str(delim))));
    }
}

/// Arrays (§9.1–§9.4) in object-field position (`Field`), list-item
/// position (`ListItem`, header prefixed `- `), or the document root
/// (`Root`, keyless header).
fn write_array(a: &[Value], pos: &Pos, depth: usize, delim: char, out: &mut String) {
    let pad = INDENT.repeat(depth);
    if a.is_empty() {
        match pos {
            // Field position: `key: []` (§9.1). An empty array first field
            // on a hyphen line spells `- key: []` the same way (§10).
            Pos::Field(k) => out.push_str(&format!("{pad}{k}: []\n")),
            Pos::HyphenField(k) => out.push_str(&format!("{pad}- {k}: []\n")),
            // Root: handled in write_root (`[]`).
            Pos::Root => out.push_str(&format!("{pad}[]\n")),
            // List-item: encoders MUST NOT emit `- []` (§9.2).
            Pos::ListItem => out.push_str(&format!("{pad}- [0{}]:\n", delim_sym(delim))),
        }
        return;
    }
    // Header text for this array's own line: `key[`, `- [` or bare `[`.
    // Scope content sits one level under the header — except a hyphen-line
    // field's scope, which the §10 depth model places at depth+2 (the
    // hyphen line itself stands at depth+1 for scope purposes).
    let content_depth = match pos {
        Pos::HyphenField(_) => depth + 2,
        _ => depth + 1,
    };
    let head = |out: &mut String, extra: &str| match pos {
        Pos::Field(k) => out.push_str(&format!("{pad}{k}{extra}")),
        Pos::HyphenField(k) => out.push_str(&format!("{pad}- {k}{extra}")),
        Pos::Root => out.push_str(&format!("{pad}{extra}")),
        Pos::ListItem => out.push_str(&format!("{pad}- {extra}")),
    };
    // Uniform tabular (§9.3) — only in field or root position: a keyless
    // fields-bearing header is invalid except at root (§6).
    if let Some(fields) = array_fields(a) {
        if matches!(pos, Pos::Field(_) | Pos::Root) {
            head(out, &format!("[{}{}]{{{}}}:\n", a.len(), delim_sym(delim), fields_str(&fields, delim)));
            let row_pad = INDENT.repeat(content_depth);
            for item in a {
                let cells = leaf_cells(item.as_object().unwrap(), &fields, delim);
                out.push_str(&format!("{row_pad}{}\n", cells.join(delim_str(delim))));
            }
            return;
        }
    }
    // Inline primitive array (§9.1).
    if a.iter().all(|v| !matches!(v, Value::Object(_) | Value::Array(_))) {
        let vals: Vec<String> = a.iter().map(|v| encode_primitive(v, delim)).collect();
        head(out, &format!("[{}{}]: {}\n", a.len(), delim_sym(delim), vals.join(delim_str(delim))));
        return;
    }
    // Arrays of primitive arrays (§9.2): list form.
    if a.iter().all(|v| {
        matches!(v, Value::Array(arr) if arr.iter().all(|x| !matches!(x, Value::Object(_) | Value::Array(_))))
    }) {
        head(out, &format!("[{}{}]:\n", a.len(), delim_sym(delim)));
        let item_pad = INDENT.repeat(content_depth);
        for v in a {
            let arr = v.as_array().unwrap();
            if arr.is_empty() {
                out.push_str(&format!("{item_pad}- [0{}]:\n", delim_sym(delim)));
                continue;
            }
            let vals: Vec<String> = arr.iter().map(|x| encode_primitive(x, delim)).collect();
            out.push_str(&format!(
                "{item_pad}- [{}{}]: {}\n",
                arr.len(),
                delim_sym(delim),
                vals.join(delim_str(delim))
            ));
        }
        return;
    }
    // Mixed / non-uniform (§9.4): list form.
    head(out, &format!("[{}{}]:\n", a.len(), delim_sym(delim)));
    let item_pad = INDENT.repeat(content_depth);
    for v in a {
        match v {
            Value::Object(m) => write_list_item_object(m, content_depth, delim, out),
            Value::Array(arr) => write_array(arr, &Pos::ListItem, content_depth, delim, out),
            primitive => out.push_str(&format!("{item_pad}- {}\n", encode_primitive(primitive, delim))),
        }
    }
}

/// §9.3 tabular detection for an array.
fn array_fields(a: &[Value]) -> Option<Vec<FieldEntry>> {
    let objs: Option<Vec<&Map<String, Value>>> = a
        .iter()
        .map(|v| v.as_object().filter(|m| !m.is_empty()))
        .collect();
    let objs = objs?;
    if !same_key_set(&objs) {
        return None;
    }
    fields_for_objects(&objs)
}

/// §10: object as a list item, hyphen line at `depth`.
fn write_list_item_object(m: &Map<String, Value>, depth: usize, delim: char, out: &mut String) {
    let pad = INDENT.repeat(depth);
    if m.is_empty() {
        out.push_str(&format!("{pad}-\n"));
        return;
    }
    let mut it = m.iter();
    let (fk, fv) = it.next().unwrap();
    let fkey = encode_key(fk);
    let head = format!("{pad}- ");
    // First field is a tabular array or keyed tabular object (§10): its
    // header rides the hyphen line, rows/entries land at depth+2, and the
    // remaining fields go at depth+1.
    let tabular_first = match fv {
        Value::Array(arr) => array_fields(arr),
        Value::Object(o) => keyed_fields(o),
        _ => None,
    };
    if let Some(fields) = tabular_first {
        match fv {
            Value::Array(arr) => {
                out.push_str(&format!(
                    "{head}{fkey}[{}{}]{{{}}}:\n",
                    arr.len(),
                    delim_sym(delim),
                    fields_str(&fields, delim)
                ));
                let row_pad = INDENT.repeat(depth + 2);
                for item in arr {
                    let cells = leaf_cells(item.as_object().unwrap(), &fields, delim);
                    out.push_str(&format!("{row_pad}{}\n", cells.join(delim_str(delim))));
                }
            }
            Value::Object(o) => {
                out.push_str(&format!(
                    "{head}{fkey}[{}:{}]{{{}}}:\n",
                    o.len(),
                    delim_sym(delim),
                    fields_str(&fields, delim)
                ));
                let row_pad = INDENT.repeat(depth + 2);
                for (k, v) in o {
                    let cells = leaf_cells(v.as_object().unwrap(), &fields, delim);
                    out.push_str(&format!("{row_pad}{}: {}\n", encode_key(k), cells.join(delim_str(delim))));
                }
            }
            _ => unreachable!(),
        }
        write_rest(&mut it, depth, delim, out);
        return;
    }
    // Otherwise the first field rides the hyphen line (§10). A keyless
    // header never appears here, so nested arrays use the `- key` prefix
    // with their items at depth+1 (the hyphen line's depth-model position).
    match fv {
        Value::Object(o) => {
            out.push_str(&format!("{head}{fkey}:\n"));
            if !o.is_empty() {
                // §10 depth model: the scope opened by the hyphen-line field
                // has its content at depth+2.
                write_object_fields(o, depth + 2, delim, out);
            }
            write_rest(&mut it, depth, delim, out);
        }
        Value::Array(arr) => {
            write_array(arr, &Pos::HyphenField(fkey.clone()), depth, delim, out);
            write_rest(&mut it, depth, delim, out);
        }
        primitive => {
            out.push_str(&format!("{head}{fkey}: {}\n", encode_primitive(primitive, delim)));
            write_rest(&mut it, depth, delim, out);
        }
    }
}

/// Emit the remaining fields of a list-item object at depth+1 (§10).
fn write_rest<'a>(
    it: &mut serde_json::map::Iter<'a>,
    depth: usize,
    delim: char,
    out: &mut String,
) {
    let rest: Map<String, Value> = it.map(|(k, v)| (k.clone(), v.clone())).collect();
    if !rest.is_empty() {
        write_object_fields(&rest, depth + 1, delim, out);
    }
}


// ---------------------------------------------------------------------------
// Tests — expected outputs are the spec's Appendix A examples (v4.1).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn enc(v: Value) -> String {
        encode(&v)
    }

    #[test]
    fn flat_object() {
        assert_eq!(
            enc(json!({"id": 123, "name": "Ada", "active": true})),
            "id: 123\nname: Ada\nactive: true"
        );
    }

    #[test]
    fn nested_object() {
        assert_eq!(
            enc(json!({"user": {"id": 123, "name": "Ada"}})),
            "user:\n  id: 123\n  name: Ada"
        );
    }

    #[test]
    fn inline_primitive_array() {
        assert_eq!(enc(json!({"tags": ["admin", "ops", "dev"]})), "tags[3]: admin,ops,dev");
    }

    #[test]
    fn inline_array_with_null_and_bool() {
        // Primitives in inline arrays must stay bare (not string-quoted).
        assert_eq!(enc(json!({"v": [null, true, false, 1]})), "v[4]: null,true,false,1");
    }

    #[test]
    fn array_of_primitive_arrays() {
        assert_eq!(
            enc(json!({"pairs": [[1, 2], [3, 4]]})),
            "pairs[2]:\n  - [2]: 1,2\n  - [2]: 3,4"
        );
    }

    #[test]
    fn tabular_array() {
        assert_eq!(
            enc(json!({
                "items": [
                    {"sku": "A1", "qty": 2, "price": 9.99},
                    {"sku": "B2", "qty": 1, "price": 14.5}
                ]
            })),
            "items[2]{sku,qty,price}:\n  A1,2,9.99\n  B2,1,14.5"
        );
    }

    #[test]
    fn tabular_nested_field_groups() {
        assert_eq!(
            enc(json!({
                "orders": [
                    {"id": 1, "customer": {"name": "Ada", "country": "DK"}, "total": 99},
                    {"id": 2, "customer": {"name": "Bob", "country": "UK"}, "total": 149}
                ]
            })),
            "orders[2]{id,customer{name,country},total}:\n  1,Ada,DK,99\n  2,Bob,UK,149"
        );
    }

    #[test]
    fn keyed_tabular() {
        assert_eq!(
            enc(json!({
                "users": {
                    "alice": {"age": 30, "city": "Berlin"},
                    "bob": {"age": 25, "city": "Oslo"}
                }
            })),
            "users[2:]{age,city}:\n  alice: 30,Berlin\n  bob: 25,Oslo"
        );
    }

    #[test]
    fn root_keyed_tabular() {
        assert_eq!(
            enc(json!({
                "alice": {"age": 30, "city": "Berlin"},
                "bob": {"age": 25, "city": "Oslo"}
            })),
            "[2:]{age,city}:\nalice: 30,Berlin\nbob: 25,Oslo"
        );
    }

    #[test]
    fn mixed_array_list_form() {
        assert_eq!(
            enc(json!({"items": [1, {"a": 1}, "text"]})),
            "items[3]:\n  - 1\n  - a: 1\n  - text"
        );
    }

    #[test]
    fn list_item_objects_first_field_on_hyphen() {
        assert_eq!(
            enc(json!({
                "items": [
                    {"id": 1, "name": "First"},
                    {"id": 2, "name": "Second", "extra": true}
                ]
            })),
            "items[2]:\n  - id: 1\n    name: First\n  - id: 2\n    name: Second\n    extra: true"
        );
    }

    #[test]
    fn nested_tabular_inside_list_item() {
        assert_eq!(
            enc(json!({
                "items": [
                    {
                        "users": [
                            {"id": 1, "name": "Ada"},
                            {"id": 2, "name": "Bob"}
                        ],
                        "status": "active"
                    }
                ]
            })),
            "items[1]:\n  - users[2]{id,name}:\n      1,Ada\n      2,Bob\n    status: active"
        );
    }

    #[test]
    fn quoted_colons_in_cells() {
        assert_eq!(
            enc(json!({
                "links": [
                    {"id": 1, "url": "http://a:b"},
                    {"id": 2, "url": "https://example.com?q=a:b"}
                ]
            })),
            "links[2]{id,url}:\n  1,\"http://a:b\"\n  2,\"https://example.com?q=a:b\""
        );
    }

    #[test]
    fn quoted_keys() {
        assert_eq!(enc(json!({"my-key": [1, 2, 3]})), "\"my-key\"[3]: 1,2,3");
        assert_eq!(
            enc(json!({"x-items": [{"id": 1, "name": "Ada"}, {"id": 2, "name": "Bob"}]})),
            "\"x-items\"[2]{id,name}:\n  1,Ada\n  2,Bob"
        );
        // Non-uniform under a quoted key: list form with first field on hyphen.
        assert_eq!(
            enc(json!({"x-items": [{"id": 1}, {"id": 2, "label": "archived"}]})),
            "\"x-items\"[2]:\n  - id: 1\n  - id: 2\n    label: archived"
        );
    }

    #[test]
    fn edge_cases() {
        assert_eq!(enc(json!({"name": ""})), "name: \"\"");
        assert_eq!(enc(json!({"tags": []})), "tags: []");
        assert_eq!(enc(json!({"version": "123"})), "version: \"123\"");
        assert_eq!(enc(json!({"enabled": "true"})), "enabled: \"true\"");
        assert_eq!(
            enc(json!({
                "root": {"level1": {"level2": {"level3": {
                    "items": [{"id": 1, "val": "a"}, {"id": 2, "val": "b"}]
                }}}}
            })),
            "root:\n  level1:\n    level2:\n      level3:\n        items[2]{id,val}:\n          1,a\n          2,b"
        );
        assert_eq!(enc(json!({"message": "Hello 世界 👋"})), "message: Hello 世界 👋");
        assert_eq!(enc(json!({"tags": ["🎉", "🎊", "🎈"]})), "tags[3]: 🎉,🎊,🎈");
        assert_eq!(
            enc(json!({"bignum": 9007199254740992u64, "decimal": 0.3333333333333333})),
            "bignum: 9007199254740992\ndecimal: 0.3333333333333333"
        );
    }

    #[test]
    fn root_forms() {
        // Root tabular array: keyless header.
        assert_eq!(
            enc(json!([{"sku": "A1"}, {"sku": "B2"}])),
            "[2]{sku}:\n  A1\n  B2"
        );
        // Root inline array.
        assert_eq!(enc(json!([1, 2, 3])), "[3]: 1,2,3");
        // Root array of primitive arrays.
        assert_eq!(enc(json!([[1, 2], [3, 4]])), "[2]:\n  - [2]: 1,2\n  - [2]: 3,4");
        // Root mixed array: keyless list-form header.
        assert_eq!(enc(json!([1, {"a": 1}, "text"])), "[3]:\n  - 1\n  - a: 1\n  - text");
        // Empty roots.
        assert_eq!(enc(json!({})), "");
        assert_eq!(enc(json!([])), "[]");
        // Root primitives.
        assert_eq!(enc(json!("hello")), "hello");
        assert_eq!(enc(json!(42)), "42");
        assert_eq!(enc(json!(true)), "true");
        // A root string that must stay a string.
        assert_eq!(enc(json!("123")), "\"123\"");
    }

    #[test]
    fn empty_and_special_objects() {
        // Empty object field.
        assert_eq!(enc(json!({"a": {}})), "a:");
        // Empty object as a list item: bare hyphen.
        assert_eq!(enc(json!({"items": [{}, {"a": 1}]})), "items[2]:\n  -\n  - a: 1");
        // Arrays containing an empty object never take tabular form.
        assert_eq!(
            enc(json!({"items": [{"a": 1}, {}]})),
            "items[2]:\n  - a: 1\n  -"
        );
        // Single-entry object: nests (keyed needs ≥2 entries).
        assert_eq!(enc(json!({"a": {"x": {"y": 1}}})), "a:\n  x:\n    y: 1");
        // Empty array in list-item position: `- [0]:` (never `- []`).
        assert_eq!(enc(json!({"items": [[], [1]]})), "items[2]:\n  - [0]:\n  - [1]: 1");
        // Null / bool / number fields.
        assert_eq!(enc(json!({"n": null, "b": false, "i": -5, "f": 1.5})),
            "n: null\nb: false\ni: -5\nf: 1.5");
        // -0 normalizes to 0.
        assert_eq!(enc(json!({"z": -0.0})), "z: 0");
    }

    #[test]
    fn delimiter_selection() {
        // Commas in values with no pipes: pipe wins, so values stay unquoted.
        assert_eq!(
            enc(json!({"tags": ["a, b", "c"]})),
            "tags[2|]: a, b|c"
        );
        // Commas and pipes both present: tab affects zero strings and wins.
        assert_eq!(
            enc(json!({"a": ["x,y", "u,v"], "b": ["p|q"]})),
            "a[2\t]: x,y\tu,v\nb[1\t]: p|q"
        );
        // Tab delimiter when commas and pipes both appear.
        assert_eq!(
            enc(json!({"t": ["a,b", "c|d"]})),
            "t[2\t]: a,b\tc|d"
        );
    }

    #[test]
    fn hyphen_and_hash_strings_quoted() {
        assert_eq!(enc(json!({"s": "-x"})), "s: \"-x\"");
        assert_eq!(enc(json!({"s": "#tag"})), "s: \"#tag\"");
        assert_eq!(enc(json!({"s": "-"})), "s: \"-\"");
        // Internal hyphens are fine unquoted.
        assert_eq!(enc(json!({"s": "well-known"})), "s: well-known");
    }

    #[test]
    fn escaping() {
        assert_eq!(
            enc(json!({"s": "line\nbreak\ttab \"quote\" back\\slash"})),
            "s: \"line\\nbreak\\ttab \\\"quote\\\" back\\\\slash\""
        );
        // Control characters -> \uXXXX.
        assert_eq!(enc(json!({"s": "a\u{0001}b"})), "s: \"a\\u0001b\"");
    }

    #[test]
    fn numeric_like_detection() {
        for s in ["42", "-3.14", "05", "+1", "1e-6", "1E+9", "-0.5"] {
            assert!(is_numeric_like(s), "{s:?} should be numeric-like");
        }
        for s in ["", ".5", "1.", "abc", "1_000", "0x10", "1e", "12a", "--1", "1..2"] {
            assert!(!is_numeric_like(s), "{s:?} should not be numeric-like");
        }
    }

    #[test]
    fn key_rules() {
        assert!(is_plain_key("a"));
        assert!(is_plain_key("_x"));
        assert!(is_plain_key("A1.b_2"));
        assert!(!is_plain_key(""));
        assert!(!is_plain_key("1a"));
        assert!(!is_plain_key("my-key"));
        assert!(!is_plain_key("a b"));
        assert!(!is_plain_key("a:b"));
    }

    #[test]
    fn no_trailing_newline() {
        let s = enc(json!({"a": 1}));
        assert!(!s.ends_with('\n'));
    }

    #[test]
    fn non_uniform_column_disqualifies_tabular() {
        // `v` mixes null and object -> list form.
        assert_eq!(
            enc(json!({"items": [
                {"v": null},
                {"v": {"x": 1}}
            ]})),
            "items[2]:\n  - v: null\n  - v:\n      x: 1"
        );
        // Array-valued column -> list form.
        assert_eq!(
            enc(json!({"items": [
                {"v": [1]},
                {"v": [2]}
            ]})),
            "items[2]:\n  - v[1]: 1\n  - v[1]: 2"
        );
    }

    #[test]
    fn hyphen_line_nested_array_first_field() {
        // §10: non-tabular array as first field rides the hyphen line; its
        // items sit at depth+2 relative to the hyphen line's depth.
        assert_eq!(
            enc(json!({"items": [
                {"v": [1, {"x": 2}], "s": "ok"}
            ]})),
            "items[1]:\n  - v[2]:\n      - 1\n      - x: 2\n    s: ok"
        );
    }
}

