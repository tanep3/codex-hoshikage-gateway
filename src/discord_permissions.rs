//! Discord role overwrites: everyone, aggregate roles, then the member override.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
pub fn attachment_allowed(
    guild: &str,
    bot: &str,
    roles: &Value,
    member: &Value,
    overwrites: &Value,
    in_thread: bool,
) -> Result<bool> {
    let roles = roles.as_array().context("guild roles missing")?;
    let member_roles = member["roles"].as_array().context("member roles missing")?;
    ensure!(
        member["user"]["id"] == bot,
        "Discord member identity mismatch"
    );
    let bits =
        |v: &Value| -> Result<u64> { Ok(v.as_str().context("permission bits missing")?.parse()?) };
    let owns = |id: &Value| member_roles.contains(id);
    let mut permissions = bits(
        &roles
            .iter()
            .find(|r| r["id"] == guild)
            .context("everyone role missing")?["permissions"],
    )?;
    for role in roles {
        if owns(&role["id"]) {
            permissions |= bits(&role["permissions"])?;
        }
    }
    if permissions & (1 << 3) != 0 {
        return Ok(true);
    }
    if let Some(until) = member["communication_disabled_until"].as_str() {
        let until =
            time::OffsetDateTime::parse(until, &time::format_description::well_known::Rfc3339)?;
        if until > time::OffsetDateTime::now_utc() {
            return Ok(false);
        }
    }
    let list = overwrites
        .as_array()
        .context("channel overwrites missing")?;
    for class in 0..3 {
        let (mut deny, mut allow) = (0, 0);
        for o in list {
            let matched = match class {
                0 => o["type"] == 0 && o["id"] == guild,
                1 => o["type"] == 0 && o["id"] != guild && owns(&o["id"]),
                _ => o["type"] == 1 && o["id"] == bot,
            };
            if matched {
                deny |= bits(&o["deny"])?;
                allow |= bits(&o["allow"])?;
            }
        }
        permissions = (permissions & !deny) | allow;
    }
    let required = (1 << 10) | (1 << 15) | (1 << 16) | if in_thread { 1 << 38 } else { 1 << 11 };
    Ok(permissions & required == required)
}
