//! Microsoft Store (MSIX/AppX) apps, which have no Uninstall registry key.
//!
//! Source: `Get-AppxPackage` for the current user (no elevation), run hidden through
//! Windows PowerShell, keeping packages that are not frameworks, resource packs or
//! system-signed and that list a launchable app in their `AppxManifest.xml`. The
//! display name is the manifest's `<DisplayName>` when it is literal text; Store
//! packages usually give an `ms-resource:` reference that needs the package's
//! resource index, so then the package name is made readable instead
//! (`Microsoft.WindowsTerminal` becomes "Windows Terminal"). The icon is the
//! manifest's `Square44x44Logo` (or `Logo`) PNG. Uninstall is
//! `Remove-AppxPackage` for that package; a package Windows marks non-removable is
//! protected. `Installed::key_name` holds the package family name, which is what
//! UserAssist records launches under (`<family>!<app id>`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use super::{AppEntry, Installed, powershell, run_capture};

const READ_TIMEOUT: Duration = Duration::from_secs(60);

const SCRIPT: &str = concat!(
    "$ErrorActionPreference='SilentlyContinue';",
    "[Console]::OutputEncoding=[Text.Encoding]::UTF8;",
    "$r=@(Get-AppxPackage|Where-Object{-not $_.IsFramework -and -not $_.IsResourcePackage -and $_.SignatureKind -ne 'System' -and $_.InstallLocation}|",
    "ForEach-Object{[pscustomobject]@{n=$_.Name;f=$_.PackageFullName;a=$_.PackageFamilyName;v=[string]$_.Version;l=$_.InstallLocation;p=$_.Publisher;r=[bool]$_.NonRemovable}});",
    "ConvertTo-Json -InputObject $r -Compress -Depth 3",
);

fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The text of `<tag ...>value</tag>` (first one), or an attribute `name="value"` of a tag.
fn element_text(xml: &str, tag: &str) -> Option<String> {
    let open = xml.find(&format!("<{tag}>"))? + tag.len() + 2;
    let end = xml[open..].find(&format!("</{tag}>"))?;
    Some(xml[open..open + end].trim().to_string()).filter(|s| !s.is_empty())
}

fn attribute(tag_text: &str, name: &str) -> Option<String> {
    let at = tag_text.find(&format!("{name}=\""))? + name.len() + 2;
    let end = tag_text[at..].find('"')?;
    Some(tag_text[at..at + end].to_string()).filter(|s| !s.is_empty())
}

/// Every `<...VisualElements ...>` tag of a manifest.
fn visual_elements(xml: &str) -> Vec<&str> {
    let mut tags = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find("VisualElements") {
        let start = rest[..at].rfind('<').unwrap_or(0);
        let Some(end) = rest[at..].find('>') else {
            break;
        };
        tags.push(&rest[start..at + end]);
        rest = &rest[at + end..];
    }
    tags
}

/// "Microsoft.WindowsTerminal" -> "Windows Terminal".
fn readable(package_name: &str) -> String {
    let base = package_name
        .split_once('.')
        .map_or(package_name, |(_, rest)| rest);
    let mut out = String::new();
    let mut previous: Option<char> = None;
    for c in base.chars() {
        let c = if c == '.' || c == '_' { ' ' } else { c };
        if c.is_uppercase() && previous.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit()) {
            out.push(' ');
        }
        out.push(c);
        previous = Some(c);
    }
    out
}

/// A logo path from a manifest (`Assets\Logo.png`) resolved to a file that exists:
/// the exact file, or the best `Logo.scale-*.png` / `Logo.targetsize-*.png` variant.
fn logo_file(location: &Path, relative: &str) -> Option<PathBuf> {
    let exact = location.join(relative.replace('/', "\\"));
    if exact.is_file() {
        return Some(exact);
    }
    let dir = exact.parent()?;
    let stem = exact.file_stem()?.to_string_lossy().to_lowercase();
    let mut variants: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"))
                && p.file_name().is_some_and(|n| {
                    let n = n.to_string_lossy().to_lowercase();
                    n.starts_with(&format!("{stem}."))
                        && !n.contains("contrast")
                        && !n.contains("altform-lightunplated")
                })
        })
        .collect();
    // Prefer the 100%/200% scale, then a 32 to 48 px target size, then whatever is first.
    variants.sort_by_key(|p| {
        let n = p
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if n.contains("scale-200") {
            0
        } else if n.contains("scale-100")
            || n.contains("targetsize-48")
            || n.contains("targetsize-32")
        {
            1
        } else {
            2
        }
    });
    variants.into_iter().next()
}

/// Store apps for the current user. Err carries why the list could not be read.
pub fn read() -> Result<Vec<Installed>, String> {
    let mut command = Command::new(powershell());
    command.args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT]);
    let (_, output) = run_capture(
        command,
        READ_TIMEOUT,
        "PowerShell",
        "Windows PowerShell is not available.",
    )?;
    let parsed: Value = serde_json::from_str(output.trim().trim_start_matches('\u{feff}'))
        .map_err(|_| "The Store apps could not be read.".to_string())?;
    let rows = parsed.as_array().cloned().unwrap_or_default();
    let mut apps = Vec::new();
    for row in rows {
        let (Some(name), Some(full), Some(family), Some(location)) = (
            text(&row["n"]),
            text(&row["f"]),
            text(&row["a"]),
            text(&row["l"]),
        ) else {
            continue;
        };
        let location_path = Path::new(&location);
        let Ok(manifest) = std::fs::read_to_string(location_path.join("AppxManifest.xml")) else {
            continue;
        };
        let tags = visual_elements(&manifest);
        // Launchable only when some app of the package is listed.
        if tags.is_empty() || tags.iter().all(|t| t.contains("AppListEntry=\"none\"")) {
            continue;
        }
        let literal =
            element_text(&manifest, "DisplayName").filter(|n| !n.starts_with("ms-resource:"));
        let display = literal.unwrap_or_else(|| readable(&name));
        let logo = tags
            .iter()
            .find_map(|t| attribute(t, "Square44x44Logo").or_else(|| attribute(t, "Logo")))
            .or_else(|| element_text(&manifest, "Logo"))
            .and_then(|relative| logo_file(location_path, &relative))
            .map(|p| p.to_string_lossy().into_owned());
        let non_removable = row["r"].as_bool().unwrap_or(false);
        let publisher = text(&row["p"]).map(|dn| {
            dn.split(',')
                .find_map(|part| part.trim().strip_prefix("CN="))
                .unwrap_or(&dn)
                .trim_matches('"')
                .to_string()
        });
        apps.push(Installed {
            entry: AppEntry {
                name: display,
                path: format!(r"Microsoft Store\{full}"),
                bundle_id: None,
                version: text(&row["v"]),
                size_bytes: 0,
                last_used: None,
                running: false,
                protected: non_removable.then(|| "Windows does not allow this app to be removed.".to_string()),
            },
            uninstall: (!non_removable).then(|| {
                format!(
                    "powershell.exe -NoProfile -NonInteractive -Command \"Remove-AppxPackage -Package '{full}'\""
                )
            }),
            key_name: family,
            publisher,
            folder: None,
            icon_source: logo,
            run_folder: Some(location),
        });
    }
    Ok(apps)
}
