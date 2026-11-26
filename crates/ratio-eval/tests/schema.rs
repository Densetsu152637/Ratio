//! `json.schema::<T>()` (chapter 06, `data`) writes the canonical JSON Schema of chapter 03
//! ("Types are schemas"). The interpreter derives it from the IR's types; the checker derives
//! it from its own, and its tests (`ratio-check/tests/taint.rs`) pin these texts, so the two
//! must agree byte for byte.

mod support;

use support::{prepared, value};

const SOURCE: &str = r#"
enum Severity { low, medium, high }
enum Finding { Bug { line: Int, why: Str }, StyleNote { note: Str }, Clean }
enum Tree { Leaf { value: Int }, Node { kids: [Tree] } }
type Plain = { a: Int, b?: Str, c: Bool? }
type Report = {
  /// What the reviewer concluded.
  verdict: Bool,
  @len(0, 20) findings: [Finding],
  @range(0, 1.5) confidence: Float "How sure",
  @pattern(r"^[a-z]+$") tag: Str,
  @example("high") severity: Severity,
  counts: Map<Str, Int>,
  by_level: Map<Severity, [Str]>,
  span?: (Int, Int),
  raw: Json,
  took: Duration,
  cost: Money,
  nothing: (),
}
enum A_2 { L, N { k: [A_2] } }
enum A { L, N { k: [A] } }
type Both = { x: A_2, y: A }

fn plain() -> Json { json.schema::<Plain>() }
fn report() -> Json { json.schema::<Report>() }
fn tree() -> Json { json.schema::<Tree>() }
fn untrusted_text() -> Json { json.schema::<Untrusted<Str>>() }
fn secret_text() -> Json { json.schema::<Secret<Untrusted<Str>>>() }
fn results() -> Json { json.schema::<Result<[Int]>>() }
fn both() -> Json { json.schema::<Both>() }
fn keyed() -> Json { json.schema::<(Map<Int, Str>, Map<Bool, Int>)>() }
"#;

fn schema(name: &str) -> String {
    let p = prepared(SOURCE);
    let v = value(&p, name, Vec::new());
    ratio_eval::to_json_text(&p, &v)
}

#[test]
fn schemas_are_derived_as_the_checker_derives_them() {
    assert_eq!(
        schema("plain"),
        r#"{"type":"object","properties":{"a":{"type":"integer"},"b":{"type":"string"},"c":{"type":"boolean"}},"required":["a"],"additionalProperties":false}"#
    );
    assert_eq!(
        schema("report"),
        concat!(
            r#"{"type":"object","properties":{"by_level":{"type":"object","additionalProperties":{"type":"array","items":{"type":"string"}},"propertyNames":{"enum":["low","medium","high"]}},"#,
            r#""confidence":{"type":"number","description":"How sure","minimum":0,"maximum":1.5},"#,
            r#""cost":{"type":"string","pattern":"^\\$[0-9]+(\\.[0-9]{1,6})?$"},"#,
            r#""counts":{"type":"object","additionalProperties":{"type":"integer"}},"#,
            r#""findings":{"type":"array","items":{"oneOf":[{"type":"object","properties":{"kind":{"const":"bug"},"line":{"type":"integer"},"why":{"type":"string"}},"required":["kind","line","why"],"additionalProperties":false},{"type":"object","properties":{"kind":{"const":"style_note"},"note":{"type":"string"}},"required":["kind","note"],"additionalProperties":false},{"type":"object","properties":{"kind":{"const":"clean"}},"required":["kind"],"additionalProperties":false}]},"minItems":0,"maxItems":20},"#,
            r#""nothing":{"type":"null"},"raw":{},"#,
            r#""severity":{"type":"string","enum":["low","medium","high"],"examples":["high"]},"#,
            r#""span":{"type":"array","prefixItems":[{"type":"integer"},{"type":"integer"}],"items":false},"#,
            r#""tag":{"type":"string","pattern":"^[a-z]+$"},"#,
            r#""took":{"type":"string","pattern":"^[0-9]+(ms|s|m|h)$"},"#,
            r#""verdict":{"type":"boolean","description":"What the reviewer concluded."}},"#,
            r#""required":["by_level","confidence","cost","counts","findings","nothing","raw","severity","tag","took","verdict"],"additionalProperties":false}"#
        )
    );
    assert_eq!(
        schema("tree"),
        concat!(
            r##"{"$ref":"#/$defs/Tree","$defs":{"Tree":{"oneOf":["##,
            r##"{"type":"object","properties":{"kind":{"const":"leaf"},"value":{"type":"integer"}},"required":["kind","value"],"additionalProperties":false},"##,
            r##"{"type":"object","properties":{"kind":{"const":"node"},"kids":{"type":"array","items":{"$ref":"#/$defs/Tree"}}},"required":["kind","kids"],"additionalProperties":false}]}}}"##
        )
    );
    assert_eq!(schema("untrusted_text"), r#"{"type":"string"}"#);
    assert_eq!(schema("secret_text"), r#"{"type":"string"}"#);
    assert!(
        schema("results").starts_with(concat!(
            r#"{"oneOf":[{"type":"object","properties":{"kind":{"const":"ok"},"value":{"type":"array","items":{"type":"integer"}}},"required":["kind","value"],"additionalProperties":false},"#,
            r#"{"type":"object","properties":{"kind":{"const":"err"},"error":"#
        )),
        "{}",
        schema("results")
    );
}

#[test]
fn recursive_enum_definitions_never_share_a_name() {
    assert_eq!(
        schema("both"),
        concat!(
            r##"{"type":"object","properties":{"x":{"$ref":"#/$defs/A_2"},"y":{"$ref":"#/$defs/A"}},"required":["x","y"],"additionalProperties":false,"$defs":{"##,
            r##""A_2":{"oneOf":[{"type":"object","properties":{"kind":{"const":"l"}},"required":["kind"],"additionalProperties":false},{"type":"object","properties":{"kind":{"const":"n"},"k":{"type":"array","items":{"$ref":"#/$defs/A_2"}}},"required":["kind","k"],"additionalProperties":false}]},"##,
            r##""A":{"oneOf":[{"type":"object","properties":{"kind":{"const":"l"}},"required":["kind"],"additionalProperties":false},{"type":"object","properties":{"kind":{"const":"n"},"k":{"type":"array","items":{"$ref":"#/$defs/A"}}},"required":["kind","k"],"additionalProperties":false}]}}}"##
        )
    );
}

#[test]
fn map_keys_other_than_text_say_so() {
    let text = schema("keyed");
    assert!(
        text.contains(r#""propertyNames":{"pattern":"^-?[0-9]+$"}"#)
            && text.contains(r#""propertyNames":{"enum":["false","true"]}"#),
        "{text}"
    );
}
