#![allow(missing_docs)]
#![allow(unreachable_pub)]

//! Shared support for the policy golden vectors.

pub mod oracle;

use oxkv_policy::{Budget, BudgetKind, CompareOp, Expr, FieldSchema, Literal, Path, PathRoot, Row};
use serde_json::Value;

/// One golden file.
pub struct GoldenFile {
    pub contract: String,
    pub cases: Vec<GoldenCase>,
}

/// One golden case.
pub struct GoldenCase {
    pub name: String,
    pub rule: String,
    pub budget: Value,
    pub expr: Value,
    pub schema: Option<Vec<String>>,
    pub before: Value,
    pub after: Option<Value>,
    pub row_key: String,
    pub mode: String,
    pub compile_error: Option<String>,
    pub expect: Option<Value>,
    pub batch: Option<Vec<BatchEntry>>,
}

/// One row in a batch case.
pub struct BatchEntry {
    pub row_key: String,
    pub before: Value,
    pub after: Option<Value>,
    pub expect: Value,
}

/// Loads one golden file.
pub fn load(text: &str) -> GoldenFile {
    let value: Value = serde_json::from_str(text).expect("the golden file is valid JSON");
    let contract = value["contract"]
        .as_str()
        .expect("the golden file names a contract")
        .to_owned();
    let cases = value["cases"]
        .as_array()
        .expect("the golden file has a case list")
        .iter()
        .map(parse_case)
        .collect();
    GoldenFile { contract, cases }
}

fn parse_case(value: &Value) -> GoldenCase {
    let batch = value["batch"].as_array().map(|entries| {
        entries
            .iter()
            .map(|entry| BatchEntry {
                row_key: entry["row_key"]
                    .as_str()
                    .expect("a batch entry has a row key")
                    .to_owned(),
                before: entry["before"].clone(),
                after: entry.get("after").cloned(),
                expect: entry["expect"].clone(),
            })
            .collect()
    });
    GoldenCase {
        name: value["name"]
            .as_str()
            .expect("the case names a vector")
            .to_owned(),
        rule: value["rule"].as_str().unwrap_or_default().to_owned(),
        budget: value["budget"].clone(),
        expr: value["expr"].clone(),
        schema: value["schema"].as_array().map(|fields| {
            fields
                .iter()
                .map(|field| field.as_str().expect("a schema field is text").to_owned())
                .collect()
        }),
        before: value["before"].clone(),
        after: value.get("after").cloned(),
        row_key: value["row_key"].as_str().unwrap_or("").to_owned(),
        mode: value["mode"].as_str().unwrap_or("write").to_owned(),
        compile_error: value["compile_error"].as_str().map(str::to_owned),
        expect: value.get("expect").cloned(),
        batch,
    }
}

/// Builds a budget from a JSON value.
pub fn budget(value: &Value) -> Budget {
    Budget::new(
        value["steps"]
            .as_u64()
            .expect("the budget has a step limit"),
        u32::try_from(
            value["depth"]
                .as_u64()
                .expect("the budget has a depth limit"),
        )
        .expect("the depth limit fits u32"),
        value["arena_bytes"]
            .as_u64()
            .expect("the budget has an arena limit"),
    )
}

/// Builds a field schema from the case value.
pub fn schema(fields: &[String]) -> FieldSchema {
    FieldSchema::from_names(fields.iter().cloned())
}

/// Builds an expression tree from a JSON value.
pub fn expr(value: &Value) -> Expr {
    match value["kind"].as_str().expect("the expression has a kind") {
        "literal" => Expr::Literal(literal(&value["value"])),
        "path" => Expr::Path(path(value)),
        "exists" => Expr::Exists(path(value)),
        "compare" => Expr::Compare {
            op: compare_op(
                value["op"]
                    .as_str()
                    .expect("the comparison has an operator"),
            ),
            left: Box::new(expr(&value["left"])),
            right: Box::new(expr(&value["right"])),
        },
        "and" => Expr::And(
            Box::new(expr(&value["left"])),
            Box::new(expr(&value["right"])),
        ),
        "or" => Expr::Or(
            Box::new(expr(&value["left"])),
            Box::new(expr(&value["right"])),
        ),
        "not" => Expr::Not(Box::new(expr(&value["value"]))),
        "in" => Expr::In {
            value: Box::new(expr(&value["value"])),
            set: value["set"]
                .as_array()
                .expect("the membership set is a list")
                .iter()
                .map(literal)
                .collect(),
        },
        "regex" => Expr::Regex {
            pattern: value["pattern"]
                .as_str()
                .expect("the regex has a pattern")
                .to_owned(),
        },
        other => panic!("the expression kind {other} is unknown"),
    }
}

