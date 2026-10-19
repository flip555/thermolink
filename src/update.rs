//! Self-update from GitHub releases, plus an in-place restart.
//!
//! A Rust deployment ships one binary, so "install" means: download the
//! release asset for this platform, verify it is really an executable,
//! copy the current binary aside as a timestamped backup, then rename the
//! new file over it. The running process keeps the old image until it
//! execs itself again, which [`restart`] does without losing the process
//! tree (important when the relay is started from `nohup` or a shell).
//!
//! The repository is expected to be private, so a missing release or a
//! missing asset is reported as an error rather than guessed around.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

/// Repository used when `THERMOLINK_REPO` is not set.
pub const DEFAULT_REPO: &str = "flip555/thermolink";

const USER_AGENT: &str = "thermolink-updater";
/// Refuse to install anything larger than this.
const MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024;

pub fn repo() -> String {
    std::env::var("THERMOLINK_REPO")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_REPO.to_string())
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Asset {
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "browser_download_url")]
    pub url: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Release {
    #[serde(default, rename = "tag_name")]
    pub tag: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "body")]
    pub notes: String,
    #[serde(default, rename = "html_url")]
    pub url: String,
    #[serde(default, rename = "published_at")]
    pub published: String,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(20))
        .build()
        .context("building the HTTP client")
}

/// Latest published release. A repository with no releases returns the
/// GitHub error text instead of an empty version, so the UI can say why.
pub async fn latest(repo: &str) -> Result<Release> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let res = client()?
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .with_context(|| format!("requesting {url}"))?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        let detail: String = body.trim().chars().take(300).collect();
        return Err(anyhow!(
            "GitHub returned {status}{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    }
    let mut release: Release =
        serde_json::from_str(&body).context("parsing the GitHub release response")?;
    release.notes = release.notes.trim().chars().take(1200).collect();
    Ok(release)
}

/// Numeric components of a tag, ignoring a leading `v` and non-digits.
pub fn parts(tag: &str) -> Vec<u64> {
    tag.trim_start_matches(['v', 'V'])
        .split('.')
        .filter_map(|chunk| {
            let digits: String = chunk.chars().filter(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect()
}

/// True only when `latest` is strictly newer than `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    parts(latest) > parts(current)
}

/// Pick the release asset for one platform. Names are matched loosely
/// because release tooling conventions differ (`x86_64-unknown-linux-gnu`,
/// `linux-x86_64`, `linux-amd64`); checksum and signature sidecars are
/// never candidates.
pub fn pick_asset<'a>(assets: &'a [Asset], os: &str, arch: &str) -> Option<&'a Asset> {
    let aliases: &[&str] = match arch {
        "x86_64" => &["x86_64", "amd64"],
        "aarch64" => &["aarch64", "arm64"],
        _ => &[arch],
    };
    let os = os.to_ascii_lowercase();
    let os_alias = match os.as_str() {
        "macos" => "darwin",
        other => other,
    };
    let mut best: Option<(u8, &Asset)> = None;
    for asset in assets {
        let name = asset.name.to_ascii_lowercase();
        if asset.url.is_empty()
            || !name.contains("thermolink")
            || [".sha256", ".sha512", ".sig", ".asc", ".txt", ".json", ".md"]
                .iter()
                .any(|ext| name.ends_with(ext))
        {
            continue;
        }
        let arch_hit = aliases.iter().any(|a| name.contains(a));
        let os_hit = name.contains(&os) || name.contains(os_alias);
        // 3: exact platform, 2: os only, 1: arch only, 0: plain `thermolink`
        let tier = if os_hit && arch_hit {
            3
        } else if os_hit {
            2
        } else if arch_hit {
            1
        } else if name == "thermolink" || name == "thermolink.exe" {
            0
        } else {
            continue;
        };
        // A platform-qualified name always beats an unqualified one, and
        // the first asset wins ties (GitHub keeps upload order stable).
        if best.as_ref().is_none_or(|(score, _)| tier > *score) {
            best = Some((tier, asset));
        }
    }
    best.map(|(_, asset)| asset)
}

/// Download one release asset, refusing anything oversized.
pub async fn download(url: &str) -> Result<Vec<u8>> {
    let res = client()?
        .get(url)
        .send()
        .await
        .with_context(|| format!("downloading {url}"))?;
    if !res.status().is_success() {
        return Err(anyhow!("download failed: HTTP {}", res.status()));
    }
    if let Some(len) = res.content_length() {
        if len > MAX_ASSET_BYTES {
            return Err(anyhow!(
                "asset is {len} bytes, above the {} MiB limit",
                MAX_ASSET_BYTES / 1024 / 1024
            ));
        }
    }
    let bytes = res.bytes().await.context("reading the release asset")?;
    if bytes.len() as u64 > MAX_ASSET_BYTES {
        return Err(anyhow!("asset exceeded the size limit while downloading"));
    }
    Ok(bytes.to_vec())
}

fn expected_magic() -> &'static [u8] {
    if cfg!(windows) {
        b"MZ"
    } else {
        b"\x7fELF"
    }
}

