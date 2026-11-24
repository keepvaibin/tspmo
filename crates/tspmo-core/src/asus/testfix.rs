//! Test support: a writable copy of the GU605MI fixture (`fixtures/asus-gu605mi`).

use std::path::{Path, PathBuf};
use tspmo_sys::SysRoot;

pub const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/asus-gu605mi");

pub struct Fix(PathBuf);

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let ty = e.file_type().unwrap();
        if ty.is_dir() {
            copy_tree(&src, &dst);
        } else if ty.is_file() {
            // `write`, not `copy`: the fixture's files are read-only, the copies must not be.
            std::fs::write(dst, std::fs::read(src).unwrap()).unwrap();
        }
    }
}

impl Fix {
    pub fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "tspmo-asus-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        copy_tree(Path::new(FIXTURE), &dir);
        Self(dir)
    }

    pub fn dir(&self) -> &Path {
        &self.0
    }

    pub fn root(&self) -> SysRoot {
        SysRoot::new(&self.0)
    }

    pub fn set(&self, logical: &str, value: &str) -> &Self {
        let p = self.0.join(logical.trim_start_matches('/'));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, value).unwrap();
        self
    }

    pub fn get(&self, logical: &str) -> String {
        std::fs::read_to_string(self.0.join(logical.trim_start_matches('/')))
            .unwrap()
            .trim()
            .to_owned()
    }
}

impl Drop for Fix {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
