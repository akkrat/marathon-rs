//! The in-memory filesystem of a fake device, served by the sync service.

use std::collections::BTreeMap;

pub(crate) const S_IFDIR: u32 = 0o040000;
pub(crate) const S_IFREG: u32 = 0o100000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// `None` for a directory.
    pub content: Option<Vec<u8>>,
    /// Permission bits only; [`Node::st_mode`] adds the file type.
    pub mode: u32,
    pub mtime: u32,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.content.is_none()
    }

    pub fn size(&self) -> u64 {
        self.content.as_ref().map_or(4096, |c| c.len() as u64)
    }

    pub fn st_mode(&self) -> u32 {
        (if self.is_dir() { S_IFDIR } else { S_IFREG }) | (self.mode & 0o7777)
    }
}

/// Absolute, normalised paths to nodes. Parent directories always exist.
#[derive(Debug, Clone)]
pub struct DeviceFs {
    nodes: BTreeMap<String, Node>,
}

impl Default for DeviceFs {
    fn default() -> Self {
        let mut fs = DeviceFs { nodes: BTreeMap::new() };
        fs.nodes.insert("/".to_string(), dir_node(0));
        for d in ["/data/local/tmp", "/sdcard", "/system/bin"] {
            // Cannot fail: the tree holds only directories so far.
            let _ = fs.mkdirs(d, 0);
        }
        fs
    }
}

fn dir_node(mtime: u32) -> Node {
    Node {
        content: None,
        mode: 0o755,
        mtime,
    }
}

/// Resolves `.`, `..` and repeated slashes. Relative paths are taken from `/`, as adbd does for sync.
pub fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for p in path.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("/{}", parts.join("/"))
}

fn parent(path: &str) -> Option<&str> {
    if path == "/" {
        return None;
    }
    match path.rfind('/') {
        Some(0) => Some("/"),
        Some(i) => Some(&path[..i]),
        None => None,
    }
}

impl DeviceFs {
    pub(crate) fn entries(&self) -> impl Iterator<Item = (&str, &Node)> {
        self.nodes.iter().map(|(name, node)| (name.as_str(), node))
    }

    pub fn get(&self, path: &str) -> Option<&Node> {
        self.nodes.get(&normalize(path))
    }

    /// Like `mkdir -p`; existing directories are left alone. Fails if a component is a file.
    pub fn mkdirs(&mut self, path: &str, mtime: u32) -> Result<(), String> {
        let path = normalize(path);
        let mut current = String::new();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            current.push('/');
            current.push_str(part);
            match self.nodes.get(&current) {
                Some(n) if n.is_dir() => {}
                Some(_) => return Err(format!("{current}: Not a directory")),
                None => {
                    self.nodes.insert(current.clone(), dir_node(mtime));
                }
            }
        }
        Ok(())
    }

    /// A scripted directory: like `mkdirs`, then the directory itself gets `mode` and `mtime` (the parents
    /// it creates stay 0755).
    pub fn mkdir(&mut self, path: &str, mode: u32, mtime: u32) -> Result<(), String> {
        self.mkdirs(path, mtime)?;
        if let Some(n) = self.nodes.get_mut(&normalize(path)) {
            n.mode = mode & 0o7777;
            n.mtime = mtime;
        }
        Ok(())
    }

    /// Creates or replaces a file, creating its parent directories like adbd's `secure_mkdirs`.
    pub fn write(&mut self, path: &str, content: Vec<u8>, mode: u32, mtime: u32) -> Result<(), String> {
        let path = normalize(path);
        if let Some(n) = self.nodes.get(&path)
            && n.is_dir()
        {
            return Err(format!("{path}: Is a directory"));
        }
        if let Some(p) = parent(&path) {
            self.mkdirs(p, mtime)?;
        }
        self.nodes.insert(
            path,
            Node {
                content: Some(content),
                mode: mode & 0o7777,
                mtime,
            },
        );
        Ok(())
    }

    /// The direct children of a directory, in name order; empty if `path` isn't a directory.
    pub fn list(&self, path: &str) -> Vec<(String, &Node)> {
        let path = normalize(path);
        if !self.nodes.get(&path).is_some_and(Node::is_dir) {
            return Vec::new();
        }
        let prefix = if path == "/" {
            "/".to_string()
        } else {
            format!("{path}/")
        };
        self.nodes
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .filter(|(k, _)| !k[prefix.len()..].contains('/') && k.len() > prefix.len())
            .map(|(k, n)| (k[prefix.len()..].to_string(), n))
            .collect()
    }

    pub fn paths(&self) -> impl Iterator<Item = (&str, &Node)> {
        self.nodes.iter().map(|(name, node)| (name.as_str(), node))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_paths() {
        assert_eq!(normalize("/a//b/./c/../d/"), "/a/b/d");
        assert_eq!(normalize("relative"), "/relative");
        assert_eq!(normalize("/.."), "/");
    }

    #[test]
    fn write_creates_parents_and_lists() {
        let mut fs = DeviceFs::default();
        fs.write("/sdcard/x/y/f.txt", b"abc".to_vec(), 0o100644, 5).unwrap();
        assert!(fs.get("/sdcard/x/y").unwrap().is_dir());
        let f = fs.get("/sdcard/x/y/f.txt").unwrap();
        assert_eq!((f.size(), f.st_mode(), f.mtime), (3, 0o100644, 5));
        let names: Vec<String> = fs.list("/sdcard/x").into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["y"]);
        let root: Vec<String> = fs.list("/").into_iter().map(|(n, _)| n).collect();
        assert_eq!(root, ["data", "sdcard", "system"]);
        assert!(fs.list("/sdcard/x/y/f.txt").is_empty());
        assert!(fs.write("/sdcard/x", vec![], 0o644, 0).is_err());
        fs.mkdir("/sdcard/private/inner", 0o700, 9).unwrap();
        let d = fs.get("/sdcard/private/inner").unwrap();
        assert_eq!((d.st_mode(), d.mtime), (0o040700, 9));
        assert_eq!(fs.get("/sdcard/private").unwrap().st_mode(), 0o040755);
        assert!(fs.write("/sdcard/x/y/f.txt/z", vec![], 0o644, 0).is_err());
    }
}