#[derive(Clone, Debug)]
pub struct Installed {
    pub path: PathBuf,
    pub backup: Option<PathBuf>,
    pub bytes: usize,
}

/// Replace `dest` with `bytes`, keeping the previous file as a backup.
///
/// The new file is written beside the destination and renamed into place,
/// so a crash mid-write cannot leave a half-written executable behind.
pub fn install(bytes: &[u8], dest: &Path) -> Result<Installed> {
    if bytes.len() < 4 || !bytes.starts_with(expected_magic()) {
        return Err(anyhow!(
            "release asset is not a {} executable",
            if cfg!(windows) { "Windows" } else { "Linux" }
        ));
    }
    if bytes.len() as u64 > MAX_ASSET_BYTES {
        return Err(anyhow!("release asset is too large"));
    }
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = dest
        .file_name()
        .ok_or_else(|| anyhow!("destination has no file name"))?
        .to_string_lossy()
        .to_string();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let backup_name = format!("{name}.bak-{stamp}");
    let backup = parent.join(&backup_name);
    let tmp = parent.join(format!(".{name}.new-{}", std::process::id()));

    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("marking {} executable", tmp.display()))?;
    }
    // Flush the file before it can replace a running binary.
    std::fs::File::open(&tmp)
        .and_then(|f| f.sync_all())
        .with_context(|| format!("flushing {}", tmp.display()))?;

    let backup = if dest.exists() {
        std::fs::copy(dest, &backup).with_context(|| format!("backing up {}", dest.display()))?;
        Some(backup)
    } else {
        None
    };

    if let Err(e) = std::fs::rename(&tmp, dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow!("replacing {}: {e}", dest.display()));
    }
    Ok(Installed {
        path: dest.to_path_buf(),
        backup,
        bytes: bytes.len(),
    })
}

/// Replace the current process image with `exe`, keeping the same PID,
/// stdout/stderr and process group. Sessions live in memory, so the caller
/// must warn that signing in again is required.
#[cfg(unix)]
pub fn restart(exe: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let err = std::process::Command::new(exe).args(args).exec();
    Err(anyhow!("exec {} failed: {err}", exe.display()))
}

#[cfg(not(unix))]
pub fn restart(exe: &Path) -> Result<()> {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    std::process::Command::new(exe)
        .args(args)
        .spawn()
        .with_context(|| format!("spawning {}", exe.display()))?;
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.into(),
            url: format!("https://example.invalid/{name}"),
            size: 1024,
        }
    }

    #[test]
    fn version_components_are_numeric() {
        assert_eq!(parts("v0.3.0"), vec![0, 3, 0]);
        assert_eq!(parts("1.2.3-rc.4"), vec![1, 2, 3, 4]);
        assert!(parts("nonsense").is_empty());
    }

    #[test]
    fn newer_versions_compare_strictly() {
        assert!(is_newer("v0.3.0", "0.2.9"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("v0.2.0", "0.2.0"));
        assert!(!is_newer("v0.1.9", "0.2.0"));
        assert!(!is_newer("nonsense", "0.2.0"));
    }

    #[test]
    fn platform_asset_beats_sidecars_and_other_platforms() {
        let assets = vec![
            asset("thermolink-linux-x86_64.sha256"),
            asset("thermolink-darwin-arm64"),
            asset("thermolink-linux-x86_64"),
            asset("thermolink-linux-aarch64"),
        ];
        let picked = pick_asset(&assets, "linux", "x86_64").expect("linux asset");
        assert_eq!(picked.name, "thermolink-linux-x86_64");

        let picked = pick_asset(&assets, "macos", "aarch64").expect("darwin asset");
        assert_eq!(picked.name, "thermolink-darwin-arm64");
    }

    #[test]
    fn unqualified_asset_is_a_last_resort() {
        let assets = vec![asset("thermolink")];
        assert_eq!(
            pick_asset(&assets, "linux", "x86_64").map(|a| a.name.as_str()),
            Some("thermolink")
        );
        let none: Vec<Asset> = vec![asset("checksums.txt")];
        assert!(pick_asset(&none, "linux", "x86_64").is_none());
    }

    #[test]
    fn install_replaces_and_backs_up() {
        let dir = std::env::temp_dir().join(format!(
            "thermolink-update-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("thermolink");
        std::fs::write(&dest, b"old-binary").unwrap();

        let mut payload = expected_magic().to_vec();
        payload.extend_from_slice(b"new-binary-payload");
        let outcome = install(&payload, &dest).expect("install");
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        let backup = outcome.backup.expect("backup");
        assert_eq!(std::fs::read(backup).unwrap(), b"old-binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_refuses_non_executables() {
        let dir = std::env::temp_dir().join(format!(
            "thermolink-update-bad-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("thermolink");
        let err = install(b"<html>not found</html>", &dest);
        assert!(err.is_err());
        assert!(!dest.exists(), "a rejected asset must not touch the target");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
