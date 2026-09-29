//! `rpg cross-modules`: the loadable modules of one build run in the server of another.
//!
//! A module and the server that loads it are compiled separately and meet only through the
//! calling convention and the layout of the structs they share, so this is the real ABI test
//! between two compilers. Every shared library under `contrib`, `src/pl` and `src/test/modules`
//! of the modules build is copied over the one with the same path in the server build, which
//! keeps a copy of its own first, and the `contrib` and `modules` suites run there under
//! autoconf, which installs whatever library is in the build tree without relinking it, since the
//! copy is newer than the objects. The server build's own libraries are put back afterwards,
//! whether the suites passed or not. Records carry the suite as `cross-contrib` and
//! `cross-modules`, so they never read as a run of the server build's own modules.

use crate::build::{BuildInfo, Phase};
use crate::settings::System;
use crate::suite::{SuitePlan, SuiteRun, run};
use std::path::{Path, PathBuf};

/// Where the modules live, relative to the build tree.
const MODULE_DIRS: &[&str] = &["contrib", "src/pl", "src/test/modules"];

/// What a cross run needs.
pub struct CrossPlan {
    /// The build whose server runs the suites.
    pub server: PathBuf,
    /// The build whose modules are loaded into it.
    pub modules: PathBuf,
    /// `PG_TEST_TIMEOUT_DEFAULT`.
    pub timeout: u32,
}

/// Every shared library under the module directories of a build tree, relative to it.
fn libraries(build_dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for dir in MODULE_DIRS {
        collect(&build_dir.join(dir), build_dir, &mut found);
    }
    found.sort();
    found
}

fn collect(dir: &Path, root: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            // tmp_check holds an installation of its own, which is not what the build made.
            if path.file_name().is_some_and(|n| n == "tmp_check") {
                continue;
            }
            collect(&path, root, found);
        } else if path.extension().is_some_and(|e| e == "so")
            && let Ok(relative) = path.strip_prefix(root)
        {
            found.push(relative.to_path_buf());
        }
    }
}

/// The libraries both builds have, which are the ones that can be swapped.
fn shared(server: &[PathBuf], modules: &[PathBuf]) -> Vec<PathBuf> {
    server
        .iter()
        .filter(|p| modules.contains(p))
        .cloned()
        .collect()
}

/// Check that the two builds are ones whose modules can be exchanged.
fn check(server: &BuildInfo, modules: &BuildInfo) -> Result<(), String> {
    if server.phase != Phase::Built || modules.phase != Phase::Built {
        return Err("both builds have to have finished".to_string());
    }
    if System::parse(&server.system)? != System::Autoconf {
        return Err("rpg cross-modules runs the server build under autoconf only so far".into());
    }
    for (what, a, b) in [
        ("pin", &server.pin, &modules.pin),
        ("commit", &server.commit, &modules.commit),
        ("config", &server.config, &modules.config),
    ] {
        if a != b {
            return Err(format!(
                "the two builds differ in their {what}, {a} and {b}, so their modules cannot be exchanged"
            ));
        }
    }
    Ok(())
}

/// Put the server build's own libraries back.
fn restore(build_dir: &Path, saved: &Path, swapped: &[PathBuf]) {
    for library in swapped {
        std::fs::copy(saved.join(library), build_dir.join(library)).ok();
    }
}

/// Swap the libraries in, run `contrib` and `modules`, and swap them back.
pub fn cross(plan: &CrossPlan) -> Result<Vec<SuiteRun>, String> {
    let server = BuildInfo::load(&plan.server)?;
    let modules = BuildInfo::load(&plan.modules)?;
    check(&server, &modules)?;
    let server_dir = PathBuf::from(&server.build_dir);
    let modules_dir = PathBuf::from(&modules.build_dir);
    let swapped = shared(&libraries(&server_dir), &libraries(&modules_dir));
    if swapped.is_empty() {
        return Err("the two builds have no module library in common".to_string());
    }
    let saved = plan.server.join("own-modules");
    for library in &swapped {
        let keep = saved.join(library);
        if let Some(parent) = keep.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        std::fs::copy(server_dir.join(library), &keep)
            .map_err(|e| format!("keeping {}: {e}", library.display()))?;
    }
    eprintln!(
        "rpg: loading {} libraries built by {} into the server built by {}",
        swapped.len(),
        modules.compiler,
        server.compiler
    );
    let mut runs = Vec::new();
    let mut failure = None;
    for library in &swapped {
        if let Err(e) = std::fs::copy(modules_dir.join(library), server_dir.join(library)) {
            failure = Some(format!("copying {}: {e}", library.display()));
            break;
        }
    }
    if failure.is_none() {
        for suite in ["contrib", "modules"] {
            let suite_plan = SuitePlan {
                out: plan.server.clone(),
                info: server.clone(),
                suite: suite.to_string(),
                row: None,
                timeout: plan.timeout,
                baseline: None,
                run: 1,
                label: Some(format!("cross-{suite}")),
            };
            match run(&suite_plan) {
                Ok(done) => runs.push(done),
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
        }
    }
    restore(&server_dir, &saved, &swapped);
    match failure {
        Some(e) => Err(e),
        None => Ok(runs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_libraries_both_builds_have_are_swapped() {
        let server = vec![
            PathBuf::from("contrib/amcheck/amcheck.so"),
            PathBuf::from("src/pl/plpgsql/src/plpgsql.so"),
        ];
        let modules = vec![
            PathBuf::from("contrib/amcheck/amcheck.so"),
            PathBuf::from("contrib/sepgsql/sepgsql.so"),
        ];
        assert_eq!(
            shared(&server, &modules),
            [PathBuf::from("contrib/amcheck/amcheck.so")]
        );
    }

    #[test]
    fn libraries_are_found_below_the_module_directories_and_not_in_tmp_check() {
        let root = std::env::temp_dir().join(format!("rpg-cross-{}", std::process::id()));
        for file in [
            "contrib/amcheck/amcheck.so",
            "contrib/amcheck/amcheck.o",
            "contrib/amcheck/tmp_check/install/lib/amcheck.so",
            "src/test/modules/test_ddl/test_ddl.so",
            "src/backend/postgres.so",
        ] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"").unwrap();
        }
        let found = libraries(&root);
        std::fs::remove_dir_all(&root).ok();
        assert_eq!(
            found,
            [
                PathBuf::from("contrib/amcheck/amcheck.so"),
                PathBuf::from("src/test/modules/test_ddl/test_ddl.so"),
            ]
        );
    }
}
