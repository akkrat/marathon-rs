//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/xctestrun/CodeCoverageTargets.kt

use super::{
    mach_o,
    v2::{CodeCoverageBuildableInfo, CodeCoverageBuildableInfoParams},
};
use crate::{
    cmd::failure,
    plist::{self, PlistDict, PlistValue},
};
use marathon_config::CONFIGURATION_EXCEPTION_CLASS;
use marathon_core::error::{VendorError, VendorResult};
use marathon_error::FailureKind;
use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
};

pub struct RemoteCoverage {
    pub infos: Vec<CodeCoverageBuildableInfo>,
    pub static_libraries: Vec<(PathBuf, String)>,
}
fn configuration(message: String) -> VendorError {
    VendorError::Other(failure(CONFIGURATION_EXCEPTION_CLASS, message))
}
fn string(info: &PlistDict, key: &str) -> Option<String> {
    match info.get_cloned(key) {
        Some(PlistValue::String(s)) => Some(s),
        _ => None,
    }
}
fn boolean(info: &PlistDict, key: &str) -> Option<bool> {
    match info.get_cloned(key) {
        Some(PlistValue::Bool(b)) => Some(b),
        _ => None,
    }
}
fn strings(info: &PlistDict, key: &str) -> Vec<String> {
    match info.get_cloned(key) {
        Some(PlistValue::Array(values)) => values
            .into_iter()
            .filter_map(|v| match v {
                PlistValue::String(s) => Some(s),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}
fn plural(info: &PlistDict, many: &str, one: &str) -> Vec<String> {
    let values = strings(info, many);
    if values.is_empty() {
        string(info, one).into_iter().collect()
    } else {
        values
    }
}
fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => (),
            Component::ParentDir if result.file_name().is_some_and(|n| n != "..") => {
                result.pop();
            }
            _ => result.push(part),
        }
    }
    result
}
fn absolute(path: &Path) -> VendorResult<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|e| VendorError::Other(failure(FailureKind::Io, e.to_string())))?
            .join(path)
    };
    Ok(path)
}
fn test_root(path: &Path) -> VendorResult<PathBuf> {
    // Kotlin takes absoluteFile.parentFile before normalizing, preserving a trailing `..`.
    Ok(normalize(absolute(path)?.parent().unwrap_or(Path::new("/"))))
}
fn parts(path: &Path) -> Vec<&str> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            Component::ParentDir => Some(".."),
            _ => None,
        })
        .collect()
}
fn bundle(path: &Path) -> bool {
    // Kotlin treats `.framework` as extension `framework`; Path::extension does not.
    path.file_name()
        .and_then(|s| s.to_str())
        .and_then(|name| name.rsplit_once('.').map(|(_, extension)| extension))
        .is_some_and(|s| ["app", "appex", "bundle", "framework", "xctest", "xpc"].contains(&s))
}
struct Installed {
    bundle: PathBuf,
    root: usize,
    depth: usize,
}
struct BundleIndex {
    roots: Vec<(PathBuf, String)>,
    installed: Option<Vec<Installed>>,
    slices: HashMap<PathBuf, Vec<mach_o::Slice>>,
}
impl BundleIndex {
    fn slices(&mut self, path: &Path) -> &Vec<mach_o::Slice> {
        self.slices
            .entry(path.to_owned())
            .or_insert_with(|| mach_o::slices(path))
    }
    fn has(&mut self, path: &Path, architecture: &str) -> bool {
        let slices = self.slices(path);
        slices.iter().all(|s| s.architecture.is_none()) || slices.iter().any(|s| s.architecture == Some(architecture))
    }
    fn index(&mut self) {
        if self.installed.is_some() {
            return;
        }
        let mut installed = vec![];
        for (index, (root, _)) in self.roots.iter().enumerate() {
            let mut stack = vec![root.clone()];
            while let Some(path) = stack.pop() {
                let is_dir = path.is_dir();
                if is_dir && path != *root && path.is_symlink() {
                    continue;
                }
                if !path.exists() && !path.is_symlink() {
                    continue;
                }
                if bundle(&path) {
                    installed.push(Installed {
                        depth: path.strip_prefix(root).map_or(0, |p| parts(p).len()),
                        bundle: path.clone(),
                        root: index,
                    });
                }
                if is_dir && let Ok(entries) = std::fs::read_dir(&path) {
                    stack.extend(entries.filter_map(Result::ok).map(|e| e.path()));
                }
            }
        }
        installed.sort_by(|a, b| {
            a.depth.cmp(&b.depth).then_with(|| {
                let a = String::from(a.bundle.to_string_lossy().as_ref());
                let b = String::from(b.bundle.to_string_lossy().as_ref());
                a.cmp(&b)
            })
        });
        self.installed = Some(installed);
    }
    fn find(
        &mut self,
        local: &Path,
        test_root: &Path,
        architecture: &str,
        join: &impl Fn(&str, &[&str]) -> String,
    ) -> Option<(String, bool)> {
        if let Some(index) = self.roots.iter().position(|(root, _)| local.starts_with(root)) {
            return (local.is_file() && self.has(local, architecture)).then(|| {
                let (root, remote) = &self.roots[index];
                (join(remote, &parts(local.strip_prefix(root).unwrap())), true)
            });
        }
        let uuid = self
            .slices(local)
            .iter()
            .find(|s| s.architecture == Some(architecture))
            .and_then(|s| s.uuid);
        let mut enclosing: Vec<_> = local
            .ancestors()
            .skip(1)
            .take_while(|p| *p != test_root)
            .filter(|p| bundle(p))
            .collect();
        enclosing.reverse();
        if enclosing.is_empty() {
            return None;
        }
        self.index();
        for outer in enclosing {
            let candidates: Vec<_> = self
                .installed
                .as_ref()
                .into_iter()
                .flatten()
                .filter(|c| c.bundle.file_name() == outer.file_name())
                .map(|c| (c.bundle.clone(), c.root))
                .collect();
            for (candidate, root) in candidates {
                let candidate = candidate.join(local.strip_prefix(outer).ok()?);
                if candidate.is_file()
                    && self.has(&candidate, architecture)
                    && (uuid.is_none()
                        || self
                            .slices(&candidate)
                            .iter()
                            .find(|s| s.architecture == Some(architecture))
                            .and_then(|s| s.uuid)
                            == uuid)
                {
                    let (local_root, remote) = &self.roots[root];
                    return Some((join(remote, &parts(candidate.strip_prefix(local_root).ok()?)), false));
                }
            }
        }
        None
    }
}
/// Blocking local file inspection; call through VendorRuntime's blocking pool at generation time.
pub fn to_remote(
    xctestrun: &Path,
    roots: &[(PathBuf, String)],
    static_directory: &str,
    join: impl Fn(&str, &[&str]) -> String,
) -> VendorResult<RemoteCoverage> {
    let root = plist::dict_from_path(xctestrun).map_err(|e| {
        configuration(format!(
            "Unable to read code coverage targets from {}: {e}",
            xctestrun.display()
        ))
    })?;
    let metadata = match root.get_cloned("__xctestrun_metadata__") {
        Some(PlistValue::Dict(d)) => Some(d),
        _ => None,
    };
    let infos = match root
        .get_cloned("CodeCoverageBuildableInfos")
        .or_else(|| metadata.and_then(|d| d.get_cloned("CodeCoverageBuildableInfos")))
    {
        Some(PlistValue::Array(infos)) => infos,
        _ => {
            return Err(configuration(format!(
                "{} has no CodeCoverageBuildableInfos: build for testing with -enableCodeCoverage YES",
                xctestrun.display()
            )));
        }
    };
    let test_root = test_root(xctestrun)?;
    let mut bundles = BundleIndex {
        roots: roots
            .iter()
            .map(|(p, r)| Ok((normalize(&absolute(p)?), r.clone())))
            .collect::<VendorResult<_>>()?,
        installed: None,
        slices: HashMap::new(),
    };
    let mut remote = RemoteCoverage {
        infos: vec![],
        static_libraries: vec![],
    };
    for info in infos {
        let PlistValue::Dict(info) = info else {
            continue;
        };
        let paths = plural(&info, "ProductPaths", "ProductPath");
        let mut architectures = plural(&info, "Architectures", "Architecture");
        if architectures.is_empty() {
            architectures = vec!["arm64".into(); paths.len()];
        }
        let mut static_products = vec![];
        let mut linked_products = vec![];
        for (path, architecture) in paths.into_iter().zip(architectures) {
            let local = normalize(&if let Some(relative) = path.strip_prefix("__TESTROOT__") {
                test_root.join(relative.trim_start_matches('/'))
            } else if Path::new(&path).is_absolute() {
                PathBuf::from(path)
            } else {
                test_root.join(path)
            });
            if boolean(&info, "IsStatic") == Some(true) || mach_o::is_static(&local) {
                if local.is_file() {
                    let (namespace, path) = local
                        .strip_prefix(&test_root)
                        .map_or(("absolute", local.as_path()), |p| ("testroot", p));
                    let mut components = vec![namespace];
                    components.extend(parts(path));
                    let destination = join(static_directory, &components);
                    remote.static_libraries.push((local, destination.clone()));
                    static_products.push((destination, architecture));
                }
            } else if let Some((path, moved)) = bundles.find(&local, &test_root, &architecture, &join) {
                linked_products.push((path, architecture, moved));
            }
        }
        linked_products.sort_by_key(|(_, _, moved)| !moved);
        static_products.extend(linked_products.into_iter().map(|(p, a, _)| (p, a)));
        let mut seen = HashSet::new();
        static_products.retain(|(p, _)| seen.insert(p.clone()));
        if static_products.is_empty() {
            continue;
        }
        let Some(name) = string(&info, "Name") else {
            continue;
        };
        let (product_paths, architectures) = static_products.into_iter().unzip();
        remote.infos.push(CodeCoverageBuildableInfo::with_params(
            CodeCoverageBuildableInfoParams {
                name,
                buildable_identifier: string(&info, "BuildableIdentifier").unwrap_or_default(),
                include_in_report: boolean(&info, "IncludeInReport").unwrap_or(true),
                is_static: boolean(&info, "IsStatic").unwrap_or(false),
                product_paths,
                architectures,
                source_files: strings(&info, "SourceFiles"),
                source_files_common_path_prefix: string(&info, "SourceFilesCommonPathPrefix").unwrap_or_default(),
                toolchains: strings(&info, "Toolchains"),
            },
        ));
    }
    let mut seen = HashSet::new();
    remote.static_libraries.retain(|(_, p)| seen.insert(p.clone()));
    Ok(remote)
}

#[cfg(test)]
#[test]
fn bundle_extension_matches_release_for_leading_dots_and_case() {
    for line in include_str!("../../../../fixtures/oracle/coverage-c95b2128/extensions.jsonl").lines() {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(
            bundle(Path::new(row["path"].as_str().unwrap())),
            row["bundle"].as_bool().unwrap()
        );
    }
}

#[cfg(test)]
#[test]
fn test_root_takes_parent_before_normalizing_trailing_parent_component() {
    assert_eq!(
        test_root(Path::new("/build/products/..")).unwrap(),
        Path::new("/build/products")
    );
}

#[cfg(all(test, unix))]
#[test]
fn bundle_index_includes_dangling_bundle_symlinks() {
    let directory = tempfile::tempdir().unwrap();
    let link = directory.path().join("Missing.framework");
    std::os::unix::fs::symlink("missing", &link).unwrap();
    let mut index = BundleIndex {
        roots: vec![(directory.path().to_owned(), "/remote".into())],
        installed: None,
        slices: HashMap::new(),
    };
    index.index();
    let installed = index.installed.unwrap();
    assert_eq!(installed.len(), 1);
    assert_eq!(installed[0].bundle, link);
    assert_eq!(installed[0].depth, 1);
}