fn compare_op(name: &str) -> CompareOp {
    match name {
        "eq" => CompareOp::Eq,
        "ne" => CompareOp::Ne,
        "lt" => CompareOp::Lt,
        "le" => CompareOp::Le,
        "gt" => CompareOp::Gt,
        "ge" => CompareOp::Ge,
        other => panic!("the comparison {other} is unknown"),
    }
}

fn path(value: &Value) -> Path {
    let root = match value["root"].as_str().expect("the path has a root") {
        "before" => PathRoot::Before,
        "after" => PathRoot::After,
        other => panic!("the path root {other} is unknown"),
    };
    let segments = value["segments"]
        .as_array()
        .expect("the path has segments")
        .iter()
        .map(|segment| segment.as_str().expect("a segment is text").to_owned())
        .collect();
    Path::new(root, segments).expect("the path has one segment")
}

/// Builds a row from a JSON value.
pub fn row(value: &Value) -> Row {
    let mut row = Row::new();
    let fields = value.as_object().expect("the row is an object");
    for (name, field) in fields {
        row.insert(name.clone(), literal(field));
    }
    row
}

fn literal(value: &Value) -> Literal {
    let text = value["type"].as_str().expect("the literal has a type");
    match text {
        "bool" => Literal::Bool(value["value"].as_bool().expect("the value is a bool")),
        "i64" => Literal::I64(value["value"].as_i64().expect("the value is an i64")),
        "u64" => Literal::U64(value["value"].as_u64().expect("the value is a u64")),
        "text" => Literal::Text(
            value["value"]
                .as_str()
                .expect("the value is text")
                .to_owned(),
        ),
        "bytes" => Literal::Bytes(unhex(value["value"].as_str().expect("the value is hex"))),
        other => panic!("the literal type {other} is unknown"),
    }
}

/// Parses a dotted path such as `before.owner`.
pub fn expected_path(text: &str) -> Path {
    let mut parts = text.split('.');
    let root = match parts.next().expect("the path is not empty") {
        "before" => PathRoot::Before,
        "after" => PathRoot::After,
        other => panic!("the path root {other} is unknown"),
    };
    let segments: Vec<String> = parts.map(str::to_owned).collect();
    Path::new(root, segments).expect("the path has one segment")
}

/// Returns true when the expected path is absent or null.
pub fn expect_path(value: &Value) -> Option<Path> {
    value["path"].as_str().map(expected_path)
}

/// Returns the expected budget failure, when present.
pub fn expect_budget(value: &Value) -> Option<(BudgetKind, u64, u64, u32)> {
    let budget = value.get("budget")?.as_object()?;
    let kind = match budget.get("kind")?.as_str()? {
        "step" => BudgetKind::Step,
        "depth" => BudgetKind::Depth,
        "memory" => BudgetKind::Memory,
        other => panic!("the budget kind {other} is unknown"),
    };
    Some((
        kind,
        budget.get("limit")?.as_u64()?,
        budget.get("used")?.as_u64()?,
        u32::try_from(budget.get("depth")?.as_u64()?).expect("the failure depth fits u32"),
    ))
}

/// Returns the lowercase hexadecimal form of a byte slice.
pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text
}

/// Decodes lowercase hexadecimal text.
pub fn unhex(text: &str) -> Vec<u8> {
    assert!(
        text.len().is_multiple_of(2),
        "the hex text has an even length"
    );
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let high = hex_value(pair[0]);
            let low = hex_value(pair[1]);
            (high << 4) | low
        })
        .collect()
}

fn hex_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        other => panic!("the hex byte {other} is invalid"),
    }
}
