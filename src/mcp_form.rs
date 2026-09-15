//! The deliberately bounded Proxy flat-primitives-v1 profile. No schema execution.
use anyhow::{Context, Result, ensure};
use serde_json::{Map, Value};
pub fn supported(c: &Value) -> bool {
    c["features"]["interaction_relay"] == true
        && c["interaction_kinds"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == "mcp_form"))
        && c["interaction_limits"]["schema_profile"] == "flat-primitives-v1"
        && c["interaction_limits"]["max_count"]
            .as_u64()
            .is_some_and(|n| n <= 16)
        && c["interaction_limits"]["max_bytes"]
            .as_u64()
            .is_some_and(|n| n <= 65536)
}
fn fields(v: &Value, allowed: &[&str]) -> Result<()> {
    ensure!(
        v.as_object()
            .context("object required")?
            .keys()
            .all(|k| allowed.contains(&k.as_str())),
        "未対応の入力条件です"
    );
    Ok(())
}
pub fn validate_schema(s: &Value) -> Result<()> {
    fields(
        s,
        &[
            "type",
            "properties",
            "required",
            "additionalProperties",
            "title",
            "description",
        ],
    )?;
    ensure!(
        s["type"] == "object" && s.get("additionalProperties").is_none_or(|v| v == false),
        "未対応のフォームです"
    );
    let p = s["properties"].as_object().context("properties required")?;
    ensure!(p.len() <= 32, "項目数が上限を超えています");
    if let Some(r) = s.get("required") {
        let mut seen = std::collections::HashSet::new();
        for k in r.as_array().context("required invalid")? {
            let k = k.as_str().context("required invalid")?;
            ensure!(p.contains_key(k) && seen.insert(k), "required invalid");
        }
    }
    for v in p.values() {
        fields(
            v,
            &[
                "type",
                "title",
                "description",
                "default",
                "enum",
                "enumNames",
                "minimum",
                "maximum",
                "minLength",
                "maxLength",
            ],
        )?;
        ensure!(
            matches!(
                v["type"].as_str(),
                Some("string" | "integer" | "number" | "boolean")
            ),
            "未対応の入力型です"
        );
        for k in ["minimum", "maximum"] {
            if let Some(n) = v.get(k) {
                ensure!(
                    matches!(v["type"].as_str(), Some("integer" | "number"))
                        && n.as_f64()
                            .is_some_and(|n| n.abs() <= 9_007_199_254_740_991.0),
                    "数値条件が不正です"
                );
            }
        }
        for k in ["minLength", "maxLength"] {
            if let Some(n) = v.get(k) {
                ensure!(
                    v["type"] == "string" && n.as_u64().is_some_and(|n| n <= 8192),
                    "文字数条件が不正です"
                );
            }
        }
        ensure!(
            !v["minimum"]
                .as_f64()
                .zip(v["maximum"].as_f64())
                .is_some_and(|(a, b)| a > b)
                && !v["minLength"]
                    .as_u64()
                    .zip(v["maxLength"].as_u64())
                    .is_some_and(|(a, b)| a > b),
            "上下限が不正です"
        );
        if let Some(e) = v.get("enum") {
            let a = e.as_array().context("enum invalid")?;
            ensure!(
                !a.is_empty() && a.len() <= 64 && a.iter().all(|x| scalar_type(v, x)),
                "選択肢が不正です"
            );
        }
        if let Some(names) = v.get("enumNames") {
            ensure!(
                names
                    .as_array()
                    .is_some_and(|a| a.len() == v["enum"].as_array().map_or(0, Vec::len)
                        && a.iter().all(Value::is_string)),
                "選択肢の表示名が不正です"
            );
        }
    }
    Ok(())
}
fn scalar_type(s: &Value, v: &Value) -> bool {
    if v.is_number()
        && !v
            .as_f64()
            .is_some_and(|n| n.abs() <= 9_007_199_254_740_991.0)
    {
        return false;
    }
    match s["type"].as_str() {
        Some("string") => v.is_string(),
        Some("integer") => v.is_i64() || v.is_u64(),
        Some("number") => v.is_number(),
        Some("boolean") => v.is_boolean(),
        _ => false,
    }
}
pub fn validate_scalar(s: &Value, v: &Value) -> Result<()> {
    ensure!(scalar_type(s, v), "入力の型が違います");
    ensure!(
        !s["enum"].as_array().is_some_and(|a| !a.contains(v)),
        "選択肢から選んでください"
    );
    if let Some(n) = v.as_f64() {
        ensure!(
            !s["minimum"].as_f64().is_some_and(|m| n < m)
                && !s["maximum"].as_f64().is_some_and(|m| n > m),
            "数値が範囲外です"
        );
    }
    if let Some(t) = v.as_str() {
        let n = t.chars().count() as u64;
        ensure!(
            t.len() <= 8192
                && !s["minLength"].as_u64().is_some_and(|m| n < m)
                && !s["maxLength"].as_u64().is_some_and(|m| n > m),
            "文字数または容量が範囲外です"
        );
    }
    Ok(())
}
pub fn parse_value(s: &Value, text: &str) -> Result<Value> {
    let v = if let Some(e) = s["enum"].as_array() {
        let n: usize = text
            .trim()
            .parse()
            .context("選択肢の番号を入力してください")?;
        e.get(n.checked_sub(1).context("選択肢の番号が範囲外です")?)
            .context("選択肢の番号が範囲外です")?
            .clone()
    } else {
        match s["type"].as_str() {
            Some("string") => Value::String(text.into()),
            Some("boolean") => match text.trim() {
                "true" | "はい" => Value::Bool(true),
                "false" | "いいえ" => Value::Bool(false),
                _ => anyhow::bail!("はい/いいえを入力してください"),
            },
            _ => serde_json::from_str(text.trim()).context("数値を入力してください")?,
        }
    };
    validate_scalar(s, &v)?;
    Ok(v)
}
pub fn validate_answers(s: &Value, a: &Map<String, Value>) -> Result<()> {
    validate_schema(s)?;
    let props = s["properties"].as_object().unwrap();
    for k in s["required"].as_array().into_iter().flatten() {
        ensure!(a.contains_key(k.as_str().unwrap()), "必須項目が未入力です");
    }
    for (k, v) in a {
        validate_scalar(props.get(k).context("不明な項目です")?, v)?;
    }
    ensure!(
        serde_json::to_vec(a)?.len() <= 65536,
        "回答全体が大きすぎます"
    );
    Ok(())
}
pub fn display(v: &Value) -> String {
    v.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string())
}
pub fn help(name: &str, s: &Value) -> String {
    let mut t = format!(
        "項目: {name}\n型: {}\n",
        s["type"].as_str().unwrap_or("不明")
    );
    for (k, label) in [
        ("title", "名称"),
        ("description", "説明"),
        ("minimum", "最小値"),
        ("maximum", "最大値"),
        ("minLength", "最小文字数"),
        ("maxLength", "最大文字数"),
        ("default", "参考の既定値（自動採用しません）"),
    ] {
        if let Some(v) = s.get(k) {
            t.push_str(&format!("{label}: {}\n", display(v)));
        }
    }
    if let Some(a) = s["enum"].as_array() {
        t.push_str("選択肢の番号を入力してください。\n");
        for (i, v) in a.iter().enumerate() {
            t.push_str(&format!(
                "{}. {}{}\n",
                i + 1,
                display(v),
                s["enumNames"]
                    .get(i)
                    .and_then(Value::as_str)
                    .map(|x| format!(" — {x}"))
                    .unwrap_or_default()
            ));
        }
    } else if s["type"] == "boolean" {
        t.push_str("はい / いいえ を入力してください。\n");
    }
    t
}
