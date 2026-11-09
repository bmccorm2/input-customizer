//! Caps Lock ↔ Backspace via `hidutil` UserKeyMapping, persisted by a LaunchAgent.

use std::path::PathBuf;
use std::process::Command;

pub const LABEL: &str = "com.bryan.keyboard-customizer.capsbackspace";
const HIDUTIL: &str = "/usr/bin/hidutil";
const CAPS: u64 = 0x700000039; // 30064771129
const BACKSPACE: u64 = 0x70000002A; // 30064771114

pub fn mapping_json() -> String {
    format!(
        r#"{{"UserKeyMapping":[{{"HIDKeyboardModifierMappingSrc":{CAPS},"HIDKeyboardModifierMappingDst":{BACKSPACE}}},{{"HIDKeyboardModifierMappingSrc":{BACKSPACE},"HIDKeyboardModifierMappingDst":{CAPS}}}]}}"#
    )
}

pub fn launch_agent_plist(hidutil_json: &str) -> Vec<u8> {
    let mut dict = plist::Dictionary::new();
    dict.insert("Label".into(), LABEL.into());
    dict.insert(
        "ProgramArguments".into(),
        plist::Value::Array(
            [HIDUTIL, "property", "--set", hidutil_json]
                .into_iter()
                .map(Into::into)
                .collect(),
        ),
    );
    dict.insert("RunAtLoad".into(), true.into());
    let mut out = Vec::new();
    plist::Value::Dictionary(dict)
        .to_writer_xml(&mut out)
        .expect("plist serialization to memory cannot fail");
    out
}

/// (src, dst) pairs found in `hidutil property --get UserKeyMapping` output.
pub fn parse_mappings(output: &str) -> Vec<(u64, u64)> {
    let field = |chunk: &str, key: &str| -> Option<u64> {
        let rest = &chunk[chunk.find(key)? + key.len()..];
        let rest = rest.trim_start().strip_prefix('=')?.trim_start();
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        rest[..end].parse().ok()
    };
    output
        .split('}')
        .filter_map(|c| {
            Some((
                field(c, "HIDKeyboardModifierMappingSrc")?,
                field(c, "HIDKeyboardModifierMappingDst")?,
            ))
        })
        .collect()
}

fn plist_path() -> PathBuf {
    let home = std::env::var_os("HOME").expect("HOME not set");
    PathBuf::from(home).join(format!("Library/LaunchAgents/{LABEL}.plist"))
}

fn gui_domain() -> String {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    format!("gui/{}", unsafe { getuid() })
}

fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("{cmd}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "{cmd} {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

pub fn enable() -> Result<(), String> {
    let json = mapping_json();
    run(HIDUTIL, &["property", "--set", &json])?;
    let path = plist_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, launch_agent_plist(&json)).map_err(|e| e.to_string())?;
    let domain = gui_domain();
    // Already loaded → nothing to do.
    if run("/bin/launchctl", &["print", &format!("{domain}/{LABEL}")]).is_err() {
        run(
            "/bin/launchctl",
            &["bootstrap", &domain, &path.to_string_lossy()],
        )?;
    }
    Ok(())
}

pub fn disable() -> Result<(), String> {
    // Ignore "not loaded".
    let _ = run(
        "/bin/launchctl",
        &["bootout", &format!("{}/{LABEL}", gui_domain())],
    );
    match std::fs::remove_file(plist_path()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.to_string()),
        _ => {}
    }
    run(HIDUTIL, &["property", "--set", r#"{"UserKeyMapping":[]}"#])?;
    Ok(())
}

pub fn status() -> bool {
    let Ok(out) = run(HIDUTIL, &["property", "--get", "UserKeyMapping"]) else {
        return false;
    };
    let maps = parse_mappings(&out);
    maps.contains(&(CAPS, BACKSPACE)) && maps.contains(&(BACKSPACE, CAPS)) && plist_path().exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_json_exact() {
        assert_eq!(
            mapping_json(),
            r#"{"UserKeyMapping":[{"HIDKeyboardModifierMappingSrc":30064771129,"HIDKeyboardModifierMappingDst":30064771114},{"HIDKeyboardModifierMappingSrc":30064771114,"HIDKeyboardModifierMappingDst":30064771129}]}"#
        );
        let v: serde_json::Value = serde_json::from_str(&mapping_json()).unwrap();
        assert_eq!(v["UserKeyMapping"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn plist_fields() {
        let bytes = launch_agent_plist(&mapping_json());
        let v = plist::Value::from_reader(std::io::Cursor::new(bytes)).unwrap();
        let d = v.as_dictionary().unwrap();
        assert_eq!(d["Label"].as_string(), Some(LABEL));
        assert_eq!(d["RunAtLoad"].as_boolean(), Some(true));
        assert!(!d.contains_key("KeepAlive"));
        let args: Vec<&str> = d["ProgramArguments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_string().unwrap())
            .collect();
        assert_eq!(
            args,
            [
                "/usr/bin/hidutil",
                "property",
                "--set",
                mapping_json().as_str()
            ]
        );
    }

    #[test]
    fn parses_hidutil_output() {
        let out = "RegistryID  Key                   Value\n\
100000953   UserKeyMapping   (\n        {\n        HIDKeyboardModifierMappingDst = 30064771114;\n        HIDKeyboardModifierMappingSrc = 30064771129;\n    },\n        {\n        HIDKeyboardModifierMappingDst = 30064771129;\n        HIDKeyboardModifierMappingSrc = 30064771114;\n    }\n)\n100000691   UserKeyMapping   (null)\n";
        assert_eq!(
            parse_mappings(out),
            vec![(CAPS, BACKSPACE), (BACKSPACE, CAPS)]
        );
        assert!(parse_mappings("100000691   UserKeyMapping   (null)").is_empty());
    }
}
