use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use crate::LoadedMemory;

pub const ZONE_CORE: &str = "core";
pub const ZONE_WORK: &str = "work";
pub const ZONE_EPISODE: &str = "episode";
pub const ZONE_GENERAL: &str = "general";
/// Autonomous goal generation: zone for stored goals and intentions.
pub const ZONE_GOALS: &str = "goals";

pub const ALL_ZONES: &[&str] = &[ZONE_CORE, ZONE_WORK, ZONE_EPISODE, ZONE_GENERAL, ZONE_GOALS];

pub fn group_by_zone<'a>(memories: &'a [LoadedMemory]) -> BTreeMap<String, Vec<&'a LoadedMemory>> {
    let mut map: BTreeMap<String, Vec<&'a LoadedMemory>> = BTreeMap::new();
    for m in memories {
        map.entry(m.frontmatter.zone.clone()).or_default().push(m);
    }
    map
}

pub fn get_zone<'a>(memories: &'a [LoadedMemory], zone: &str) -> Vec<&'a LoadedMemory> {
    memories
        .iter()
        .filter(|m| m.frontmatter.zone == zone)
        .collect()
}

pub fn build_palace_index_simple(memories: &[LoadedMemory]) -> String {
    let mut buf = String::from("## Memory Palace\n\n");
    let grouped = group_by_zone(memories);

    for &zone in ALL_ZONES {
        let items = grouped.get(zone).map(|v| v.as_slice()).unwrap_or(&[]);
        buf.push_str(&format!("### {} ({} items)\n", zone, items.len()));
        if items.is_empty() {
            buf.push_str("(empty)\n");
        } else {
            for m in items.iter().take(5) {
                let preview = m.body.lines().next().unwrap_or("").trim();
                let chars: Vec<char> = preview.chars().collect();
                let truncated: String = chars.iter().take(80).collect();
                if chars.len() > 80 {
                    buf.push_str(&format!("- {truncated}…\n"));
                } else {
                    buf.push_str(&format!("- {truncated}\n"));
                }
            }
            if items.len() > 5 {
                buf.push_str(&format!("  … and {} more\n", items.len() - 5));
            }
        }
        buf.push('\n');
    }

    buf
}

fn palace_dir() -> PathBuf {
    openpaw_core::openpaw_home().join("palace")
}

pub fn load_palace_index() -> io::Result<Option<String>> {
    let path = palace_dir().join("index.md");
    if path.exists() {
        Ok(Some(std::fs::read_to_string(&path)?))
    } else {
        Ok(None)
    }
}

pub fn save_palace_index(content: &str) -> io::Result<()> {
    let dir = palace_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("index.md");
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn load_zone_summary(zone: &str) -> io::Result<Option<String>> {
    let path = palace_dir().join(format!("{zone}.md"));
    if path.exists() {
        Ok(Some(std::fs::read_to_string(&path)?))
    } else {
        Ok(None)
    }
}

pub fn save_zone_summary(zone: &str, content: &str) -> io::Result<()> {
    validate_zone_name(zone)?;
    let dir = palace_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{zone}.md"));
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn validate_zone_name(zone: &str) -> io::Result<()> {
    if zone.is_empty()
        || zone.contains('/')
        || zone.contains('\\')
        || zone.contains("..")
        || !zone
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid zone name: {zone:?}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Confidence, MemoryFrontmatter, Scope, Source};
    use std::path::PathBuf;

    fn make_mem(zone: &str, body: &str) -> LoadedMemory {
        let fm = MemoryFrontmatter::new(Source::User, Confidence::Medium, vec![]).with_zone(zone);
        LoadedMemory {
            frontmatter: fm,
            body: body.to_string(),
            source_path: PathBuf::new(),
            scope: Scope::User,
        }
    }

    #[test]
    fn zone_backward_compat() {
        let yaml =
            "id: mem_test\ncreated: 2024-01-01T00:00:00Z\nsource: user\nconfidence: medium\n";
        let fm: MemoryFrontmatter = serde_yml::from_str(yaml).unwrap();
        assert_eq!(fm.zone, "general");
    }

    #[test]
    fn palace_index_format() {
        let memories = vec![
            make_mem("core", "I prefer dark mode"),
            make_mem("work", "Project X uses React"),
            make_mem("general", "Rust is great"),
        ];
        let index = build_palace_index_simple(&memories);
        assert!(index.contains("## Memory Palace"));
        assert!(index.contains("### core (1 items)"));
        assert!(index.contains("### work (1 items)"));
        assert!(index.contains("### general (1 items)"));
        assert!(index.contains("### episode (0 items)"));
    }

    #[test]
    fn group_by_zone_test() {
        let memories = vec![
            make_mem("core", "a"),
            make_mem("core", "b"),
            make_mem("work", "c"),
        ];
        let grouped = group_by_zone(&memories);
        assert_eq!(grouped.get("core").map(|v| v.len()), Some(2));
        assert_eq!(grouped.get("work").map(|v| v.len()), Some(1));
        assert!(!grouped.contains_key("episode"));
    }

    #[test]
    fn validate_zone_name_rejects_traversal() {
        assert!(validate_zone_name("../etc").is_err());
        assert!(validate_zone_name("a/b").is_err());
        assert!(validate_zone_name("").is_err());
        assert!(validate_zone_name("core").is_ok());
    }
}
